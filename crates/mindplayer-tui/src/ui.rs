//! Rendering for every screen. `render` also records the right-pane size back
//! into `App` so the PTY can be spawned/resized at the correct dimensions.

use crate::app::{App, Focus, Screen, SessionStatus};
use crate::mascot;
use crate::terminal_view::TerminalView;
use chrono::{DateTime, Utc};
use mindplayer_core::tokens::human_tokens;
use mindplayer_core::Agent;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

const ACCENT: Color = Color::Rgb(126, 162, 247);
const DIM: Color = Color::Rgb(140, 146, 158);
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

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
fn draw_mascot(f: &mut Frame, area: Rect, tick: usize, custom: Option<&mascot::Sprite>) {
    // A custom sprite may be larger than the built-in 16×16.
    let (w, h) = match custom {
        Some(s) => (s.cell_width(), s.cell_height()),
        None => (mascot::WIDTH, mascot::HEIGHT),
    };
    if area.width < w || area.height < h {
        return;
    }
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y,
        width: w,
        height: h,
    };
    let lines = match custom {
        Some(sprite) => sprite.lines(tick),
        None => mascot::lines(tick),
    };
    f.render_widget(Paragraph::new(lines), r);
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
    draw_mascot(f, chunks[1], app.spinner, app.mascot.as_ref());

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

    draw_mascot(f, chunks[1], app.spinner, app.mascot.as_ref());
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

    let keys = match app.focus {
        Focus::List => {
            "↑↓ move  enter open  n new  d dir  e label  x close  a archived  g sub  r rescan  q quit"
        }
        Focus::Terminal => "ctrl-x back to list   wheel scrolls history   shift+drag to copy",
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

    if let Some(choice) = app.new_picker {
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
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Sessions · {tab}{subs} ({}) ", app.visible.len()))
        .border_style(Style::default().fg(if focused { ACCENT } else { DIM }));

    // Fill the pane: title gets whatever width is left after border, status
    // badge, tag, time, and token columns (~34 cols of fixed chrome).
    let max_title = (area.width as usize).saturating_sub(34).max(12);
    let items: Vec<ListItem> = (0..app.visible.len())
        .filter_map(|row| app.session_at(row))
        .map(|s| {
            let (tag, tag_color) = match s.agent {
                Agent::Codex => ("codex ", ACCENT),
                Agent::Claude => ("claude", Color::Magenta),
                Agent::Kiro => ("kiro  ", Color::Cyan),
            };
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
            ListItem::new(Line::from(vec![
                Span::styled(format!("{badge} "), badge_style),
                Span::styled(
                    format!("{tag} "),
                    Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                ),
                // last-active recency: the list is sorted newest-first, so this
                // column descends from top to bottom.
                Span::styled(
                    format!("{:>4} ", relative_time(s.last_active, now)),
                    Style::default().fg(DIM),
                ),
                Span::raw(truncate(&s.title, max_title)),
                Span::styled(
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
                ),
            ]))
        })
        .collect();

    // Record the visible row count (inside the borders) so PageUp/PageDown can
    // step by a screenful.
    app.list_rows = area.height.saturating_sub(2);

    let mut state = ListState::default();
    if !app.visible.is_empty() {
        state.select(Some(app.selected));
    }
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
    let region_top = content_top + app.visible.len() as u16;
    let region_h = bottom.saturating_sub(region_top);
    const GAP: u16 = 1;
    // The custom mascot can be taller than the built-in one; size the block to it.
    let mascot_h = app
        .mascot
        .as_ref()
        .map_or(mascot::HEIGHT, |s| s.cell_height());
    let block_h = mascot_h + GAP + 2; // mascot + gap + tagline + legend
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
                height: mascot_h,
            },
            app.spinner,
            app.mascot.as_ref(),
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
                y: top + mascot_h + GAP,
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
                y: top + mascot_h + GAP + 1,
                width: inner_w,
                height: 1,
            },
        );
    }
}

fn live_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Terminal;
    let active_id = app.active.clone();
    let ended = app.active_ended();
    let live_count = app.ptys.len();
    let count_suffix = if live_count > 1 {
        format!(" [{live_count} live]")
    } else {
        String::new()
    };
    let title = match (active_id.as_deref(), app.selected_session()) {
        (Some(id), _) if ended => format!(" Live · {} (ended){count_suffix} ", short(id)),
        (Some(id), _) => format!(" Live · {}{count_suffix} ", short(id)),
        (None, Some(s)) => format!(" Live · {} (enter to resume) ", short(&s.id)),
        (None, None) => " Live ".to_string(),
    };
    // Top/bottom borders only: a left/right `│` sits in real screen cells next
    // to the content, so a terminal Shift+drag selection grabs it on every line.
    // Dropping the side borders lets the PTY content reach the edges, so copying
    // yields just the content (top title + bottom rule still frame the pane).
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .title(title)
        .border_style(Style::default().fg(if focused { ACCENT } else { DIM }));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Record pane size so the PTY can be spawned/resized to match.
    app.pty_rows = inner.height.max(1);
    app.pty_cols = inner.width.max(1);
    app.pty_x = inner.x;
    app.pty_y = inner.y;

    let rendered = if let Some(pty) = app.active_pty() {
        if let Ok(parser) = pty.parser().lock() {
            let screen = parser.screen();
            f.render_widget(TerminalView::new(screen), inner);
            // Place the real terminal cursor at the PTY's cursor so the macOS
            // IME composition popup (Korean/CJK) appears in the right spot and
            // the cursor is visible while typing. Without this the IME preedit
            // shows at the top-left and CJK input feels broken.
            if focused && !ended && !screen.hide_cursor() {
                let (row, col) = screen.cursor_position();
                let cx = inner.x + col.min(inner.width.saturating_sub(1));
                let cy = inner.y + row.min(inner.height.saturating_sub(1));
                f.set_cursor_position((cx, cy));
            }
        }
        true
    } else {
        false
    };

    if !rendered {
        let mascot_h = app
            .mascot
            .as_ref()
            .map_or(mascot::HEIGHT, |s| s.cell_height());
        draw_mascot(f, inner, app.spinner, app.mascot.as_ref());
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
            y: inner.y + mascot_h.min(inner.height),
            width: inner.width,
            height: inner.height.saturating_sub(mascot_h),
        };
        f.render_widget(hint, hint_area);
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
