use super::*;
use chrono::Duration;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use mindplayer_core::{Aggregate, State, TokenUsage};
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct ListFixture {
    app: App,
    dir: PathBuf,
}

impl ListFixture {
    fn new(count: usize) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = loop {
            let path = std::env::temp_dir().join(format!(
                "mindplayer-list-render-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create isolated list fixture {path:?}: {error}"),
            }
        };
        let mut app = App::new_in(PathBuf::from("/synthetic/work"));
        app.state = State::default();
        app.state_path = dir.join("state.json");
        app.audit_path = dir.join("audit.jsonl");
        app.convo_dir = dir.join("convo");

        app.screen = Screen::Main;
        app.focus = Focus::List;
        app.accounts.clear();
        app.limits = Some(Vec::new());
        app.quota_cache = None;
        app.all_sessions = (0..count)
            .map(|index| Session {
                id: format!("session-{index:05}"),
                agent: Agent::Codex,
                cwd: PathBuf::from("/synthetic/work"),
                file: PathBuf::new(),
                started_at: None,
                last_active: Some(now() - Duration::days(2)),
                last_prompt_at: None,
                tokens: TokenUsage::default(),
                title: format!("row-{index:05}"),
                archived: false,
                is_subagent: false,
                context_pct: None,
            })
            .collect();
        // Sorting and archive paging have separate owners. Exercise rendering
        // directly, including histories larger than an archive page.
        app.visible = (0..count).map(Row::Session).collect();
        app.recent_count = count / 2;
        app.visible_aggregate = Aggregate::of_refs(app.visible_sessions());
        app.rebuild_list_items();
        Self { app, dir }
    }

    fn categorized() -> Self {
        let mut fixture = Self::new(7);
        let app = &mut fixture.app;
        for (session, title) in app.all_sessions.iter_mut().zip([
            "Root",
            "Lane",
            "Recent loose",
            "Older first",
            "Older last",
            "Hidden lane",
            "Folded session",
        ]) {
            session.title = title.into();
        }
        app.all_sessions[1].last_active = Some(now() - Duration::minutes(5));
        app.all_sessions[5].last_active = Some(now() - Duration::minutes(2));
        app.all_sessions[5].archived = true;
        for child in [1, 5] {
            app.state.set_handoff_link(
                &app.all_sessions[child].id,
                &app.all_sessions[0].id,
                PathBuf::new(),
                now(),
            );
        }
        let topic_a = app.state.create_category("Topic A", now()).unwrap();
        let folded = app.state.create_category("Folded", now()).unwrap();
        let topic_b = app.state.create_category("Topic B", now()).unwrap();
        app.state.assign_category(&app.all_sessions[0].id, &topic_a);
        app.state.assign_category(&app.all_sessions[6].id, &folded);
        app.state.assign_category(&app.all_sessions[3].id, &topic_b);
        app.state.set_collapsed(&folded, true);
        app.state.set_category_auto_sync(&folded, false);
        app.category_counts = [
            (Some(topic_a.clone()), 2),
            (Some(folded.clone()), 1),
            (Some(topic_b.clone()), 1),
            (None, 2),
        ]
        .into();
        app.loose_counts = [1, 1];
        app.visible = vec![
            Row::Header(Some(topic_a)),
            Row::Session(0),
            Row::Session(1),
            Row::Header(Some(folded)),
            Row::Header(None),
            Row::Session(2),
            Row::Header(Some(topic_b)),
            Row::Session(3),
            Row::Header(None),
            Row::Session(4),
        ];
        app.recent_count = 6;
        app.visible_aggregate = Aggregate::of_refs(app.visible_sessions());
        app.rebuild_list_items();
        fixture
    }

    fn start_pty(&mut self, id: &str) {
        let command = mindplayer_core::resume::Command {
            program: "cat".into(),
            args: Vec::new(),
            cwd: self.dir.clone(),
            env: Default::default(),
        };
        let pty =
            crate::pty::PtySession::spawn(&command, id, 8, 80).expect("start isolated fixture PTY");
        self.app.ptys.insert(id.into(), pty);
    }
}

impl Drop for ListFixture {
    fn drop(&mut self) {
        self.app.ptys.clear();
        std::fs::remove_dir_all(&self.dir).expect("remove this list fixture's scratch directory");
    }
}

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

fn draw_list(app: &mut App, area: Rect) -> Buffer {
    app.rebuild_list_items();
    let mut terminal =
        Terminal::new(TestBackend::new(area.right() + 2, area.bottom() + 2)).unwrap();
    terminal
        .draw(|frame| session_list(frame, app, area, now()))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn row_text(buffer: &Buffer, area: Rect, row: u16) -> String {
    (area.x + 1..area.right() - 1)
        .map(|x| buffer[(x, area.y + 1 + row)].symbol())
        .collect::<String>()
}

fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn buffer_text(buffer: &Buffer) -> String {
    buffer.content().iter().map(|cell| cell.symbol()).collect()
}

fn draw_screen(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    words(&buffer_text(terminal.backend().buffer()))
}

#[test]
fn list_archive_footer_keeps_page_restore_and_search_hints() {
    let mut fixture = ListFixture::new(2);
    fixture.app.show_archived = true;
    for (matches, page, expected) in [
        (0, 0, "1/1"),
        (1, 0, "1/1"),
        (200, 0, "1/1"),
        (201, 0, "1/2"),
        (401, 1, "2/3"),
        (401, 2, "3/3"),
    ] {
        fixture.app.archive_matches = matches;
        fixture.app.archive_page = page;
        for query in [None, Some("thread root".to_string())] {
            fixture.app.search_query = query;
            let text = draw_screen(&mut fixture.app);
            let hint = if fixture.app.search_query.is_some() {
                format!("PgUp/PgDn page {expected} · enter restore · type to filter · esc cancel")
            } else {
                format!("[ ] or PgUp/PgDn page {expected} · enter restore · / search")
            };
            assert!(
                text.contains(&hint),
                "matches={matches}, page={page}: {text}"
            );
            assert!(
                text.contains(&format!("{matches} archived · page — tok")),
                "{text}"
            );
            assert!(!text.contains("0 tok"), "metadata usage is unknown: {text}");
        }
    }
}

#[test]
fn list_archive_and_active_usage_are_unknown_until_manually_measured() {
    let mut fixture = ListFixture::new(2);
    let area = Rect::new(2, 2, 120, 8);
    fixture.app.show_archived = true;
    for session in &mut fixture.app.all_sessions {
        session.archived = true;
    }
    let buffer = draw_list(&mut fixture.app, area);
    for row in [1, 4] {
        let text = row_text(&buffer, area, row);
        assert!(!text.contains(" tok"), "{text}");
        assert!(
            !text.contains("tok"),
            "unknown usage must not read as zero: {text}"
        );
    }

    fixture.app.show_archived = false;
    fixture.app.all_sessions[0].archived = false;
    fixture.app.all_sessions[0].tokens.total = 1_234;
    fixture.app.visible_aggregate = Aggregate::of_refs(fixture.app.visible_sessions());
    let buffer = draw_list(&mut fixture.app, area);
    let text = row_text(&buffer, area, 1);
    assert!(!text.contains(" tok"), "{text}");
    assert!(draw_screen(&mut fixture.app).contains("2 sessions · — tok"));
}

#[test]
fn list_visible_count_excludes_headers_and_running_count_includes_hidden_ptys() {
    let mut fixture = ListFixture::categorized();
    fixture.start_pty("session-00000");
    fixture.start_pty("hidden-running");
    fixture.start_pty("hidden-ended");
    fixture.app.ended.insert("hidden-ended".into());
    let text = buffer_text(&draw_list(&mut fixture.app, Rect::new(2, 2, 120, 16)));
    assert!(text.contains("5 visible / 2 running"), "{text}");
    assert_eq!(
        fixture.app.ptys.len(),
        3,
        "render must preserve hidden and ended PTYs"
    );
}

#[test]
fn list_thread_metadata_is_viewport_bounded_and_offscreen_activity_stays_local() {
    let mut fixture = ListFixture::categorized();
    fixture.app.all_sessions[5].last_prompt_at = Some(now() - Duration::minutes(3));
    fixture.start_pty("session-00005");
    let metadata = list_render_metadata(&fixture.app, std::iter::once(2));
    assert_eq!(metadata.len(), 1, "only the visible source is materialized");
    let child = &metadata["session-00001"];
    assert_eq!(child.source_count, 1);
    assert_eq!(child.visible_child_count, 0);
    assert!(child.child_prefix.is_none());
    let metadata = list_render_metadata(&fixture.app, 0..3);
    assert_eq!(metadata.len(), 2);
    assert_eq!(metadata["session-00000"].visible_child_count, 1);
    assert!(metadata["session-00001"].child_prefix.is_some());
    assert!(list_render_metadata(&fixture.app, std::iter::once(0)).is_empty());
    let area = Rect::new(2, 2, 120, 16);
    for ended in [false, true] {
        if ended {
            fixture.app.ended.insert("session-00005".into());
        }
        let buffer = draw_list(&mut fixture.app, area);
        for (row, source) in [(2, 0), (3, 1)] {
            let text = row_text(&buffer, area, row);
            let expected = relative_time(fixture.app.all_sessions[source].last_active, now());
            assert!(
                text.split_whitespace().any(|word| word == expected),
                "hidden lane ended={ended} must not replace source {source}'s {expected}: {text}"
            );
        }
    }
}

#[test]
fn list_headers_keep_band_counts_folds_sync_and_thread_labels() {
    let mut fixture = ListFixture::categorized();
    let area = Rect::new(3, 2, 145, 19);
    let buffer = draw_list(&mut fixture.app, area);
    for (row, expected) in [
        (0, "── recent 3 sessions"),
        (1, "▶ ▾ Topic A 2 ⇄ auto"),
        (4, ""),
        (5, "▸ Folded"),
        (6, ""),
        (7, "· uncategorized"),
        (9, ""),
        (10, "── older 2 sessions"),
        (11, "▾ Topic B ⇄ auto"),
        (13, ""),
        (14, "· uncategorized"),
    ] {
        assert_eq!(words(&row_text(&buffer, area, row)), expected, "row {row}");
    }
    assert_eq!(fixture.app.list_rows, 17);
    assert!(!fixture.app.hero_visible);
}

#[test]
fn list_scrolling_keeps_selection_and_absolute_cell_coordinates() {
    let mut fixture = ListFixture::categorized();
    let labels = [
        "── recent",
        "Topic A",
        "Root",
        "Lane",
        "",
        "Folded",
        "",
        "uncategorized",
        "Recent loose",
        "",
        "── older",
        "Topic B",
        "Older first",
        "",
        "uncategorized",
        "Older last",
    ];
    let logical_to_item: [usize; 10] = [1, 2, 3, 5, 7, 8, 11, 12, 14, 15];
    for width in [45, 88, 145] {
        for height in [3, 4, 5, 7, 16] {
            let area = Rect::new(4, 3, width, height);
            let rows = usize::from(height - 2);
            for (selected, item) in logical_to_item.into_iter().enumerate() {
                fixture.app.selected = selected;
                let start = (item + 1).saturating_sub(rows);
                let buffer = draw_list(&mut fixture.app, area);
                for (offset, label) in labels.iter().skip(start).take(rows).enumerate() {
                    let text = row_text(&buffer, area, offset as u16);
                    assert!(
                        if label.is_empty() { text.trim().is_empty() } else { text.contains(label) },
                        "{width}x{height}, selected {selected}, cell row {offset}: {text:?}, expected {label:?}"
                    );
                }
                let pointer = (area.x + 1, area.y + 1 + (item - start) as u16);
                assert_eq!(buffer[pointer].symbol(), "▶", "selection {selected}");
                assert_eq!(fixture.app.selected, selected);
                assert_eq!(fixture.app.list_rows, height - 2);
            }
        }
    }
}

#[test]
fn list_page_movement_never_selects_a_gap_or_loses_the_cursor() {
    let mut fixture = ListFixture::categorized();
    let area = Rect::new(4, 3, 80, 7);
    for direction in [1, 1, 1, -1, -1, -1] {
        fixture.app.move_page(direction);
        let selected = fixture.app.selected;
        let buffer = draw_list(&mut fixture.app, area);
        let selected_item = fixture.app.list_row_items[selected];
        assert_eq!(fixture.app.list_item_rows[selected_item], Some(selected));
        assert_eq!(
            (0..area.height - 2)
                .filter(|row| buffer[(area.x + 1, area.y + 1 + row)].symbol() == "▶")
                .count(),
            1,
            "selected logical row {selected}",
        );
    }
}

#[test]
fn list_mouse_wheel_uses_logical_rows_including_headers() {
    let mut fixture = ListFixture::categorized();
    let area = Rect::new(5, 4, 100, 6);
    for (kind, expected) in [
        (MouseEventKind::ScrollDown, 1),
        (MouseEventKind::ScrollDown, 2),
        (MouseEventKind::ScrollDown, 3),
        (MouseEventKind::ScrollUp, 2),
        (MouseEventKind::ScrollUp, 1),
        (MouseEventKind::ScrollUp, 0),
        (MouseEventKind::ScrollUp, 9),
    ] {
        assert!(crate::handle_mouse(
            &mut fixture.app,
            MouseEvent {
                kind,
                column: area.x + 2,
                row: area.y + 2,
                modifiers: KeyModifiers::NONE,
            }
        ));
        assert_eq!(fixture.app.selected, expected);
        let buffer = draw_list(&mut fixture.app, area);
        let highlighted = (0..area.height - 2)
            .filter(|row| buffer[(area.x + 1, area.y + 1 + row)].symbol() == "▶")
            .count();
        assert_eq!(highlighted, 1, "mouse selection {expected}");
    }
    let selected = fixture.app.selected;
    assert!(!crate::handle_mouse(
        &mut fixture.app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 2,
            row: area.y + 2,
            modifiers: KeyModifiers::NONE,
        }
    ));
    assert_eq!(fixture.app.selected, selected);
}

#[test]
fn list_large_history_renders_the_selected_tail() {
    let mut fixture = ListFixture::new(5_000);
    fixture.app.selected = 4_999;
    let area = Rect::new(2, 3, 120, 32);
    let buffer = draw_list(&mut fixture.app, area);
    assert!(row_text(&buffer, area, 0).contains("row-04970"));
    assert!(row_text(&buffer, area, 29).contains("row-04999"));
    assert_eq!(buffer[(area.x + 1, area.y + 30)].symbol(), "▶");
    assert_eq!(fixture.app.list_rows, 30);
    assert!(!fixture.app.hero_visible);
}

#[test]
fn list_single_band_keeps_its_label_and_selection_without_a_second_divider() {
    let mut fixture = ListFixture::new(3);
    let area = Rect::new(2, 2, 100, 6);
    for (recent_count, expected) in [(0, "older"), (3, "recent"), (usize::MAX, "recent")] {
        fixture.app.recent_count = recent_count;
        fixture.app.selected = 2;
        let buffer = draw_list(&mut fixture.app, area);
        assert_eq!(
            words(&row_text(&buffer, area, 0)),
            format!("── {expected} 3 sessions")
        );
        assert!(row_text(&buffer, area, 3).contains("row-00002"));
        assert_eq!(buffer[(area.x + 1, area.y + 4)].symbol(), "▶");
    }
}

#[test]
fn empty_list_shows_its_hero_without_a_selection() {
    let mut fixture = ListFixture::new(4);
    fixture.app.all_sessions.clear();
    fixture.app.visible.clear();
    let area = Rect::new(2, 2, 100, 30);
    let buffer = draw_list(&mut fixture.app, area);
    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("No sessions yet."), "{text}");
    assert!(fixture.app.hero_visible);
    assert_eq!(fixture.app.list_rows, 28);
}

#[test]
#[ignore = "release-profile actual list rendering benchmark"]
fn list_actual_render_5k_50k() {
    benchmark_list_render(1);
}

#[test]
#[ignore = "release-profile actual threaded list rendering benchmark"]
fn list_actual_render_threaded_5k_50k() {
    benchmark_list_render(4);
}

fn benchmark_list_render(thread_width: usize) {
    let mut worst = std::time::Duration::ZERO;
    for count in [5_000, 50_000] {
        let mut fixture = ListFixture::new(count);
        if thread_width > 1 {
            for index in 0..count {
                let root = index / thread_width * thread_width;
                if root != index {
                    fixture.app.state.set_handoff_link(
                        &fixture.app.all_sessions[index].id,
                        &fixture.app.all_sessions[root].id,
                        PathBuf::new(),
                        now(),
                    );
                }
            }
            let metadata = list_render_metadata(&fixture.app, count - 38..count);
            assert!(
                metadata.len() <= 2 * 38,
                "metadata must be viewport bounded"
            );
            assert_eq!(
                metadata[fixture.app.all_sessions[count - 4].id.as_str()].visible_child_count,
                3
            );
        }
        fixture.app.selected = count - 1;
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        let mut timings = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            terminal
                .draw(|frame| session_list(frame, &mut fixture.app, frame.area(), now()))
                .unwrap();
            std::hint::black_box(terminal.backend().buffer());
            timings.push(start.elapsed());
        }
        timings.sort_unstable();
        worst = worst.max(timings[2]);
        eprintln!(
            "list_actual_render rows={count} thread_width={thread_width} viewport=140x40 samples=3 p50_ms={:.3} max_ms={:.3}",
            timings[1].as_secs_f64() * 1000.0,
            timings[2].as_secs_f64() * 1000.0,
        );
        let text = buffer_text(terminal.backend().buffer());
        assert!(
            text.contains(&format!("row-{:05}", count - 1)),
            "selected tail must render"
        );
    }
    assert!(
        worst < std::time::Duration::from_millis(100),
        "actual fixture render exceeded the local 100 ms budget: {worst:?}"
    );
}
