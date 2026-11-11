use super::*;
use mindplayer_core::catalog::{self, UsageSnapshot};
use working_set::{source_key, SourceKey};

pub(super) struct PendingUsage {
    source: SourceKey,
    epoch: u64,
    valid: bool,
    result: Receiver<Result<UsageSnapshot, String>>,
}

pub(super) fn restore(values: &HashMap<SourceKey, UsageSnapshot>, session: &mut Session) {
    if let Some(value) = (!values.is_empty())
        .then(|| values.get(&source_key(session)))
        .flatten()
    {
        session.tokens = value.tokens;
        // Kiro context is live bounded metadata, not a frozen token snapshot.
        if session.agent != Agent::Kiro {
            session.context_pct = value.context_pct;
        }
    } else {
        session.tokens = TokenUsage::default();
        if session.agent != Agent::Kiro {
            session.context_pct = None;
        }
    }
}

impl App {
    pub(super) fn restore_manual_usage(&self, session: &mut Session) {
        restore(&self.manual_usage, session);
    }

    pub fn measured_usage(&self, session: &Session) -> Option<&UsageSnapshot> {
        if self.manual_usage.is_empty() {
            return None;
        }
        self.manual_usage.get(&source_key(session))
    }

    pub(super) fn invalidate_manual_usage(&mut self) {
        // Keep the receiver until the worker exits: cancellation must not permit
        // overlapping parsers, even when the old result is no longer usable.
        if let Some(pending) = &mut self.manual_usage_pending {
            pending.valid = false;
        }
    }

    pub fn measure_selected_usage(&mut self) {
        if self.multi_select || self.selected_session().is_none() {
            self.status = "select one session to measure tokens (r)".into();
            return;
        }
        if self.manual_usage_pending.is_some() {
            self.status = "manual token measurement already in progress".into();
            return;
        }
        let session = self
            .selected_session()
            .expect("selected session checked")
            .clone();
        let source = source_key(&session);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = catalog::measure_usage(&session).map_err(|error| format!("{error:#}"));
            let _ = tx.send(result);
        });
        self.manual_usage_pending = Some(PendingUsage {
            source,
            epoch: self.working_set.epoch,
            valid: true,
            result: rx,
        });
        self.status = "measuring selected session tokens manually".into();
    }

    pub fn poll_manual_usage(&mut self) -> bool {
        let Some(pending) = &self.manual_usage_pending else {
            return false;
        };
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Err("measurement worker stopped".into()),
        };
        let pending = self
            .manual_usage_pending
            .take()
            .expect("pending receiver checked");
        if !pending.valid
            || pending.epoch != self.working_set.epoch
            || !self
                .known_metadata()
                .any(|s| source_key(s) == pending.source)
        {
            self.status = "manual token measurement discarded: session source changed".into();
            return true;
        }
        match result {
            Ok(value) => {
                self.status = if value.partial {
                    "manual tokens measured · partial (bounded source read)"
                } else {
                    "manual tokens measured · press r to measure again"
                }
                .into();
                self.manual_usage.insert(pending.source, value);
                self.rebuild_visible();
            }
            Err(error) => {
                self.status = format!("manual token measurement failed: {error}");
            }
        }
        true
    }

    /// Measured rows only; unmeasured rows never become zero-valued samples.
    pub fn manual_usage_summary<'a>(&self, sessions: impl Iterator<Item = &'a Session>) -> String {
        if self.manual_usage.is_empty() {
            return "— tok · manual, none measured".into();
        }
        let mut total = 0u64;
        let mut measured = 0usize;
        let mut count = 0usize;
        let mut partial = false;
        let mut estimated = false;
        for session in sessions {
            count += 1;
            // Kiro exposes context occupancy, not a measured token total.
            if session.agent == Agent::Kiro {
                continue;
            }
            if let Some(value) = self.measured_usage(session) {
                total = total.saturating_add(value.tokens.total);
                measured += 1;
                partial |= value.partial;
                estimated |= session.agent == Agent::Cursor;
            }
        }
        if measured == 0 {
            return "— tok · manual, none measured".into();
        }
        format!(
            "{}{} tok · manual {measured}/{count}{}",
            if estimated { "~" } else { "" },
            human_tokens(total),
            if partial || measured < count {
                " · partial"
            } else {
                ""
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_source() -> (App, Session) {
        let mut app = crate::app::tests::isolated_app();
        let mut source = crate::app::tests::session("manual", Agent::Codex, false);
        source.file = PathBuf::from("/fixture/manual.jsonl");
        source.cwd = PathBuf::from("/fixture");
        app.all_sessions.push(source.clone());
        app.rebuild_visible();
        (app, source)
    }

    fn value(total: u64) -> UsageSnapshot {
        UsageSnapshot {
            tokens: TokenUsage {
                total,
                ..Default::default()
            },
            context_pct: Some(12.0),
            partial: false,
        }
    }

    fn pending(app: &mut App, source: &Session) -> mpsc::Sender<Result<UsageSnapshot, String>> {
        assert!(app.manual_usage_pending.is_none());
        let (tx, rx) = mpsc::channel();
        app.manual_usage_pending = Some(PendingUsage {
            source: source_key(source),
            epoch: app.working_set.epoch,
            valid: true,
            result: rx,
        });
        tx
    }

    fn finish(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.manual_usage_pending.is_some() {
            app.poll_manual_usage();
            assert!(Instant::now() < deadline, "manual worker did not finish");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn completion_changes_only_usage_and_survives_catalog_refresh() {
        let (mut app, source) = app_with_source();
        let tx = pending(&mut app, &source);
        app.all_sessions[0].title = "new title".into();
        app.all_sessions[0].last_prompt_at = Some(Utc::now());
        let prompt = app.all_sessions[0].last_prompt_at;
        tx.send(Ok(value(123))).unwrap();
        assert!(app.poll_manual_usage());
        assert_eq!(app.all_sessions[0].title, "new title");
        assert_eq!(app.all_sessions[0].last_prompt_at, prompt);
        assert_eq!(app.all_sessions[0].tokens.total, 123);
        app.apply_catalog((
            app.working_set.epoch,
            Instant::now(),
            Ok(catalog::CatalogSnapshot {
                active: vec![source.clone()],
                ..Default::default()
            }),
        ));
        assert_eq!(app.all_sessions[0].tokens.total, 123);
        assert_eq!(app.measured_usage(&source).unwrap().tokens.total, 123);
        assert!(app.summary_head().contains("manual 1/1"));
        let mut restarted = crate::app::tests::isolated_app();
        restarted.all_sessions = app.all_sessions.clone();
        restarted.rebuild_visible();
        assert!(restarted.measured_usage(&source).is_none());
        assert!(restarted.summary_head().contains("— tok"));
    }

    #[test]
    fn repeated_requests_errors_and_disconnect_leave_one_worker_and_keep_success() {
        let (mut app, source) = app_with_source();
        app.manual_usage.insert(source_key(&source), value(17));
        let tx = pending(&mut app, &source);
        app.measure_selected_usage();
        assert!(app.status.contains("already in progress"));
        tx.send(Err("unreadable source".into())).unwrap();
        assert!(app.poll_manual_usage());
        assert!(app.manual_usage_pending.is_none());
        assert_eq!(app.measured_usage(&source).unwrap().tokens.total, 17);
        let tx = pending(&mut app, &source);
        drop(tx);
        assert!(app.poll_manual_usage());
        assert!(app.status.contains("worker stopped"));
        assert!(app.manual_usage_pending.is_none());
        let tx = pending(&mut app, &source);
        tx.send(Ok(value(0))).unwrap();
        app.poll_manual_usage();
        assert_eq!(app.measured_usage(&source).unwrap().tokens.total, 0);
        assert!(app.summary_head().contains("0 tok"));
    }

    #[test]
    fn stale_results_do_not_revive_closed_or_changed_sources() {
        for change in 0..5 {
            let (mut app, source) = app_with_source();
            let tx = pending(&mut app, &source);
            match change {
                0 => app.all_sessions.clear(),
                1 => app.all_sessions[0].file = PathBuf::from("/other/manual.jsonl"),
                2 => app.all_sessions[0].agent = Agent::Claude,
                3 => app.all_sessions[0].cwd = PathBuf::from("/other"),
                _ => app.working_set.epoch = app.working_set.epoch.wrapping_add(1),
            }
            let count = app.all_sessions.len();
            tx.send(Ok(value(55))).unwrap();
            assert!(app.poll_manual_usage());
            assert!(app.manual_usage.is_empty(), "change {change}");
            assert_eq!(app.all_sessions.len(), count, "no resurrection");
            assert!(app.manual_usage_pending.is_none());
        }
        let (mut app, source) = app_with_source();
        let tx = pending(&mut app, &source);
        app.cancel_catalog_work();
        app.measure_selected_usage();
        assert!(app.status.contains("already in progress"));
        tx.send(Ok(value(55))).unwrap();
        app.poll_manual_usage();
        assert!(app.manual_usage.is_empty());
        let (mut app, source) = app_with_source();
        let tx = pending(&mut app, &source);
        app.remove_pane(&source.id);
        tx.send(Ok(value(55))).unwrap();
        app.poll_manual_usage();
        assert!(app.manual_usage.is_empty());
    }

    #[test]
    fn selection_guards_do_not_start_workers() {
        let (mut app, _) = app_with_source();
        app.multi_select = true;
        app.measure_selected_usage();
        assert!(app.manual_usage_pending.is_none());
        app.multi_select = false;
        app.visible = vec![Row::Header(None)];
        app.measure_selected_usage();
        assert!(app.manual_usage_pending.is_none());
        app.multi_select = false;
        app.visible.clear();
        app.measure_selected_usage();
        assert!(app.manual_usage_pending.is_none());
        assert!(app.status.contains("select one session"));
    }

    #[test]
    fn archive_paging_preserves_measurement_without_launch_or_activation() {
        let (mut app, mut source) = app_with_source();
        source.archived = true;
        app.all_sessions.clear();
        app.archive_catalog = Arc::new(vec![source.clone()]);
        app.show_archived = true;
        app.all_sessions.push(source.clone());
        app.rebuild_visible();
        let tx = pending(&mut app, &source);
        app.request_archive_page();
        assert!(app.all_sessions.is_empty());
        tx.send(Ok(value(71))).unwrap();
        app.poll_manual_usage();
        assert_eq!(app.measured_usage(&source).unwrap().tokens.total, 71);
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.all_sessions.is_empty() {
            app.poll_working_set();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.all_sessions[0].tokens.total, 71);
        assert!(app.all_sessions[0].archived);
        assert!(app.pending.is_none());
        assert!(app.ptys.is_empty());
        assert!(app.panes.is_empty());
    }

    #[test]
    fn activity_update_cannot_apply_to_same_id_in_another_source() {
        let (mut app, source) = app_with_source();
        let mut wrong = source.clone();
        wrong.file = PathBuf::from("/other/manual.jsonl");
        let (tx, rx) = mpsc::channel();
        tx.send(vec![ActivityUpdate {
            source: source_key(&wrong),
            last_active: Some(Utc::now()),
            last_prompt_at: Some(Utc::now()),
            context_pct: Some(99.0),
        }])
        .unwrap();
        app.refresh_rx = Some(rx);
        assert!(!app.poll_refresh());
        assert_eq!(app.all_sessions[0].last_active, source.last_active);
        assert_eq!(app.all_sessions[0].last_prompt_at, source.last_prompt_at);
    }

    #[test]
    fn manual_worker_reports_the_underlying_identity_error() {
        let (mut app, mut source) = app_with_source();
        source.cwd = crate::app::tests::fresh_login_test_home();
        source.file = source.cwd.join("wrong-identity.jsonl");
        let header = serde_json::json!({"type":"session_meta","payload":{"id":"replacement","cwd":source.cwd}});
        std::fs::write(&source.file, format!("{header}\n")).unwrap();
        app.all_sessions = vec![source];
        app.rebuild_visible();
        app.measure_selected_usage();
        finish(&mut app);
        assert!(
            app.status.contains("validating session source"),
            "{}",
            app.status
        );
        assert!(
            app.status
                .contains("native session identity is missing or changed"),
            "{}",
            app.status
        );
        assert!(app.manual_usage.is_empty());
    }

    #[test]
    fn catalog_preserves_prompt_observed_before_it_leaves_metadata_windows() {
        use std::io::Write;

        let (mut app, mut source) = app_with_source();
        source.cwd = crate::app::tests::fresh_login_test_home();
        source.file = source.cwd.join("displaced-prompt.jsonl");
        let prompt_at = Utc::now() - chrono::Duration::minutes(1);
        let header =
            serde_json::json!({"type":"session_meta","payload":{"id":source.id,"cwd":source.cwd}});
        let padding = serde_json::json!({"type":"response_item","payload":{
            "type":"message","role":"assistant","content":[{"type":"output_text","text":"x".repeat(300 * 1024)}]
        }});
        let prompt = serde_json::json!({"type":"response_item","timestamp":prompt_at,"payload":{
            "type":"message","role":"user","content":[{"type":"input_text","text":"new human prompt"}]
        }});
        std::fs::write(&source.file, format!("{header}\n{padding}\n{prompt}\n")).unwrap();
        app.all_sessions = vec![source.clone()];
        app.rebuild_visible();
        app.start_refresh();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.refresh_rx.is_some() {
            app.poll_refresh();
            assert!(Instant::now() < deadline, "activity worker did not finish");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.all_sessions[0].last_prompt_at, Some(prompt_at));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&source.file)
            .unwrap();
        writeln!(file, "{padding}").unwrap();
        drop(file);
        let metadata = catalog::hydrate(&source).unwrap();
        assert_eq!(
            metadata.last_prompt_at, None,
            "prompt is outside both windows"
        );
        assert!(app.apply_catalog((
            app.working_set.epoch,
            Instant::now(),
            Ok(catalog::CatalogSnapshot {
                active: vec![metadata],
                ..Default::default()
            }),
        )));
        assert_eq!(app.all_sessions[0].last_prompt_at, Some(prompt_at));
        assert!(app.manual_usage.is_empty());
    }

    #[test]
    fn aggregates_label_unmeasured_and_bounded_rows_as_partial() {
        let (mut app, source) = app_with_source();
        assert!(app.summary_head().contains("— tok"));
        app.manual_usage.insert(source_key(&source), value(0));
        let mut other = source.clone();
        other.id = "other".into();
        app.all_sessions.push(other);
        app.rebuild_visible();
        assert!(app.summary_head().contains("0 tok · manual 1/2 · partial"));
        app.all_sessions.pop();
        app.manual_usage
            .get_mut(&source_key(&source))
            .unwrap()
            .partial = true;
        app.rebuild_visible();
        assert!(app.summary_head().contains("manual 1/1 · partial"));
    }

    #[test]
    fn aggregates_mark_estimates_only_when_cursor_usage_is_included() {
        let (mut app, source) = app_with_source();
        let mut cursor = source.clone();
        cursor.agent = Agent::Cursor;
        cursor.id = "cursor".into();
        cursor.file = PathBuf::from("/fixture/cursor.json");
        app.manual_usage.insert(source_key(&source), value(600));
        app.manual_usage.insert(source_key(&cursor), value(400));

        assert!(app
            .manual_usage_summary(std::iter::once(&cursor))
            .starts_with("~400 tok"));
        assert!(app
            .manual_usage_summary([&source, &cursor].into_iter())
            .starts_with("~1.0K tok"));
        assert!(!app
            .manual_usage_summary(std::iter::once(&source))
            .starts_with('~'));
        app.manual_usage.remove(&source_key(&cursor));
        assert!(!app
            .manual_usage_summary([&source, &cursor].into_iter())
            .starts_with('~'));
    }

    #[test]
    fn explicit_worker_reparses_same_mtime_and_never_measures_other_rows() {
        let root = std::env::temp_dir().join(format!(
            "mp-manual-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let (mut app, mut source) = app_with_source();
        source.file = root.join("manual.jsonl");
        source.cwd = root.clone();
        let header =
            serde_json::json!({"type":"session_meta","payload":{"id":source.id,"cwd":root}});
        let write_usage = |total| {
            let usage = serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":total,"total_tokens":total}}}});
            std::fs::write(&source.file, format!("{header}\n{usage}\n")).unwrap();
        };
        write_usage(10);
        let mtime = std::fs::metadata(&source.file).unwrap().modified().unwrap();
        app.all_sessions = vec![
            source.clone(),
            crate::app::tests::session("unreadable", Agent::Claude, false),
        ];
        app.rebuild_visible();
        app.selected = app.row_of_session_source(&source).unwrap();
        app.measure_selected_usage();
        app.measure_selected_usage();
        finish(&mut app);
        assert_eq!(app.measured_usage(&source).unwrap().tokens.total, 10);
        assert_eq!(app.manual_usage.len(), 1);
        write_usage(20);
        std::fs::File::options()
            .write(true)
            .open(&source.file)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        app.start_refresh();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.refresh_rx.is_some() {
            app.poll_refresh();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        app.measure_selected_usage();
        finish(&mut app);
        assert_eq!(app.measured_usage(&source).unwrap().tokens.total, 20);
        assert!(app.pending.is_none());
        write_usage(30);
        let mut archived = source.clone();
        archived.archived = true;
        app.archive_catalog = Arc::new(vec![archived.clone()]);
        app.all_sessions = vec![archived];
        app.show_archived = true;
        app.rebuild_visible();
        app.selected = app.row_of_session_source(&source).unwrap();
        app.measure_selected_usage();
        finish(&mut app);
        assert_eq!(app.measured_usage(&source).unwrap().tokens.total, 30);
        assert!(app.all_sessions[0].archived);
        assert!(app.pending.is_none());
        assert!(app.ptys.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod metadata_context_tests {
    use super::*;

    fn overlapping_activity_catalog(catalog_newer: bool) {
        for catalog_first in [false, true] {
            let mut app = crate::app::tests::isolated_app();
            let source = crate::app::tests::session("kiro-overlap", Agent::Kiro, false);
            app.all_sessions.push(source.clone());
            app.rebuild_visible();
            let older_start = Instant::now();
            let newer_start = older_start + Duration::from_secs(1);
            let newer_time = Utc::now();
            let older_time = newer_time - chrono::Duration::minutes(1);
            let (catalog_start, activity_start) = if catalog_newer {
                (newer_start, older_start)
            } else {
                (older_start, newer_start)
            };
            let mut catalog_row = source.clone();
            catalog_row.last_active = Some(if catalog_newer {
                newer_time
            } else {
                older_time
            });
            catalog_row.last_prompt_at = catalog_row.last_active;
            catalog_row.context_pct = Some(if catalog_newer { 20.0 } else { 80.0 });
            let message = (
                app.working_set.epoch,
                catalog_start,
                Ok(catalog::CatalogSnapshot {
                    active: vec![catalog_row],
                    ..Default::default()
                }),
            );
            let (tx, rx) = mpsc::channel();
            app.refresh_rx = Some(rx);
            app.refresh_started_at = activity_start;
            tx.send(vec![ActivityUpdate {
                source: source_key(&source),
                last_active: Some(if catalog_newer {
                    older_time
                } else {
                    newer_time
                }),
                last_prompt_at: Some(if catalog_newer {
                    older_time
                } else {
                    newer_time
                }),
                context_pct: Some(if catalog_newer { 80.0 } else { 20.0 }),
            }])
            .unwrap();
            if catalog_first {
                assert!(app.apply_catalog(message));
                app.poll_refresh();
            } else {
                assert!(app.poll_refresh());
                assert!(app.apply_catalog(message));
            }
            assert_eq!(
                app.all_sessions[0].context_pct,
                Some(20.0),
                "catalog first: {catalog_first}"
            );
            assert_eq!(app.all_sessions[0].last_active, Some(newer_time));
            assert_eq!(app.all_sessions[0].last_prompt_at, Some(newer_time));
            assert!(app.refresh_rx.is_none());
            assert!(app.manual_usage.is_empty());
        }
    }

    #[test]
    fn newer_catalog_context_survives_both_activity_arrival_orders() {
        overlapping_activity_catalog(true);
    }

    #[test]
    fn newer_activity_context_survives_both_catalog_arrival_orders() {
        overlapping_activity_catalog(false);
    }

    #[test]
    fn pending_activity_is_drained_before_a_newer_lower_context_refresh() {
        let mut app = crate::app::tests::isolated_app();
        let source = crate::app::tests::session("kiro-drain", Agent::Kiro, false);
        app.all_sessions.push(source.clone());
        let older_start = Instant::now();
        let (tx, rx) = mpsc::channel();
        app.refresh_rx = Some(rx);
        app.refresh_started_at = older_start;
        let mut catalog_row = source.clone();
        catalog_row.context_pct = Some(40.0);
        assert!(app.apply_catalog((
            app.working_set.epoch,
            older_start + Duration::from_secs(1),
            Ok(catalog::CatalogSnapshot {
                active: vec![catalog_row],
                ..Default::default()
            }),
        )));
        app.start_refresh();
        assert_eq!(app.refresh_started_at, older_start, "no overlapping worker");
        assert!(app.refresh_rx.is_some());
        assert!(!app.poll_refresh(), "still waiting for the original worker");
        tx.send(vec![ActivityUpdate {
            source: source_key(&source),
            last_active: None,
            last_prompt_at: None,
            context_pct: Some(90.0),
        }])
        .unwrap();
        assert!(!app.poll_refresh(), "stale context must not cause a redraw");
        assert!(app.refresh_rx.is_none());
        assert_eq!(app.all_sessions[0].context_pct, Some(40.0));
        let (tx, rx) = mpsc::channel();
        app.refresh_rx = Some(rx);
        app.refresh_started_at = older_start + Duration::from_secs(2);
        tx.send(vec![ActivityUpdate {
            source: source_key(&source),
            last_active: None,
            last_prompt_at: None,
            context_pct: Some(10.0),
        }])
        .unwrap();
        assert!(app.poll_refresh());
        assert_eq!(app.all_sessions[0].context_pct, Some(10.0));
    }

    #[test]
    fn failed_activity_read_does_not_renew_context_observation() {
        let mut app = crate::app::tests::isolated_app();
        let mut source = crate::app::tests::session("kiro-missing", Agent::Kiro, false);
        source.file = crate::app::tests::fresh_login_test_home().join("missing-kiro.json");
        source.context_pct = Some(42.0);
        let observed_at = Instant::now();
        let key = source_key(&source);
        app.context_observations
            .insert(key.clone(), (observed_at, 42.0));
        app.all_sessions.push(source);
        app.start_refresh();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.refresh_rx.is_some() {
            app.poll_refresh();
            assert!(Instant::now() < deadline, "activity worker did not finish");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.all_sessions[0].context_pct, Some(42.0));
        assert_eq!(app.context_observations[&key], (observed_at, 42.0));
    }

    #[test]
    fn catalog_metadata_does_not_cross_source_identity() {
        for change in 0..4 {
            let mut app = crate::app::tests::isolated_app();
            let mut source = crate::app::tests::session("kiro-source", Agent::Kiro, false);
            source.last_active = Some(Utc::now());
            source.last_prompt_at = source.last_active;
            source.context_pct = Some(90.0);
            app.context_observations
                .insert(source_key(&source), (Instant::now(), 90.0));
            app.all_sessions.push(source.clone());
            match change {
                0 => source.file = PathBuf::from("/other/source.json"),
                1 => source.cwd = PathBuf::from("/other"),
                2 => source.id = "other-id".into(),
                _ => source.agent = Agent::Codex,
            }
            source.last_active = None;
            source.last_prompt_at = None;
            source.context_pct = None;
            assert!(app.apply_catalog((
                app.working_set.epoch,
                Instant::now(),
                Ok(catalog::CatalogSnapshot {
                    active: vec![source],
                    ..Default::default()
                }),
            )));
            assert_eq!(app.all_sessions[0].last_active, None, "change {change}");
            assert_eq!(app.all_sessions[0].last_prompt_at, None, "change {change}");
            assert_eq!(app.all_sessions[0].context_pct, None, "change {change}");
        }
    }

    #[test]
    fn fresh_kiro_context_is_not_replaced_by_the_manual_snapshot() {
        let mut app = crate::app::tests::isolated_app();
        let mut session = crate::app::tests::session("kiro", Agent::Kiro, false);
        session.context_pct = Some(42.0);
        let source = source_key(&session);
        app.manual_usage.insert(
            source.clone(),
            UsageSnapshot {
                tokens: TokenUsage::default(),
                context_pct: Some(12.0),
                partial: false,
            },
        );
        app.all_sessions.push(session);
        app.rebuild_visible();
        assert_eq!(app.all_sessions[0].context_pct, Some(42.0));
        let (tx, rx) = mpsc::channel();
        tx.send(vec![ActivityUpdate {
            source,
            last_active: None,
            last_prompt_at: None,
            context_pct: Some(56.0),
        }])
        .unwrap();
        app.refresh_rx = Some(rx);
        assert!(app.poll_refresh());
        assert_eq!(app.all_sessions[0].context_pct, Some(56.0));
        assert!(app.summary_head().contains("— tok"));
    }
}
