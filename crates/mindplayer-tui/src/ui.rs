//! Rendering for every screen. `render` also records the right-pane size back
//! into `App` so the PTY can be spawned/resized at the correct dimensions.

use crate::app::{App, Focus, PaneLayout, Screen, SessionStatus, MAX_PANES};
use crate::mascot;
use crate::orchestration;
use crate::terminal_view::TerminalView;
use chrono::{DateTime, Utc};
use mindplayer_core::tokens::human_tokens;
use mindplayer_core::Agent;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::Frame;
use std::path::Path;

const ACCENT: Color = Color::Rgb(126, 162, 247);
const DIM: Color = Color::Rgb(140, 146, 158);
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

fn agent_tag(agent: Agent) -> (&'static str, Color) {
    match agent {
        Agent::Codex => ("codex ", ACCENT),
        Agent::Claude => ("claude", Color::Magenta),
        Agent::Kiro => ("kiro  ", Color::Cyan),
    }
}

fn plural_session(count: usize) -> &'static str {
    if count == 1 {
        "session"
    } else {
        "sessions"
    }
}

pub fn render(f: &mut Frame, app: &mut App) {
    match app.screen {
        Screen::ScopeSelect => scope_select(f, app),
        Screen::Scanning => scanning(f, app),
        Screen::ScanSummary => scan_summary(f, app),
        Screen::Main => main_view(f, app),
    }
}

fn title_bar(area: Rect, f: &mut Frame) {
    let line = Line::from(vec![
        Span::styled(
            "◆ MindPlayer",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" v{}", env!("MINDPLAYER_VERSION")),
            Style::default().fg(DIM),
        ),
        Span::styled(
            "  Codex / Claude / Kiro session manager",
            Style::default().fg(DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn footer(area: Rect, f: &mut Frame, keys: &str) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(keys, Style::default().fg(DIM)))),
        area,
    );
}

/// Draw the animated mascot, horizontally centered and top-anchored in `area`.
fn draw_mascot(f: &mut Frame, area: Rect, tick: usize) {
    if area.width < mascot::WIDTH || area.height < mascot::HEIGHT {
        return;
    }
    let r = Rect {
        x: area.x + (area.width - mascot::WIDTH) / 2,
        y: area.y,
        width: mascot::WIDTH,
        height: mascot::HEIGHT,
    };
    f.render_widget(Paragraph::new(mascot::lines(tick)), r);
}

fn scope_select(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());
    title_bar(chunks[0], f);
    draw_mascot(f, chunks[1], app.spinner);

    let options = [
        format!("working dir   {}", app.cwd.display()),
        "global        all sessions everywhere".to_string(),
    ];
    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let selected = i == app.scope_choice;
            let marker = if selected { "▶ " } else { "  " };
            let style = if selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(format!("{marker}{text}"), style)))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Where should MindPlayer collect sessions? ")
        .border_style(Style::default().fg(ACCENT));
    let inner = centered(chunks[1], 70, 8);
    f.render_widget(List::new(items).block(block), inner);

    footer(chunks[2], f, "↑↓ choose   enter scan   q quit");
}

fn scanning(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());
    title_bar(chunks[0], f);

    draw_mascot(f, chunks[1], app.spinner);
    let spin = SPINNER[app.spinner % SPINNER.len()];
    let area = centered(chunks[1], 60, 5);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Collecting ");
    let body = Paragraph::new(vec![
        Line::from(Span::styled(
            format!("{spin}  scanning {} ...", app.scope_label()),
            Style::default().fg(ACCENT),
        )),
        Line::from(Span::styled(
            "reading ~/.codex, ~/.claude, and ~/.kiro sessions",
            Style::default().fg(DIM),
        )),
    ])
    .block(block)
    .alignment(Alignment::Center);
    f.render_widget(body, area);

    footer(chunks[2], f, "collecting…");
}

fn scan_summary(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());
    title_bar(chunks[0], f);

    let a = &app.aggregate;
    let total = a.session_count().max(1);
    let codex_ratio = a.codex_count as f64 / total as f64;

    let area = centered(chunks[1], 64, 11);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Collected ");
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
    f.render_widget(block, area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("✓ ", Style::default().fg(Color::Green)),
            Span::raw(format!("{} sessions collected", a.session_count())),
        ])),
        rows[0],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("codex  ", Style::default().fg(ACCENT)),
            Span::raw(format!("{:>3}", a.codex_count)),
            Span::styled("   claude  ", Style::default().fg(Color::Magenta)),
            Span::raw(format!("{:>3}", a.claude_count)),
            Span::styled("   kiro  ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{:>3}", a.kiro_count)),
        ])),
        rows[2],
    );
    f.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(ACCENT))
            .ratio(codex_ratio)
            .label(format!("codex {:.0}%", codex_ratio * 100.0)),
        rows[3],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("total tokens  ", Style::default().fg(DIM)),
            Span::styled(
                human_tokens(a.total.total),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "   (codex {} · claude {} · kiro {})",
                    human_tokens(a.codex.total),
                    human_tokens(a.claude.total),
                    if a.kiro_count > 0 { "—" } else { "0" },
                ),
                Style::default().fg(DIM),
            ),
        ])),
        rows[4],
    );

    footer(chunks[2], f, "enter open mindplayer   q quit");
}

fn main_view(f: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());
    title_bar(outer[0], f);

    // Read the clock once per frame and thread it down, so the sort key and the
    // displayed relative times can't disagree (and we don't call Utc::now()
    // multiple times for a single render).
    let now = Utc::now();

    // Full-screen switch: the list OR the live session fills the body — no split.
    // Background sessions keep running regardless of which view is shown.
    match app.focus {
        Focus::List => session_list(f, app, outer[1], now),
        Focus::Terminal => live_pane(f, app, outer[1]),
    }

    let keys: String = match app.focus {
        Focus::List if app.multi_select => {
            "MULTI-SELECT · space mark · enter launch all marked · esc cancel".to_string()
        }
        Focus::List => {
            // When a live view is detached but still running, surface that ctrl-x
            // jumps back into it.
            let live = if !app.panes.is_empty() {
                format!("ctrl-x live ({}) · ", app.panes.len())
            } else {
                String::new()
            };
            format!("{live}enter open · v multi-select · n new · h handoff   View: / search · ? help")
        }
        Focus::Terminal => {
            "ctrl-x list · tab/ctrl-w pane · ctrl-z zoom · ctrl-o layout · ctrl-q close · wheel history · drag=copy this pane"
                .to_string()
        }
    };
    let status = if app.status.is_empty() {
        app.summary_line()
    } else {
        format!("{}  ·  {}", app.status, app.summary_line())
    };
    let footer_line = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(outer[2]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(status, Style::default().fg(DIM)))),
        footer_line[0],
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(keys, Style::default().fg(DIM))))
            .alignment(Alignment::Right),
        footer_line[1],
    );

    if app.help_visible {
        help_popup(f);
    } else if let Some(draft) = &app.dispatch_apply {
        dispatch_apply_popup(f, draft);
    } else if let Some(draft) = &app.dispatch {
        dispatch_popup(f, draft);
    } else if let Some(draft) = &app.broadcast {
        broadcast_popup(f, draft);
    } else if let Some(draft) = &app.orchestration {
        orchestration_popup(f, draft);
    } else if let Some(choice) = app.handoff_picker {
        handoff_popup(f, choice, app.selected_session().map(|s| s.agent));
    } else if let Some(choice) = app.new_picker {
        new_session_popup(f, choice);
    } else if let Some(label) = &app.new_label {
        if app.label_target.is_some() {
            label_edit_popup(f, label);
        } else {
            label_input_popup(f, app.new_agent, label);
        }
    } else if let Some(path) = &app.dir_input {
        dir_input_popup(f, path);
    }
}

fn dir_input_popup(f: &mut Frame, path: &str) {
    let area = centered(f.area(), 64, 7);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Set working dir ");
    let lines = vec![
        Line::from(Span::styled(
            "Directory (blank = global, ~ allowed):",
            Style::default().fg(DIM),
        )),
        Line::from(vec![
            Span::raw(path.to_string()),
            Span::styled("▏", Style::default().fg(ACCENT)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "enter scan   esc cancel",
            Style::default().fg(DIM),
        )),
    ];
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn label_edit_popup(f: &mut Frame, label: &str) {
    let area = centered(f.area(), 54, 7);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Set label ");
    let lines = vec![
        Line::from(Span::styled(
            "Label (blank to clear):",
            Style::default().fg(DIM),
        )),
        Line::from(vec![
            Span::raw(label.to_string()),
            Span::styled("▏", Style::default().fg(ACCENT)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "enter save   esc cancel",
            Style::default().fg(DIM),
        )),
    ];
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn label_input_popup(f: &mut Frame, agent: Option<Agent>, label: &str) {
    let area = centered(f.area(), 54, 7);
    f.render_widget(Clear, area);
    let agent_name = agent.map(Agent::as_str).unwrap_or("session");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" New {agent_name} session "));
    let lines = vec![
        Line::from(Span::styled(
            "Label / subject (optional):",
            Style::default().fg(DIM),
        )),
        Line::from(vec![
            Span::raw(label.to_string()),
            Span::styled("▏", Style::default().fg(ACCENT)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "enter start   esc cancel",
            Style::default().fg(DIM),
        )),
    ];
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn session_list(f: &mut Frame, app: &mut App, area: Rect, now: DateTime<Utc>) {
    let focused = app.focus == Focus::List;
    let tab = if app.show_archived {
        "archived"
    } else {
        "active"
    };
    let subs = if app.show_subagents { " +sub" } else { "" };
    let search = app
        .search_query
        .as_deref()
        .map(|query| format!(" · /{query}"))
        .unwrap_or_default();
    let multi = if app.multi_select {
        format!(" · MULTI-SELECT ({} marked)", app.marked.len())
    } else {
        String::new()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.multi_select {
            Color::Green
        } else if focused {
            ACCENT
        } else {
            DIM
        }))
        .title(format!(
            " Sessions · recent first · {tab}{subs}{search}{multi} ({}) ",
            app.visible.len()
        ));

    // Fill the pane: title gets whatever width is left after border, status
    // badge, agent, time, identity, and usage columns.
    let show_id = area.width >= 58;
    let show_cwd = area.width >= 78;
    let identity_width = usize::from(show_id) * 11 + usize::from(show_cwd) * 11;
    // 38 = status badge + agent bar/tag + time + thread prefix + the 2-col
    // multi-select mark column prepended to every row.
    let max_title = (area.width as usize)
        .saturating_sub(38 + identity_width)
        .max(12);
    // Top-level categories. rebuild_visible sorts every recent group (touched
    // in the last 24h OR running live now) above the rest and records the
    // boundary in `recent_count`, so the split is position-based — the
    // headers always match the sort order and never recompute per row.
    let recent_count = app.recent_count.min(app.visible.len());
    let older_count = app.visible.len().saturating_sub(recent_count);

    let mut items: Vec<ListItem> = Vec::new();
    let mut selected_item = None;
    let mut current_recent: Option<bool> = None;
    for row in 0..app.visible.len() {
        let Some(s) = app.session_at(row) else {
            continue;
        };
        let is_recent = row < recent_count;
        if current_recent != Some(is_recent) {
            current_recent = Some(is_recent);
            let (label, count) = if is_recent {
                ("recent", recent_count)
            } else {
                ("older", older_count)
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled("  ── ", Style::default().fg(DIM)),
                Span::styled(
                    label,
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {count} {}", plural_session(count)),
                    Style::default().fg(DIM),
                ),
            ])));
        }
        if row == app.selected {
            selected_item = Some(items.len());
        }
        {
            let marked = app.marked.contains(&s.id);
            let depth = app.session_depth(&s.id);
            let child_count = app.thread_child_count(&s.id);
            let (tag, tag_color) = agent_tag(s.agent);
            // Clear, fixed-width status badge so it's easy to scan a column of
            // running / working / done sessions.
            let (badge, badge_color, badge_bold) = match app.session_status(&s.id) {
                SessionStatus::Blocked => ("● blocked", Color::Rgb(245, 180, 90), true),
                SessionStatus::Working => ("● working", Color::Green, true),
                SessionStatus::Idle => ("● idle   ", ACCENT, false),
                SessionStatus::Ended => ("○ done   ", Color::Rgb(150, 120, 120), false),
                SessionStatus::Inactive => ("         ", DIM, false),
            };
            let mut badge_style = Style::default().fg(badge_color);
            if badge_bold {
                badge_style = badge_style.add_modifier(Modifier::BOLD);
            }
            let (thread_prefix, title_style) = if depth > 0 {
                ("  └─ ", Style::default().fg(Color::Rgb(190, 196, 210)))
            } else if child_count > 0 {
                ("▾ ", Style::default().add_modifier(Modifier::BOLD))
            } else {
                ("  ", Style::default())
            };
            let title_suffix = if depth == 0 && child_count > 0 {
                format!("  [{child_count} lanes]")
            } else {
                String::new()
            };
            let (mark_glyph, mark_style) = if marked {
                (
                    "✓ ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default())
            };
            // Time column reflects live activity: a running session (or a thread
            // root with a running/active-today lane) reads "now" / its lane's
            // recent time rather than its own possibly-stale transcript mtime.
            let (live_now, eff_active) = app.row_activity(s, child_count);
            let when = if live_now {
                "now".to_string()
            } else {
                relative_time(eff_active, now)
            };
            let mut spans = vec![
                Span::styled(mark_glyph, mark_style),
                Span::styled(format!("{badge} "), badge_style),
                Span::styled(
                    "▌",
                    Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{tag} "),
                    Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                ),
                // last-active recency: the list is sorted newest-first, so this
                // column descends from top to bottom.
                Span::styled(format!("{when:>4} "), Style::default().fg(DIM)),
                Span::styled(thread_prefix, Style::default().fg(DIM)),
                Span::styled(
                    truncate(&format!("{}{title_suffix}", s.title), max_title),
                    title_style,
                ),
            ];
            if show_id {
                spans.push(Span::styled(
                    format!("  {}", short(&s.id)),
                    Style::default().fg(Color::Rgb(104, 185, 132)),
                ));
            }
            if show_cwd {
                spans.push(Span::styled(
                    format!(" {}", truncate(&cwd_leaf(&s.cwd), 10)),
                    Style::default().fg(DIM),
                ));
            }
            spans.push(Span::styled(
                // Kiro records no token totals; show its context-window
                // occupancy (e.g. "15%") instead, or "—" if unknown.
                if s.agent == Agent::Kiro {
                    match s.context_pct {
                        Some(p) => format!("  {p:.0}%"),
                        None => "  —".to_string(),
                    }
                } else {
                    format!("  {}", human_tokens(s.tokens.total))
                },
                Style::default().fg(DIM),
            ));
            items.push(ListItem::new(Line::from(spans)));
        }
    }

    // Record the visible row count (inside the borders) so PageUp/PageDown can
    // step by a screenful.
    app.list_rows = area.height.saturating_sub(2);

    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(selected_item);
    }
    let rendered_rows = items.len();
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(ACCENT)
                .bg(Color::Rgb(48, 60, 96))
                .add_modifier(Modifier::BOLD),
        )
        // Arrow makes the current selection obvious; non-selected rows are
        // padded by the same width so columns stay aligned.
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut state);

    // When the list is short, fill the empty space below it with a centered
    // "hero" block (mascot + tagline + status legend) so the pane reads as
    // intentional instead of a stranded sprite over a void.
    let content_top = area.y + 1; // inside the top border
    let bottom = area.y + area.height - 1; // bottom border row
    let region_top = content_top + rendered_rows as u16;
    let region_h = bottom.saturating_sub(region_top);
    const GAP: u16 = 1;
    let block_h = mascot::HEIGHT + GAP + 2; // mascot + gap + tagline + legend
                                            // Record whether the animated hero is actually on screen, so the event loop
                                            // only forces ~12fps redraws of the list when there's something to animate.
    app.hero_visible = region_h >= block_h + 2;
    if app.hero_visible {
        let inner_x = area.x + 1;
        let inner_w = area.width.saturating_sub(2);
        let top = region_top + (region_h - block_h) / 2;
        draw_mascot(
            f,
            Rect {
                x: inner_x,
                y: top,
                width: inner_w,
                height: mascot::HEIGHT,
            },
            app.spinner,
        );
        let tagline = Paragraph::new(Line::from(Span::styled(
            "Run many Codex · Claude · Kiro sessions like tabs",
            Style::default().fg(DIM),
        )))
        .alignment(Alignment::Center);
        f.render_widget(
            tagline,
            Rect {
                x: inner_x,
                y: top + mascot::HEIGHT + GAP,
                width: inner_w,
                height: 1,
            },
        );
        let legend = Paragraph::new(Line::from(vec![
            Span::styled("● blocked", Style::default().fg(Color::Rgb(245, 180, 90))),
            Span::styled("   ● working", Style::default().fg(Color::Green)),
            Span::styled("   ● idle", Style::default().fg(ACCENT)),
            Span::styled("   ○ done", Style::default().fg(Color::Rgb(150, 120, 120))),
        ]))
        .alignment(Alignment::Center);
        f.render_widget(
            legend,
            Rect {
                x: inner_x,
                y: top + mascot::HEIGHT + GAP + 1,
                width: inner_w,
                height: 1,
            },
        );
    }
}

/// Number of grid rows to split the live area into for `n` panes. Horizontal
/// layout favors wide grids (fewer rows / more columns), Vertical favors tall
/// ones, so the chosen split direction still reads as "side-by-side" vs
/// "stacked" even past the 3-pane point. Small counts (up to 6) use
/// hand-tuned layouts; an orchestration thread can easily have far more lanes
/// than that (20+ is routine), so anything larger falls back to a near-square
/// grid sized by `sqrt(n)` — still biased wide (floor) or tall (ceil) to match
/// the chosen layout.
fn grid_rows(n: usize, layout: PaneLayout) -> usize {
    match (n, layout) {
        (_, PaneLayout::Single) | (1, _) => 1,
        (2, PaneLayout::Horizontal) | (3, PaneLayout::Horizontal) => 1,
        (2, PaneLayout::Vertical) => 2,
        (3, PaneLayout::Vertical) => 3,
        (4, _) => 2,
        (5, PaneLayout::Horizontal) | (6, PaneLayout::Horizontal) => 2,
        (5, PaneLayout::Vertical) | (6, PaneLayout::Vertical) => 3,
        (_, PaneLayout::Horizontal) => ((n as f64).sqrt().floor() as usize).max(1),
        (_, PaneLayout::Vertical) => ((n as f64).sqrt().ceil() as usize).max(1),
    }
}

pub fn compute_pane_rects(area: Rect, n: usize, layout: PaneLayout) -> Vec<Rect> {
    let n = n.clamp(1, MAX_PANES);
    if n == 1 || layout == PaneLayout::Single {
        return vec![area];
    }

    // Distribute panes row-major across a balanced grid: each row gets either
    // `base_cols` or `base_cols + 1` columns so there are never empty cells.
    let rows = grid_rows(n, layout);
    let base_cols = n / rows;
    let extra = n % rows;
    let mut rects = Vec::with_capacity(n);
    let band_base = area.height / rows as u16;
    let band_rem = area.height % rows as u16;
    let mut y = area.y;
    for r in 0..rows {
        let band_h = band_base + u16::from((r as u16) < band_rem);
        let cols = base_cols + usize::from(r < extra);
        let col_base = area.width / cols as u16;
        let col_rem = area.width % cols as u16;
        let mut x = area.x;
        for c in 0..cols {
            let cell_w = col_base + u16::from((c as u16) < col_rem);
            rects.push(Rect {
                x,
                y,
                width: cell_w,
                height: band_h,
            });
            x = x.saturating_add(cell_w);
        }
        y = y.saturating_add(band_h);
    }
    rects
}

fn live_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let focused_view = app.focus == Focus::Terminal;
    let live_count = app.live_pty_count();

    if app.panes.is_empty() {
        let count_suffix = if live_count > 1 {
            format!(" [{live_count} live]")
        } else {
            String::new()
        };
        let title = match app.selected_session() {
            Some(s) => format!(
                " Live · {} (enter to resume){count_suffix} ",
                app.session_display_name(&s.id, 42)
            ),
            None => " Live ".to_string(),
        };
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .title(title)
            .border_style(Style::default().fg(if focused_view { ACCENT } else { DIM }));
        let inner = block.inner(area);
        f.render_widget(block, area);
        app.pty_rows = inner.height.max(1);
        app.pty_cols = inner.width.max(1);
        app.pty_x = inner.x;
        app.pty_y = inner.y;
        draw_mascot(f, inner, app.spinner);
        let hint = Paragraph::new(vec![
            Line::from(Span::styled(
                "Select a session and press enter to resume it here.",
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled(
                "Press n to start a new Codex or Claude session.",
                Style::default().fg(DIM),
            )),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
        let hint_area = Rect {
            x: inner.x,
            y: inner.y + mascot::HEIGHT.min(inner.height),
            width: inner.width,
            height: inner.height.saturating_sub(mascot::HEIGHT),
        };
        f.render_widget(hint, hint_area);
        return;
    }

    let panes = app.panes.clone();

    if app.zoomed {
        // Full-size view of just the focused pane. Bounds for every other pane
        // are dropped so a stale (smaller, pre-zoom) rect can never steal a
        // mouse click that lands inside the now-fullscreen pane's area.
        let idx = app.focused.min(panes.len() - 1);
        let sid = panes[idx].clone();
        app.pane_bounds.retain(|id, _| id == &sid);
        app.pane_sizes.retain(|id, _| id == &sid);
        render_pane(f, app, &sid, idx, panes.len(), area, focused_view, true);
        return;
    }

    let rects = compute_pane_rects(area, panes.len(), app.effective_layout());
    for (idx, sid) in panes.iter().enumerate() {
        let pane_area = rects.get(idx).copied().unwrap_or(area);
        let pane_focused = focused_view && idx == app.focused;
        render_pane(
            f,
            app,
            sid,
            idx,
            panes.len(),
            pane_area,
            pane_focused,
            false,
        );
    }
}

/// Render one live pane's border, title, and terminal contents into `pane_area`
/// — shared by the normal split-grid layout and the single-pane zoomed view so
/// the two never drift out of sync.
#[allow(clippy::too_many_arguments)]
fn render_pane(
    f: &mut Frame,
    app: &mut App,
    sid: &str,
    idx: usize,
    total: usize,
    pane_area: Rect,
    pane_focused: bool,
    zoomed: bool,
) {
    let ended = app.ended.contains(sid);
    let (dot, dot_color) = pane_dot(app, sid, ended);
    let name = app.session_display_name(sid, pane_area.width.saturating_sub(18) as usize);
    // A plain-colored name blends into the border line and is easy to miss when
    // several panes are open — a bold, high-contrast chip (bg = the same color
    // as the status dot) makes "which session is this" readable at a glance
    // without stealing a content row from the terminal view.
    let title = Line::from(vec![
        Span::styled(dot, Style::default().fg(dot_color)),
        Span::styled(
            format!(" {name} "),
            Style::default()
                .fg(Color::Rgb(15, 17, 22))
                .bg(dot_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {}/{} ", idx + 1, total),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        if zoomed {
            Span::styled("🔍 ", Style::default().fg(ACCENT))
        } else {
            Span::raw("")
        },
        if ended {
            Span::styled("(ended) ", Style::default().fg(DIM))
        } else {
            Span::raw("")
        },
    ]);
    // A thick border on the focused pane makes it unmistakable which one Tab
    // will act on next when several panes are open side by side.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if pane_focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .title(title)
        .border_style(if pane_focused {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        });
    let inner = block.inner(pane_area);
    f.render_widget(block, pane_area);
    app.pane_sizes
        .insert(sid.to_string(), (inner.height.max(1), inner.width.max(1)));
    app.pane_bounds.insert(
        sid.to_string(),
        (inner.x, inner.y, inner.height.max(1), inner.width.max(1)),
    );

    if pane_focused {
        app.pty_rows = inner.height.max(1);
        app.pty_cols = inner.width.max(1);
        app.pty_x = inner.x;
        app.pty_y = inner.y;
    }

    let selection = app.selection_for_pane(sid);
    if let Some(pty) = app.ptys.get(sid) {
        if let Ok(parser) = pty.parser().lock() {
            let screen = parser.screen();
            f.render_widget(TerminalView::new(screen).with_selection(selection), inner);
            if pane_focused && !ended && !screen.hide_cursor() {
                let (row, col) = screen.cursor_position();
                let cx = inner.x + col.min(inner.width.saturating_sub(1));
                let cy = inner.y + row.min(inner.height.saturating_sub(1));
                f.set_cursor_position((cx, cy));
            }
        }
    } else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "starting...",
                Style::default().fg(DIM),
            )))
            .alignment(Alignment::Center),
            inner,
        );
    }
}

fn pane_dot(app: &App, sid: &str, ended: bool) -> (&'static str, Color) {
    if ended {
        return ("○ ", Color::Rgb(150, 120, 120));
    }
    match app.session_status(sid) {
        SessionStatus::Blocked => ("● ", Color::Rgb(245, 180, 90)),
        SessionStatus::Working => ("● ", Color::Green),
        SessionStatus::Idle => ("● ", ACCENT),
        SessionStatus::Ended => ("○ ", Color::Rgb(150, 120, 120)),
        SessionStatus::Inactive => ("  ", DIM),
    }
}

fn new_session_popup(f: &mut Frame, choice: usize) {
    let area = centered(f.area(), 40, 7);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" New session ");
    let opts = ["codex", "claude", "kiro"];
    let lines: Vec<Line> = opts
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let selected = i == choice;
            let marker = if selected { "▶ " } else { "  " };
            let style = if selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(format!("{marker}{name}"), style))
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left),
        area,
    );
}

fn handoff_popup(f: &mut Frame, choice: usize, source: Option<Agent>) {
    let area = centered(f.area(), 48, 8);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Handoff ");
    let opts = [Agent::Codex, Agent::Claude, Agent::Kiro];
    let mut lines: Vec<Line> = opts
        .iter()
        .enumerate()
        .map(|(i, agent)| {
            let selected = i == choice;
            let same = source == Some(*agent);
            let marker = if selected { "▶ " } else { "  " };
            let suffix = if same { " (source)" } else { "" };
            let mut style = if same {
                Style::default().fg(DIM)
            } else {
                Style::default()
            };
            if selected {
                style = style.fg(ACCENT).add_modifier(Modifier::BOLD);
            }
            Line::from(Span::styled(
                format!("{marker}{}{suffix}", agent.as_str()),
                style,
            ))
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "enter start with handoff prompt   esc cancel",
        Style::default().fg(DIM),
    )));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left),
        area,
    );
}

fn orchestration_popup(f: &mut Frame, draft: &orchestration::Draft) {
    let area = centered(f.area(), 88, 19);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Orchestration ");
    let step_style = |step| {
        if draft.step == step {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        }
    };
    let skill = if draft.skill.is_empty() {
        ""
    } else {
        &draft.skill
    };
    let skill_display = if skill.is_empty() {
        "mode, $ralplan, $analyze ...".to_string()
    } else {
        truncate(skill, 60)
    };
    let instruction = if draft.instruction.is_empty() {
        ""
    } else {
        &draft.instruction
    };
    let provider_choice = |provider, label: &str| {
        if draft.provider == provider {
            Span::styled(
                format!("[{label}] "),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!("{label} "), Style::default().fg(DIM))
        }
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("1 provider ", step_style(orchestration::Step::Provider)),
            provider_choice(orchestration::Provider::ClaudeCode, "cc"),
            provider_choice(orchestration::Provider::Codex, "codex"),
            provider_choice(orchestration::Provider::Kiro, "kiro"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("2 skill / mode ", step_style(orchestration::Step::Skill)),
            Span::raw(skill_display),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "3 instruction",
            step_style(orchestration::Step::Instruction),
        )]),
    ];
    lines.extend(textarea_lines(
        instruction,
        "Paste or type English/Korean instructions here.",
        6,
        78,
    ));
    lines.extend([
        Line::from(""),
        Line::from(vec![
            Span::styled("4 child lanes ", step_style(orchestration::Step::Children)),
            Span::raw(format!("{}", draft.children)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "1/2/3 provider   enter next/start   ctrl-j or shift/alt-enter newline   paste keeps newlines   esc cancel",
            Style::default().fg(DIM),
        )),
    ]);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn broadcast_popup(f: &mut Frame, draft: &orchestration::BroadcastDraft) {
    let area = centered(f.area(), 88, 14);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Broadcast cycle ");
    let mut lines = vec![Line::from(Span::styled(
        "Instruction for all child lanes",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))];
    lines.extend(textarea_lines_with_cursor(
        &draft.instruction,
        Some(draft.cursor),
        "Paste or type the next cycle instruction.",
        8,
        78,
    ));
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "enter broadcast   ctrl-j or shift/alt-enter newline   paste keeps newlines   esc cancel",
            Style::default().fg(DIM),
        )),
    ]);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn dispatch_popup(f: &mut Frame, draft: &orchestration::BroadcastDraft) {
    let area = centered(f.area(), 88, 14);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Main dispatch ");
    let mut lines = vec![Line::from(Span::styled(
        "Topic for main to route across child lanes",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))];
    lines.extend(textarea_lines_with_cursor(
        &draft.instruction,
        Some(draft.cursor),
        "Paste or type the dispatch topic for the main lane.",
        8,
        78,
    ));
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "enter ask main   ctrl-j or shift/alt-enter newline   M apply after main answers   esc cancel",
            Style::default().fg(DIM),
        )),
    ]);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn dispatch_apply_popup(f: &mut Frame, draft: &orchestration::BroadcastDraft) {
    let area = centered(f.area(), 92, 18);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Apply dispatch ");
    let mut lines = vec![Line::from(Span::styled(
        "Paste MINDPLAYER_DISPATCH block",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))];
    lines.extend(textarea_lines_with_cursor(
        &draft.instruction,
        Some(draft.cursor),
        "Paste main's MINDPLAYER_DISPATCH block here.",
        12,
        82,
    ));
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "enter apply   ctrl-j or shift/alt-enter newline   paste keeps newlines   esc cancel",
            Style::default().fg(DIM),
        )),
    ]);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn help_popup(f: &mut Frame) {
    let area = centered(f.area(), 92, 24);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Keyboard shortcuts ");
    let section = |title: &'static str| {
        Line::from(Span::styled(
            title,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
    };
    let item = |key: &'static str, text: &'static str| {
        Line::from(vec![
            Span::styled(
                format!("{key:<14}"),
                Style::default().fg(Color::Rgb(190, 196, 210)),
            ),
            Span::raw(text),
        ])
    };
    let lines = vec![
        section("Session"),
        item(
            "enter",
            "open the selected session, adding it to the live view (launch all marked in multi-select)",
        ),
        item(
            "v",
            "toggle multi-select mode (then space marks, enter launches all)",
        ),
        item("space", "mark/unmark session — multi-select mode only"),
        item("n", "start a new session"),
        item("h", "handoff selected session to another provider"),
        item("e", "edit selected session label"),
        item("x", "close/archive selected session"),
        Line::from(""),
        section("Orchestration"),
        item("o", "start an orchestration group"),
        item("b", "broadcast same instruction to all child lanes"),
        item("m", "ask main to route work to selected child lanes"),
        item("M", "paste and apply main dispatch block to child lanes"),
        item("p", "run child peer-review cycle"),
        item("s", "send synthesis prompt to main lane"),
        Line::from(""),
        section("View"),
        item("/", "search visible sessions"),
        item("d", "change working directory scope"),
        item("a", "toggle archived sessions"),
        item("g", "toggle subagent sessions"),
        item("r", "rescan sessions"),
        Line::from(""),
        section("Terminal / Modal"),
        item("ctrl-x", "return from terminal to session list"),
        item("tab / shift-tab", "cycle live panes (when 2+ open)"),
        item("ctrl-w", "cycle live panes (always)"),
        item("ctrl-z", "zoom the focused pane full-size (toggle back to split)"),
        item("ctrl-o", "toggle pane layout (horizontal/vertical)"),
        item("ctrl-q", "close focused pane"),
        item("ctrl-j", "insert newline in text modals"),
        item("shift/alt-enter", "insert newline in text modals"),
        item("esc", "cancel modal or close this help"),
        item("?", "show or close this help"),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        area,
    );
}

// --- helpers --------------------------------------------------------------

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

fn textarea_lines(text: &str, placeholder: &str, rows: usize, width: usize) -> Vec<Line<'static>> {
    textarea_lines_with_cursor(text, None, placeholder, rows, width)
}

fn textarea_lines_with_cursor(
    text: &str,
    cursor: Option<usize>,
    placeholder: &str,
    rows: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let content: Vec<String> = if text.is_empty() {
        if cursor.is_some() {
            vec!["▏".to_string()]
        } else {
            vec![placeholder.to_string()]
        }
    } else {
        wrapped_text_rows(&text_with_cursor(text, cursor), width)
    };
    let cursor_row = cursor
        .and_then(|_| content.iter().position(|row| row.contains('▏')))
        .unwrap_or(0);
    let start = viewport_start_for_cursor(content.len(), rows, cursor_row);
    for row in 0..rows {
        let body = content.get(start + row).map(String::as_str).unwrap_or("");
        let style = if text.is_empty() && cursor.is_none() {
            Style::default().fg(DIM)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled("  │ ", Style::default().fg(DIM)),
            Span::styled(body.to_string(), style),
        ]));
    }
    if content.len() > rows {
        let above = start;
        let below = content.len().saturating_sub(start + rows);
        let summary = match (above, below) {
            (0, below) => format!("  └ +{below} more lines"),
            (above, 0) => format!("  ┌ +{above} earlier lines"),
            (above, below) => format!("  ├ +{above} earlier, +{below} more"),
        };
        lines.push(Line::from(Span::styled(summary, Style::default().fg(DIM))));
    }
    lines
}

fn viewport_start_for_cursor(total: usize, rows: usize, cursor_row: usize) -> usize {
    if total <= rows {
        return 0;
    }
    let half = rows / 2;
    let max_start = total.saturating_sub(rows);
    cursor_row.saturating_sub(half).min(max_start)
}

fn text_with_cursor(text: &str, cursor: Option<usize>) -> String {
    let Some(mut cursor) = cursor else {
        return text.to_string();
    };
    if cursor > text.len() {
        cursor = text.len();
    }
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let mut out = String::with_capacity(text.len() + "▏".len());
    out.push_str(&text[..cursor]);
    out.push('▏');
    out.push_str(&text[cursor..]);
    out
}

fn wrapped_text_rows(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for raw_line in text.split('\n') {
        let mut current = String::new();
        let mut current_width = 0usize;
        for ch in raw_line.chars() {
            let ch_width = display_width(ch);
            if current_width > 0 && current_width + ch_width > width {
                rows.push(current);
                current = String::new();
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }
        rows.push(current);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn display_width(ch: char) -> usize {
    let code = ch as u32;
    if ch == '\t' {
        return 4;
    }
    if code < 0x20 || (0x7f..=0x9f).contains(&code) || is_combining_mark(code) {
        return 0;
    }
    if is_wide_char(code) {
        2
    } else {
        1
    }
}

fn is_combining_mark(code: u32) -> bool {
    matches!(
        code,
        0x0300..=0x036f
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20ff
            | 0xfe20..=0xfe2f
    )
}

fn is_wide_char(code: u32) -> bool {
    matches!(
        code,
        0x1100..=0x115f
            | 0x231a..=0x231b
            | 0x2329..=0x232a
            | 0x23e9..=0x23ec
            | 0x23f0
            | 0x23f3
            | 0x25fd..=0x25fe
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267f
            | 0x2693
            | 0x26a1
            | 0x26aa..=0x26ab
            | 0x26bd..=0x26be
            | 0x26c4..=0x26c5
            | 0x26ce
            | 0x26d4
            | 0x26ea
            | 0x26f2..=0x26f3
            | 0x26f5
            | 0x26fa
            | 0x26fd
            | 0x2705
            | 0x270a..=0x270b
            | 0x2728
            | 0x274c
            | 0x274e
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27b0
            | 0x27bf
            | 0x2b1b..=0x2b1c
            | 0x2b50
            | 0x2b55
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1f64f
            | 0x1f900..=0x1f9ff
            | 0x20000..=0x3fffd
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

fn cwd_leaf(cwd: &Path) -> String {
    cwd.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| cwd.display().to_string())
}

/// Compact "time since last active": now, 5m, 2h, 3d, 4w, 6mo, 2y.
fn relative_time(t: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(t) = t else {
        return "—".to_string();
    };
    let secs = (now - t).num_seconds().max(0);
    match secs {
        s if s < 60 => "now".to_string(),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s if s < 86_400 * 7 => format!("{}d", s / 86_400),
        s if s < 86_400 * 30 => format!("{}w", s / (86_400 * 7)),
        s if s < 86_400 * 365 => format!("{}mo", s / (86_400 * 30)),
        s => format!("{}y", s / (86_400 * 365)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_leaf_uses_last_path_component() {
        assert_eq!(cwd_leaf(Path::new("/Users/alex/project")), "project");
        assert_eq!(cwd_leaf(Path::new("/")), "/");
    }

    #[test]
    fn textarea_wraps_long_lines_instead_of_truncating() {
        assert_eq!(
            wrapped_text_rows("/Users/alex/Work/project", 8),
            vec!["/Users/a", "lex/Work", "/project"]
        );
        assert_eq!(
            wrapped_text_rows("first\nsecond line", 6),
            vec!["first", "second", " line"]
        );
        assert_eq!(wrapped_text_rows("한글abc", 6), vec!["한글ab", "c"]);
        assert_eq!(wrapped_text_rows("다시 개발해", 8), vec!["다시 개", "발해"]);
    }

    #[test]
    fn textarea_width_counts_cjk_as_double_width() {
        assert_eq!(display_width('a'), 1);
        assert_eq!(display_width('한'), 2);
        assert_eq!(display_width('界'), 2);
    }

    #[test]
    fn textarea_inserts_visible_cursor_marker() {
        assert_eq!(text_with_cursor("review", Some(2)), "re▏view");
        assert_eq!(
            wrapped_text_rows(&text_with_cursor("한글abc", Some(7)), 6),
            vec!["한글a▏", "bc"]
        );
    }

    #[test]
    fn textarea_viewport_follows_cursor_row() {
        assert_eq!(viewport_start_for_cursor(8, 4, 7), 4);
        let lines =
            textarea_lines_with_cursor("1\n2\n3\n4\n5", Some("1\n2\n3\n4\n5".len()), "", 3, 20);
        let rendered = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("5▏")));
        assert!(!rendered.iter().any(|line| line.contains("1")));
    }

    fn body() -> Rect {
        Rect {
            x: 0,
            y: 1,
            width: 120,
            height: 40,
        }
    }

    #[test]
    fn single_pane_fills_the_body() {
        let rects = compute_pane_rects(body(), 1, PaneLayout::Single);
        assert_eq!(rects, vec![body()]);
        let rects = compute_pane_rects(body(), 1, PaneLayout::Horizontal);
        assert_eq!(rects, vec![body()]);
    }

    #[test]
    fn two_panes_split_horizontally_without_gap() {
        let area = body();
        let rects = compute_pane_rects(area, 2, PaneLayout::Horizontal);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].x, area.x);
        assert_eq!(rects[0].y, area.y);
        assert_eq!(rects[0].height, area.height);
        assert_eq!(rects[1].height, area.height);
        assert_eq!(rects[0].x + rects[0].width, rects[1].x);
        assert_eq!(rects[0].width + rects[1].width, area.width);
    }

    #[test]
    fn two_panes_split_vertically_without_gap() {
        let area = body();
        let rects = compute_pane_rects(area, 2, PaneLayout::Vertical);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].x, area.x);
        assert_eq!(rects[0].width, area.width);
        assert_eq!(rects[1].width, area.width);
        assert_eq!(rects[0].y + rects[0].height, rects[1].y);
        assert_eq!(rects[0].height + rects[1].height, area.height);
    }

    #[test]
    fn three_panes_tile_the_body() {
        let area = body();
        for layout in [PaneLayout::Horizontal, PaneLayout::Vertical] {
            let rects = compute_pane_rects(area, 3, layout);
            assert_eq!(rects.len(), 3);
            match layout {
                PaneLayout::Horizontal => {
                    assert_eq!(rects[0].x + rects[0].width, rects[1].x);
                    assert_eq!(rects[1].x + rects[1].width, rects[2].x);
                    assert_eq!(rects.iter().map(|r| r.width).sum::<u16>(), area.width);
                    assert!(rects.iter().all(|r| r.height == area.height));
                }
                PaneLayout::Vertical => {
                    assert_eq!(rects[0].y + rects[0].height, rects[1].y);
                    assert_eq!(rects[1].y + rects[1].height, rects[2].y);
                    assert_eq!(rects.iter().map(|r| r.height).sum::<u16>(), area.height);
                    assert!(rects.iter().all(|r| r.width == area.width));
                }
                PaneLayout::Single => unreachable!(),
            }
        }
    }

    /// Every cell of an `n`-pane grid stays inside `area`, has no zero-size
    /// pane, and the cells cover `area` exactly (no gaps / no overlap, checked
    /// via summed cell area) — for both split layouts.
    fn assert_tiles_exactly(area: Rect, n: usize) {
        for layout in [PaneLayout::Horizontal, PaneLayout::Vertical] {
            let rects = compute_pane_rects(area, n, layout);
            assert_eq!(rects.len(), n, "all {n} panes get a rect");
            for r in &rects {
                assert!(r.x >= area.x && r.x + r.width <= area.x + area.width);
                assert!(r.y >= area.y && r.y + r.height <= area.y + area.height);
                assert!(r.width > 0 && r.height > 0, "no zero-size pane");
            }
            let covered: u32 = rects
                .iter()
                .map(|r| u32::from(r.width) * u32::from(r.height))
                .sum();
            assert_eq!(covered, u32::from(area.width) * u32::from(area.height));
        }
    }

    #[test]
    fn six_panes_tile_without_gaps_or_overlap() {
        assert_tiles_exactly(body(), 6);
    }

    #[test]
    fn twenty_panes_tile_without_gaps_or_overlap() {
        // A real orchestration thread routinely accumulates this many lanes —
        // the grid must still generalize cleanly past the hand-tuned 1-6 cases.
        assert_tiles_exactly(body(), 20);
    }

    #[test]
    fn pane_rects_clamp_to_max_panes() {
        // Asking for more than MAX_PANES never yields more rects than the cap.
        let rects = compute_pane_rects(body(), MAX_PANES + 3, PaneLayout::Horizontal);
        assert_eq!(rects.len(), MAX_PANES);
    }
}
