use super::*;
use crate::startup_routing::StartupRouting;
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
        app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
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
            crate::pty::PtySession::spawn_with_routing(&command, id, 8, 80, RoutingMode::Native)
                .expect("start isolated fixture PTY");
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
                text.contains(&format!("{matches} archived · usage not loaded")),
                "{text}"
            );
            assert!(!text.contains("0 tok"), "metadata usage is unknown: {text}");
        }
    }
}

#[test]
fn list_archive_usage_is_unknown_while_active_usage_remains_loaded() {
    let mut fixture = ListFixture::new(2);
    let area = Rect::new(2, 2, 120, 8);
    fixture.app.show_archived = true;
    for session in &mut fixture.app.all_sessions {
        session.archived = true;
    }
    let buffer = draw_list(&mut fixture.app, area);
    for row in [1, 3] {
        let text = row_text(&buffer, area, row);
        assert!(text.trim_end().ends_with('—'), "{text}");
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
    assert!(text.contains("1.2K tok"), "{text}");
    assert!(draw_screen(&mut fixture.app).contains("2 sessions · 1.2K tok"));
}

#[test]
fn list_scan_summary_distinguishes_active_usage_from_unloaded_archive() {
    let mut fixture = ListFixture::new(2);
    fixture.app.all_sessions[0].tokens.total = 1_234;
    fixture.app.aggregate = Aggregate::of(&fixture.app.all_sessions);
    let mut archived = fixture.app.all_sessions[1].clone();
    archived.archived = true;
    archived.tokens = TokenUsage::default();
    fixture.app.archive_catalog = std::sync::Arc::new(vec![archived; 3]);
    fixture.app.archive_matches = 1; // Search matches are not the catalog count.
    fixture.app.screen = Screen::ScanSummary;
    let text = draw_screen(&mut fixture.app);
    assert!(text.contains("2 active sessions collected"), "{text}");
    assert!(text.contains("3 archived · usage not loaded"), "{text}");
    assert!(text.contains("active tokens 1.2K"), "{text}");
    assert!(
        !text.contains("total tokens"),
        "archive usage is not totaled: {text}"
    );
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
fn list_thread_metadata_is_viewport_bounded_and_includes_offscreen_lanes() {
    let mut fixture = ListFixture::categorized();
    fixture.app.all_sessions[0].last_prompt_at = Some(now() - Duration::hours(1));
    fixture.app.all_sessions[1].last_prompt_at = Some(now() - Duration::minutes(7));
    fixture.app.all_sessions[5].last_prompt_at = Some(now() - Duration::minutes(3));
    fixture.start_pty("session-00005");
    let metadata = list_render_metadata(&fixture.app, std::iter::once(2));
    assert_eq!(
        metadata.len(),
        2,
        "only the visible child and its root are materialized"
    );
    assert_eq!(metadata["session-00001"].child_count, 0);
    let root = &metadata["session-00000"];
    assert_eq!(
        root.child_count, 2,
        "hidden lanes still contribute direct child counts"
    );
    assert!(
        root.live_now,
        "an offscreen running lane makes the thread live"
    );
    assert_eq!(root.last_active, Some(now() - Duration::minutes(2)));
    assert_eq!(root.last_prompt_at, Some(now() - Duration::minutes(3)));
    assert!(list_render_metadata(&fixture.app, std::iter::once(0)).is_empty());
    let area = Rect::new(2, 2, 120, 16);
    let buffer = draw_list(&mut fixture.app, area);
    for row in [2, 3] {
        let text = words(&row_text(&buffer, area, row));
        assert!(
            text.contains("3m"),
            "a live thread displays its newest prompt: {text}"
        );
    }
    fixture.app.ended.insert("session-00005".into());
    let buffer = draw_list(&mut fixture.app, area);
    for row in [2, 3] {
        let text = words(&row_text(&buffer, area, row));
        assert!(
            text.contains("2m"),
            "an ended thread displays its newest activity: {text}"
        );
    }
}

#[test]
fn list_headers_keep_band_counts_folds_sync_and_thread_labels() {
    let mut fixture = ListFixture::categorized();
    let area = Rect::new(3, 2, 145, 16);
    let buffer = draw_list(&mut fixture.app, area);
    for (row, expected) in [
        (0, "── recent 3 sessions"),
        (1, "▶ ▾ Topic A · 2 sessions ⇄ auto"),
        (4, "▸ Folded ⇄ off"),
        (5, "· uncategorized"),
        (7, "── older 2 sessions"),
        (8, "▾ Topic B ⇄ auto"),
        (10, "· uncategorized"),
    ] {
        assert_eq!(words(&row_text(&buffer, area, row)), expected, "row {row}");
    }
    let root = words(&row_text(&buffer, area, 2));
    let lane = words(&row_text(&buffer, area, 3));
    assert!(root.contains("2m ▾ Root [2 lanes]"), "{root}");
    assert!(lane.contains("2m └─ Lane"), "{lane}");
    assert_eq!(fixture.app.list_rows, 14);
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
        "Folded",
        "uncategorized",
        "Recent loose",
        "── older",
        "Topic B",
        "Older first",
        "uncategorized",
        "Older last",
    ];
    let logical_to_item: [usize; 10] = [1, 2, 3, 4, 5, 6, 8, 9, 10, 11];
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
                        text.contains(label),
                        "{width}x{height}, selected {selected}, cell row {offset}: {text:?}, expected {label:?}"
                    );
                }
                let pointer = (area.x + 1, area.y + 1 + (item - start) as u16);
                assert_eq!(buffer[pointer].symbol(), "▶", "selection {selected}");
                assert_eq!(buffer[pointer].fg, ACCENT, "selection {selected}");
                assert_eq!(buffer[pointer].bg, Color::Rgb(48, 60, 96));
                assert_eq!(fixture.app.selected, selected);
                assert_eq!(fixture.app.list_rows, height - 2);
            }
        }
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
fn list_startup_and_empty_view_keep_their_selection_and_hero_rules() {
    let mut fixture = ListFixture::new(4);
    fixture.app.startup_routing = StartupRouting::default();
    fixture.app.selected = 3;
    let area = Rect::new(2, 2, 100, 5);
    let buffer = draw_list(&mut fixture.app, area);
    assert_eq!(words(&row_text(&buffer, area, 0)), "── recent 2 sessions");
    assert!(!row_text(&buffer, area, 1).contains('▶'));
    assert!(row_text(&buffer, area, 1).contains("row-00000"));
    assert!(!fixture.app.hero_visible);

    fixture.app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
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
                metadata[fixture.app.all_sessions[count - 4].id.as_str()].child_count,
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
