//! Rendering for every screen. `render` also records the right-pane size back
//! into `App` so the PTY can be spawned/resized at the correct dimensions.

use crate::app::{App, CategoryMenu, Focus, PaneLayout, Row, Screen, SessionStatus, MAX_PANES};
use crate::mascot;
use crate::terminal_view::TerminalView;
use crate::text_input;
use crate::walker;
use chrono::{DateTime, Utc};
use mindplayer_core::tokens::human_tokens;
use mindplayer_core::{Agent, Session};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::Frame;
use std::path::{Path, PathBuf};

const ACCENT: Color = Color::Rgb(126, 162, 247);
const DIM: Color = Color::Rgb(140, 146, 158);
// A quiet teal, not ACCENT — the idle-status dot used to share ACCENT with
// the focus border/selection highlight, so an idle+focused/selected session
// had no color-based way to tell "this is idle" apart from "this is focused."
const IDLE: Color = Color::Rgb(111, 154, 149);
/// Category headers — distinct from ACCENT so a topic never reads as a status.
const CATEGORY: Color = Color::Rgb(180, 142, 240);
// Orchid — reserved for the manual "in progress" mark so it never gets
// mistaken for a live-status color (blocked/working/idle/done all sit in the
// amber/green/teal/rose range).
const IN_PROGRESS: Color = Color::Rgb(201, 166, 255);
// A distinct gold, close to but not the same as the Blocked status amber
// (245, 180, 90) — zoom is a view mode, not a session status, and the two can
// legitimately show on the same pane at once (a zoomed, blocked session).
const ZOOM: Color = Color::Rgb(235, 160, 70);
// A bright cyan for the HTML-preview badge, distinct from ACCENT (soft blue),
// ZOOM (gold), DIM (gray), and every status color (amber/green/teal/rose).
const PREVIEW: Color = Color::Rgb(82, 196, 214);
// Rose used for the inline error line inside the preview popup.
const ERROR: Color = Color::Rgb(245, 130, 120);
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

fn agent_tag(agent: Agent) -> (&'static str, Color) {
    match agent {
        Agent::Codex => ("codex ", ACCENT),
        Agent::Claude => ("claude", Color::Magenta),
        Agent::Kiro => ("kiro  ", Color::Cyan),
        Agent::Cursor => ("cursor", Color::Rgb(242, 170, 76)),
    }
}

/// Cells in an account's remaining-quota gauge.
const QUOTA_GAUGE_CELLS: usize = 6;

/// The colour a used-percentage earns. Three steps rather than a gradient, so
/// it still reads on a 16-colour terminal and nobody has to judge a hue.
fn quota_tier(used: f64) -> Color {
    if used <= 70.0 {
        Color::Rgb(125, 187, 132)
    } else if used <= 90.0 {
        ZOOM
    } else {
        ERROR
    }
}

fn quota_label_color(label: &str) -> Color {
    match label.split_whitespace().next().unwrap_or("") {
        "codex" => ACCENT,
        "claude" => Color::Magenta,
        "kiro" => Color::Cyan,
        _ => Color::Rgb(180, 142, 240),
    }
}

/// One footer row per account window: name, gauge, percentage, then the figure
/// behind it. A row whose provider reported no window gets an em dash where the
/// gauge would be — an empty gauge would read as "plenty left".
fn quota_row_spans(
    row: &mindplayer_core::limits::QuotaRow,
    name_width: usize,
) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        format!("  {:<name_width$} ", row.label),
        Style::default().fg(quota_label_color(&row.label)),
    )];
    match row.used_percent {
        Some(used) => {
            let filled = ((used / 100.0) * QUOTA_GAUGE_CELLS as f64).round() as usize;
            let filled = filled.min(QUOTA_GAUGE_CELLS);
            let tier = quota_tier(used);
            spans.push(Span::styled(
                format!(
                    "{}{}",
                    "▰".repeat(filled),
                    "▱".repeat(QUOTA_GAUGE_CELLS - filled)
                ),
                Style::default().fg(tier),
            ));
            spans.push(Span::styled(
                format!("  {used:>5.1}%"),
                Style::default().fg(tier).add_modifier(Modifier::BOLD),
            ));
        }
        None => spans.push(Span::styled(
            format!("{:<width$}        ", "—", width = QUOTA_GAUGE_CELLS),
            Style::default().fg(DIM),
        )),
    }
    if !row.detail.is_empty() {
        spans.push(Span::styled(
            format!("  {}", row.detail),
            Style::default().fg(if row.used_percent.is_some_and(|used| used >= 100.0) {
                ERROR
            } else {
                DIM
            }),
        ));
    }
    if let Some(resets) = row.resets.as_deref() {
        spans.push(Span::styled(
            format!("  resets {resets}"),
            Style::default().fg(DIM),
        ));
    }
    spans
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
            "  Codex / Claude / Kiro / Cursor session manager",
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

/// The category menu (`t` on a header): auto-sync, sync now, rename, remove.
fn category_menu_popup(f: &mut Frame, app: &App) {
    let Some(menu) = app.category_menu.as_ref() else {
        return;
    };
    let name = app.category_label(&menu.cat_id);
    let title = format!(" {name} ");

    // Renaming replaces the list — one thing on screen at a time.
    if let Some(buf) = &menu.rename {
        let area = centered(f.area(), 50, 5);
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled("Category name:", Style::default().fg(DIM))),
                Line::from(Span::styled(
                    format!("{buf}▏"),
                    Style::default().fg(CATEGORY).add_modifier(Modifier::BOLD),
                )),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(CATEGORY))
                    .title(title),
            ),
            area,
        );
        return;
    }

    let on = app.state.category_auto_sync(&menu.cat_id);
    let rows: [(&str, String); 4] = [
        ("auto-sync", (if on { "on" } else { "off" }).to_string()),
        ("sync now", String::new()),
        ("rename…", String::new()),
        (
            if menu.confirm_remove {
                "remove category — enter to confirm"
            } else {
                "remove category"
            },
            String::new(),
        ),
    ];
    let area = centered(f.area(), 46, rows.len() as u16 + 2);
    f.render_widget(Clear, area);
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, (label, value))| {
            let selected = i == menu.selected;
            let marker = if selected { "▶ " } else { "  " };
            let mut style = if selected {
                Style::default().fg(CATEGORY).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            };
            if i == CategoryMenu::REMOVE && menu.confirm_remove {
                style = style.fg(Color::Rgb(217, 112, 112));
            }
            let mut spans = vec![Span::styled(format!("{marker}{label}"), style)];
            if !value.is_empty() {
                spans.push(Span::styled(
                    format!("        {value}"),
                    Style::default().fg(if on { Color::Green } else { DIM }),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(CATEGORY))
                .title(title),
        ),
        area,
    );
}

/// The `t` category picker: existing topics, then create/remove. Shows how many
/// sessions the pick will apply to, since multi-select can target many at once.
fn category_popup(f: &mut Frame, app: &App) {
    let Some(picker) = app.category_picker.as_ref() else {
        return;
    };
    let rows = app.category_picker_rows();
    let title = if picker.targets.len() > 1 {
        format!(" Category for {} sessions ", picker.targets.len())
    } else {
        " Category ".to_string()
    };

    // Typing a new name replaces the list — one thing to look at at a time.
    if let Some(name) = &picker.new_name {
        let area = centered(f.area(), 54, 5);
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled("New category name:", Style::default().fg(DIM))),
                Line::from(Span::styled(
                    format!("{name}▏"),
                    Style::default().fg(CATEGORY).add_modifier(Modifier::BOLD),
                )),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(CATEGORY))
                    .title(title),
            ),
            area,
        );
        return;
    }

    let h = (rows.len() as u16 + 2).min(f.area().height);
    let area = centered(f.area(), 54, h);
    f.render_widget(Clear, area);
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, (id, label))| {
            let selected = i == picker.selected;
            let marker = if selected { "▶ " } else { "  " };
            // Mark where the target already sits, so re-opening reads as a state
            // view rather than a blank choice.
            let current = id.as_deref().is_some_and(|id| {
                picker
                    .targets
                    .first()
                    .and_then(|t| app.state.category_of(t))
                    == Some(id)
            });
            let mut style = if selected {
                Style::default().fg(CATEGORY).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            };
            if id.is_none() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            let count = id
                .as_deref()
                .map(|id| app.category_session_count(Some(id)))
                .unwrap_or(0);
            let suffix = match (id.is_some(), current) {
                (true, true) => format!("  · {count}  (current)"),
                (true, false) => format!("  · {count}"),
                _ => String::new(),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker}{label}"), style),
                Span::styled(suffix, Style::default().fg(DIM)),
            ]))
        })
        .collect();
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(CATEGORY))
                .title(title),
        ),
        area,
    );
}

/// Block-letter "MINDPLAYER" for the startup screen. A terminal can't scale its
/// font, so making the title bigger means drawing it out of block glyphs — this
/// is a fixed wordmark rather than a general font, since it only ever spells the
/// one word. 5 rows tall, [`WORDMARK_W`] columns wide.
const WORDMARK: [&str; 5] = [
    "█   █ ███ █   █ ███  ███  █     ███  █   █ ████ ███ ",
    "██ ██  █  ██  █ █  █ █  █ █    █   █  █ █  █    █  █",
    "█ █ █  █  █ █ █ █  █ ███  █    █████   █   ███  ███ ",
    "█   █  █  █  ██ █  █ █    █    █   █   █   █    █ █ ",
    "█   █ ███ █   █ ███  █    ████ █   █   █   ████ █  █",
];
const WORDMARK_W: u16 = 52;
const WORDMARK_H: u16 = WORDMARK.len() as u16;

/// Draw the wordmark horizontally centered in `area` at row `y`. Skipped when
/// the area is too narrow to hold it, so a small terminal degrades to the plain
/// title bar instead of a chopped-up logo.
fn draw_wordmark(f: &mut Frame, area: Rect, y: u16) {
    if area.width < WORDMARK_W {
        return;
    }
    let lines: Vec<Line<'static>> = WORDMARK
        .iter()
        .map(|row| Line::from(Span::styled(*row, Style::default().fg(ACCENT))))
        .collect();
    f.render_widget(
        Paragraph::new(lines),
        Rect {
            x: area.x + (area.width - WORDMARK_W) / 2,
            y,
            width: WORDMARK_W,
            height: WORDMARK_H,
        },
    );
}

/// Draw the walking-character strip across the full width of `area`, anchored
/// at its top. Unlike the fixed-size mascot this uses whatever width it's
/// given, so the character has the whole row to walk; `walker::lines` returns
/// nothing when that width is too small and we simply skip the draw.
fn draw_walker(f: &mut Frame, area: Rect, app: &App) {
    if area.height < walker::HEIGHT {
        return;
    }
    let lines = walker::lines(app.walker(), app.spinner, area.width);
    if lines.is_empty() {
        return;
    }
    f.render_widget(
        Paragraph::new(lines),
        Rect {
            height: walker::HEIGHT,
            ..area
        },
    );
}

/// The scope screen's character picker: every character shown as a live
/// portrait next to its name, so you pick by looking rather than by guessing
/// from a label.
fn walker_picker(f: &mut Frame, area: Rect, app: &App, highlighted: usize) {
    let rows_per = (walker::SPRITE_H / 2) as u16;
    let h = walker::ALL.len() as u16 * rows_per + 2; // +2 for the border
    let w = 34u16;
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT))
            .title(" Pick your walking buddy "),
        popup,
    );
    let inner = Rect {
        x: popup.x + 1,
        y: popup.y + 1,
        width: popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    };
    // Frame 0 vs 1 alternates on the same cadence the strip walks at, so the
    // portraits are alive without drifting out of step with the character
    // below them.
    let frame = (app.spinner / 6) % 2;
    for (i, ch) in walker::ALL.iter().enumerate() {
        let top = inner.y + i as u16 * rows_per;
        if top + rows_per > inner.y + inner.height {
            break;
        }
        let selected = i == highlighted;
        let portrait = walker::portrait(ch, frame);
        f.render_widget(
            Paragraph::new(portrait),
            Rect {
                x: inner.x + 2,
                y: top,
                width: walker::SPRITE_W as u16,
                height: rows_per,
            },
        );
        let label_style = if selected {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        let marker = if selected { "▶ " } else { "  " };
        // Vertically center the label against the portrait block.
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{marker}{}", ch.name),
                label_style,
            ))),
            Rect {
                x: inner.x + 2 + walker::SPRITE_W as u16 + 1,
                y: top + rows_per / 2,
                width: inner.width.saturating_sub(walker::SPRITE_W as u16 + 5),
                height: 1,
            },
        );
    }
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

    // Box sized to its contents: two borders plus one row per option. The old
    // fixed height of 8 left four dead rows inside, which read as a tiny box
    // stranded in a big empty screen.
    let box_h = options.len() as u16 + 2;
    const GAP: u16 = 1;
    // Wordmark + gap + box, centered as one block so the pair reads together.
    let group_h = WORDMARK_H + GAP + box_h;
    let (wordmark_y, inner) = if chunks[1].height >= group_h {
        let top = chunks[1].y + (chunks[1].height - group_h) / 2;
        (
            Some(top),
            Rect {
                y: top + WORDMARK_H + GAP,
                ..centered(chunks[1], 70, box_h)
            },
        )
    } else {
        // Too short for the wordmark — just center the box on its own.
        (None, centered(chunks[1], 70, box_h))
    };

    if let Some(y) = wordmark_y {
        draw_wordmark(f, chunks[1], y);
    }

    // The character walks along the floor of this screen — the strip is pinned
    // to the bottom of the region, just above the footer. The box is centered
    // in the same region, so only draw the strip when it clears the box's
    // bottom edge; on a short terminal it's dropped rather than overlapped.
    let strip_y = chunks[1].y + chunks[1].height.saturating_sub(walker::HEIGHT);
    if strip_y >= inner.y + inner.height {
        draw_walker(
            f,
            Rect {
                y: strip_y,
                height: walker::HEIGHT,
                ..chunks[1]
            },
            app,
        );
    }
    f.render_widget(List::new(items).block(block), inner);

    footer(
        chunks[2],
        f,
        "↑↓ choose   enter scan   c character   q quit",
    );

    // Drawn last so it sits above both the strip and the scope list.
    if let Some(highlighted) = app.walker_picker {
        walker_picker(f, chunks[1], app, highlighted);
        footer(chunks[2], f, "↑↓ pick   enter select   esc cancel");
    }
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

    let spin = SPINNER[app.spinner % SPINNER.len()];
    let area = centered(chunks[1], 60, 5);
    // See the matching guard in scope_select: bottom-anchored, and skipped on a
    // terminal too small to clear this centered box without overlap.
    let strip_y = chunks[1].y + chunks[1].height.saturating_sub(walker::HEIGHT);
    if strip_y >= area.y + area.height {
        draw_walker(
            f,
            Rect {
                y: strip_y,
                height: walker::HEIGHT,
                ..chunks[1]
            },
            app,
        );
    }
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
            "reading ~/.codex, ~/.claude, ~/.kiro, and ~/.cursor sessions",
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
            Span::styled("   cursor  ", Style::default().fg(Color::Rgb(242, 170, 76))),
            Span::raw(format!("{:>3}", a.cursor_count)),
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
                    "   (codex {} · claude {} · kiro {} · cursor {})",
                    human_tokens(a.codex.total),
                    human_tokens(a.claude.total),
                    if a.kiro_count > 0 { "—" } else { "0" },
                    if a.cursor_count > 0 { "—" } else { "0" },
                ),
                Style::default().fg(DIM),
            ),
        ])),
        rows[4],
    );

    footer(chunks[2], f, "enter open mindplayer   q quit");
}

fn main_view(f: &mut Frame, app: &mut App) {
    // The plain list view hides ~8 shortcuts (session-management and
    // view-toggle basics) behind the `?` help modal. Multi-select, search,
    // and the live-pane hints are already complete for their own mode, so
    // only the plain list gets a second footer row.
    let show_more_keys =
        app.focus == Focus::List && !app.multi_select && app.search_query.is_none();
    // Built before the layout because its width decides how tall the footer is.
    // The status carries counts, a usage bar, per-account quotas and the working
    // directory; sharing one row with the key hints truncated it mid-number, and
    // a clipped "$0.06/$20" reads as a $2 limit. It gets the full width and wraps.
    let mut status: Vec<Span> = Vec::new();
    if !app.status.is_empty() {
        status.push(Span::styled(
            format!("{}  ·  ", app.status),
            Style::default().fg(DIM),
        ));
    }
    status.push(Span::styled(app.summary_head(), Style::default().fg(DIM)));
    status.push(Span::styled(app.summary_tail(), Style::default().fg(DIM)));
    // Says so while the account rows below are the previous run's. It clears
    // itself the moment this run's own reading lands, so a stale number never
    // passes for a current one.
    if let Some(at) = app.quota_cached_at() {
        status.push(Span::styled(
            format!(
                "  ·  accounts as of {}",
                at.with_timezone(&chrono::Local).format("%H:%M")
            ),
            Style::default().fg(DIM).add_modifier(Modifier::DIM),
        ));
    }
    let status_line = Line::from(status);
    // Two rows is the ceiling: past that the footer would eat the list it exists
    // to describe, and anything still over is the scope label, which repeats
    // what the title bar already says.
    let status_h = (status_line.width() as u16)
        .div_ceil(f.area().width.max(1))
        .clamp(1, 2);
    // One row per account window. Each is gauged, so they cannot share a line
    // the way plain numbers did — and giving each its own row is what lets the
    // names align vertically enough to scan.
    let mut quota_rows = app.quota_rows();
    mindplayer_core::limits::order_for_display(&mut quota_rows, |agent| {
        app.account_for(agent).name
    });
    // Provider order is fixed, and the login a new session would take comes
    // first within each — otherwise the lines follow the order accounts were
    // added and a provider's rows end up split apart.

    // The provider becomes its own column, so the label drops the name it
    // already starts with. Two logins of one provider still need the account
    // on the line; one login says nothing extra.
    for row in &mut quota_rows {
        // Keep the footer compact; the Accounts screen retains the reported
        // usage/limit figures. The exhausted state must still be explicit.
        if row.agent == Agent::Codex && row.label == "codex monthly" {
            row.detail = if row.used_percent.is_some_and(|used| used >= 100.0) {
                "limit reached".into()
            } else {
                String::new()
            };
        }
        let mut rest = mindplayer_core::limits::label_without_provider(row).to_string();
        if !row.account.is_empty() && app.provider_has_several_logins(row.agent) {
            rest = if rest.is_empty() {
                row.account.clone()
            } else {
                format!("{} {rest}", row.account)
            };
        }
        row.label = rest;
    }
    let name_width = quota_rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0);
    let platform_width = quota_rows
        .iter()
        .map(|r| r.agent.as_str().chars().count())
        .max()
        .unwrap_or(0);
    let quota_h = quota_rows.len() as u16;
    let footer_h: u16 = status_h + quota_h + 1 + u16::from(show_more_keys);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(footer_h),
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

    let keys: String = if app.search_query.is_some() && app.multi_select {
        // Marking wins over typing for these two keys (see main.rs) — say so,
        // instead of only ever showing one of the two active modes' hints.
        "MULTI-SELECT + search · space mark · v exit multi-select · type to filter · esc exit search".to_string()
    } else if app.search_query.is_some() {
        "type to filter · enter open · ↑↓ move · esc exit search".to_string()
    } else {
        match app.focus {
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
            "ctrl-x list · tab/ctrl-w pane · ctrl-z zoom · ctrl-y links · ctrl-q close · wheel history · drag=copy this pane"
                .to_string()
        }
        }
    };
    // The status owns its own full-width row(s); the key hints get the next one.
    // The last row (Min(0), collapsing to nothing when `show_more_keys` is false)
    // surfaces a couple of the most-reached-for hidden shortcuts plus an honest
    // count of the rest, instead of hiding all of them silently.
    let footer_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(status_h),
            Constraint::Length(quota_h),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(outer[2]);
    f.render_widget(
        Paragraph::new(status_line).wrap(Wrap { trim: false }),
        footer_rows[0],
    );
    if quota_h > 0 {
        // The provider is named once per group. A blank there reads as "the
        // one above", which is what makes the lines a group at a glance.
        let mut previous: Option<Agent> = None;
        let lines: Vec<Line> = quota_rows
            .iter()
            .map(|row| {
                let head = if previous == Some(row.agent) {
                    String::new()
                } else {
                    row.agent.as_str().to_string()
                };
                previous = Some(row.agent);
                let mut spans = vec![Span::styled(
                    format!(" {head:<platform_width$} "),
                    Style::default()
                        .fg(quota_label_color(row.agent.as_str()))
                        .add_modifier(Modifier::BOLD),
                )];
                spans.extend(quota_row_spans(row, name_width));
                Line::from(spans)
            })
            .collect();
        f.render_widget(Paragraph::new(lines), footer_rows[1]);
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(keys, Style::default().fg(DIM))))
            .alignment(Alignment::Right),
        footer_rows[2],
    );
    if show_more_keys {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "i in progress · c catch-up · t category · ←→ fold    +5 more · ? help",
                Style::default().fg(Color::Rgb(90, 95, 108)),
            )))
            .alignment(Alignment::Right),
            footer_rows[3],
        );
    }

    if app.help_visible {
        help_popup(f);
    } else if app.accounts_panel.is_some() {
        accounts_popup(f, app);
    } else if app.category_menu.is_some() {
        category_menu_popup(f, app);
    } else if app.category_picker.is_some() {
        category_popup(f, app);
    } else if let Some(choice) = app.handoff_picker {
        handoff_popup(f, choice, app.selected_session().cloned().as_ref(), app);
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
    } else if let Some(picker) = &app.link_picker {
        link_picker_popup(f, picker);
    } else if let Some(choice) = app.html_preview_picker {
        html_preview_picker_popup(f, choice, app.html_candidates_for_focused());
    } else if let Some(path) = &app.html_preview_input {
        html_preview_popup(f, path, app.html_preview_error.as_deref());
    } else if let Some(id) = &app.catchup_confirm {
        let title = app
            .all_sessions
            .iter()
            .find(|s| &s.id == id)
            .map(|s| s.title.as_str())
            .unwrap_or("this session");
        catchup_confirm_popup(f, title);
    } else if let Some(input) = &app.transition_report_input {
        transition_report_popup(f, input);
    } else if let Some(draft) = &app.transition_report_review {
        transition_report_review_popup(f, draft, app.transition_report_review_editing);
    }
}

fn transition_report_popup(f: &mut Frame, input: &str) {
    let area = centered(f.area(), 70, 8);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Transition report ");
    let lines = vec![
        Line::from(Span::styled(
            "topic / RUNBOOK §n / files (free text — sent to the focused pane):",
            Style::default().fg(DIM),
        )),
        Line::from(vec![
            Span::raw(input.to_string()),
            Span::styled("▏", Style::default().fg(ACCENT)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "enter send   esc cancel",
            Style::default().fg(DIM),
        )),
    ];
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn transition_report_review_popup(
    f: &mut Frame,
    draft: &text_input::BroadcastDraft,
    editing: bool,
) {
    let area = centered(f.area(), 88, 14);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(if editing {
            " Transition report — editing "
        } else {
            " Transition report — review "
        });
    let mut lines = vec![Line::from(Span::styled(
        if editing {
            "Editing the assembled prompt:"
        } else {
            "About to send this to the focused pane:"
        },
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))];
    lines.extend(textarea_lines_with_cursor(
        &draft.instruction,
        editing.then_some(draft.cursor),
        "",
        8,
        78,
    ));
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            if editing {
                "enter send   shift/alt/ctrl-enter newline   esc cancel"
            } else {
                "enter send as-is   e edit first   esc cancel"
            },
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

fn catchup_confirm_popup(f: &mut Frame, title: &str) {
    let area = centered(f.area(), 64, 7);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Send catch-up prompt? ");
    let lines = vec![
        Line::from(Span::styled(
            "This session is still busy — the prompt will queue in behind",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            format!("its current turn: {}", truncate(title, 56)),
            Style::default(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "enter send anyway   esc cancel",
            Style::default().fg(DIM),
        )),
    ];
    f.render_widget(Paragraph::new(lines).block(block), area);
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

/// Path-input popup for opening a local `.html` file in the browser. Mirrors
/// `dir_input_popup`, but shows an inline error line inside the SAME still-open
/// popup when the last-submitted path didn't resolve — so the user can correct
/// it in place instead of losing the popup to a bottom-bar status message.
fn html_preview_popup(f: &mut Frame, path: &str, error: Option<&str>) {
    let area = centered(f.area(), 70, 8);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(PREVIEW))
        .title(" Open HTML in browser ");
    let mut lines = vec![
        Line::from(Span::styled(
            "Path to a local .html file (~ allowed):",
            Style::default().fg(DIM),
        )),
        Line::from(vec![
            Span::raw(path.to_string()),
            Span::styled("▏", Style::default().fg(PREVIEW)),
        ]),
    ];
    // Third row: the inline error when present, otherwise a blank spacer so the
    // popup's height and the hint's position stay stable.
    if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("⚠ {err}"),
            Style::default().fg(ERROR),
        )));
    } else {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "enter open   esc cancel",
        Style::default().fg(DIM),
    )));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Ranked picker of detected `.html` candidates (most-recent-first) for the
/// focused pane — what Ctrl-P opens instead of the blank path popup when the
/// passive poll has found files. Styled like `new_session_popup`/`dir_input_popup`
/// (centered box, PREVIEW-cyan border, the selected row marked with `▶` and
/// highlighted). Each row shows the filename plus its parent directory so
/// same-named files in different subdirs stay distinguishable. The hint line
/// documents `tab` as the escape hatch to the free-text path popup.
/// The "copy a link" picker. Same shape as the `.html` picker so the two read as
/// one family — only opened when an answer had two or more links.
fn link_picker_popup(f: &mut Frame, picker: &crate::app::LinkPicker) {
    const MAX_ROWS: usize = 10;
    let shown = picker.links.len().min(MAX_ROWS);
    let overflow = usize::from(picker.links.len() > MAX_ROWS);
    let height = (shown + overflow + 4).max(5) as u16;
    let area = centered(f.area(), 84, height);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(PREVIEW))
        .title(" Copy a link ");
    // Saying which answer these came from is what makes searching back
    // acceptable — otherwise the links have no provenance on screen.
    let from = match picker.turns_ago {
        0 => "the latest answer".to_string(),
        1 => "the answer before".to_string(),
        n => format!("{n} answers back"),
    };
    let mut lines = vec![Line::from(Span::styled(
        format!("Links in {from}:"),
        Style::default().fg(DIM),
    ))];
    for (i, url) in picker.links.iter().take(MAX_ROWS).enumerate() {
        let selected = i == picker.selected;
        let marker = if selected { "▶ " } else { "  " };
        let style = if selected {
            Style::default().fg(PREVIEW).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(format!("{marker}{url}"), style)));
    }
    if overflow == 1 {
        lines.push(Line::from(Span::styled(
            format!("  … +{} more", picker.links.len() - MAX_ROWS),
            Style::default().fg(DIM),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "enter copy   a copy all   ↑↓ choose   esc cancel",
        Style::default().fg(DIM),
    )));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines), inner);
}

fn html_preview_picker_popup(f: &mut Frame, choice: usize, candidates: &[PathBuf]) {
    const MAX_ROWS: usize = 10;
    let shown = candidates.len().min(MAX_ROWS);
    // header + rows (+ overflow line) + blank + hint, all inside the borders.
    let overflow = usize::from(candidates.len() > MAX_ROWS);
    let height = (shown + overflow + 4).max(5) as u16;
    let area = centered(f.area(), 84, height);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(PREVIEW))
        .title(" Open a detected .html file ");
    let mut lines = vec![Line::from(Span::styled(
        "Recently edited .html files in this session's directory:",
        Style::default().fg(DIM),
    ))];
    for (i, path) in candidates.iter().take(MAX_ROWS).enumerate() {
        let selected = i == choice;
        let marker = if selected { "▶ " } else { "  " };
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("(unnamed)");
        let parent = path.parent().and_then(Path::to_str).unwrap_or("");
        let name_style = if selected {
            Style::default().fg(PREVIEW).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{name}"), name_style),
            Span::styled(format!("  {parent}"), Style::default().fg(DIM)),
        ]));
    }
    if overflow == 1 {
        lines.push(Line::from(Span::styled(
            format!("  … +{} more", candidates.len() - MAX_ROWS),
            Style::default().fg(DIM),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "enter open   ↑↓ choose   tab type a path   esc cancel",
        Style::default().fg(DIM),
    )));
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
    // Worked out once: resolving a session's account is a path comparison per
    // account, and this list runs to thousands of rows.
    let account_marks = app.account_marks();
    let focused = app.focus == Focus::List;
    let tab = if app.show_archived {
        "archived"
    } else {
        "active"
    };
    // A trailing cursor makes search the same kind of "live text entry" as
    // every popup input, instead of the one text-entry mode with no visible
    // caret at all.
    let search = app
        .search_query
        .as_deref()
        .map(|query| format!(" · /{query}▏"))
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
            " Sessions · recent first · {tab}{search}{multi} ({}) ",
            app.visible.len()
        ));

    // Fill the pane: title gets whatever width is left after border, status
    // badge, agent, time, identity, and usage columns.
    let show_id = area.width >= 58;
    let show_cwd = area.width >= 78;
    let identity_width = usize::from(show_id) * 11 + usize::from(show_cwd) * 11;
    // 39 = status badge + agent bar/tag + time + thread prefix + the 2-col
    // multi-select mark column + the 1-col in-progress rail prepended to
    // every row.
    let max_title = (area.width as usize)
        .saturating_sub(39 + identity_width)
        .max(12);
    // Top-level categories. rebuild_visible sorts every recent group (touched
    // in the last 24h OR running live now) above the rest and records the
    // boundary in `recent_count`, so the split is position-based — the
    // headers always match the sort order and never recompute per row.
    let recent_count = app.recent_count.min(app.visible.len());
    // Rows now include category headers, so count sessions for the band labels
    // rather than rows — otherwise "recent N sessions" would count headers too.
    let recent_sessions = (0..recent_count)
        .filter(|&r| app.session_at(r).is_some())
        .count();
    let older_sessions = (recent_count..app.visible.len())
        .filter(|&r| app.session_at(r).is_some())
        .count();

    let mut items: Vec<ListItem> = Vec::new();
    let mut selected_item = None;
    let mut current_recent: Option<bool> = None;
    for row in 0..app.visible.len() {
        let Some(kind) = app.row_at(row).cloned() else {
            continue;
        };
        let is_recent = row < recent_count;
        if current_recent != Some(is_recent) {
            current_recent = Some(is_recent);
            let (label, count) = if is_recent {
                ("recent", recent_sessions)
            } else {
                ("older", older_sessions)
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
        // Category header rows: the fold marker plus a session count. Rendered
        // as a normal list item so the cursor can sit on it (that is what makes
        // `←`/`→` able to fold without stealing `→` from session rows).
        let cat: Option<Option<String>> = match &kind {
            Row::Header(cat) => Some(cat.clone()),
            Row::Session(_) => None,
        };
        if let Some(cat) = cat {
            let count = app.category_session_count(cat.as_deref());
            let (glyph, label) = match &cat {
                Some(id) => (
                    if app.state.is_collapsed(id) {
                        "▸ "
                    } else {
                        "▾ "
                    },
                    app.category_label(id),
                ),
                None => ("· ", "uncategorized".to_string()),
            };
            let name_style = if cat.is_some() {
                Style::default().fg(CATEGORY).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            };
            let mut spans = vec![
                Span::styled(format!("  {glyph}"), Style::default().fg(CATEGORY)),
                Span::styled(label, name_style),
            ];
            // A one-session topic reads as a label, not a group — no tally.
            if count != 1 {
                spans.push(Span::styled(
                    format!("  · {count} {}", plural_session(count)),
                    Style::default().fg(DIM),
                ));
            }
            // Auto-sync state, shown on every real category rather than only
            // when enabled: not knowing a topic's sync is off is exactly how
            // "why don't these sessions know about each other" happens.
            if let Some(id) = cat.as_deref() {
                let on = app.state.category_auto_sync(id);
                spans.push(Span::styled(
                    if on { "  ⇄ auto" } else { "  ⇄ off" },
                    Style::default().fg(if on { Color::Green } else { DIM }),
                ));
            }
            items.push(ListItem::new(Line::from(spans)));
            continue;
        }
        let Some(s) = app.session_at(row) else {
            continue;
        };
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
                SessionStatus::Idle => ("● idle   ", IDLE, false),
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
            // Plain white, not Green — Green is also the "working" status
            // color, so a marked+working row used to show two same-colored,
            // different-shaped glyphs side by side.
            let (mark_glyph, mark_style) = if marked {
                (
                    "✓ ",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default())
            };
            // Time column reflects live activity: a running session (or a thread
            // root with a running/active-today lane) reads "now" / its lane's
            // recent time rather than its own possibly-stale transcript mtime.
            let (live_now, eff_active) = app.row_activity(s, child_count);
            // A running session's file mtime is always "just now" (the agent
            // keeps writing), so showing that here would say nothing useful —
            // time since the last genuine prompt (how long it's been working
            // solo, or how long it's been sitting idle since you last typed)
            // is the actually informative number for a live row. Kiro can't
            // derive this at all, and a session with no scan yet won't have it
            // either — fall back to the old plain "now" rather than a bare "—"
            // for what's still very much a live, just-created row.
            let when = if live_now {
                match app.row_last_prompt(s, child_count) {
                    Some(t) => relative_time(Some(t), now),
                    None => "now".to_string(),
                }
            } else {
                relative_time(eff_active, now)
            };
            // A 1-col rail, independent of recent/older and the live status
            // badge — it's the one thing that survives a session going
            // Idle/Ended and staying buried in "older".
            let rail = if app.state.is_in_progress(&s.id) {
                Span::styled("┃", Style::default().fg(IN_PROGRESS))
            } else {
                Span::raw(" ")
            };
            // Sessions inside a category sit one step in from their header, so
            // the group reads as a group. Without this the rows line up with the
            // header and the nesting is invisible — the whole point of grouping.
            let in_category = app.state.category_of(&s.id).is_some()
                || app
                    .state
                    .category_of(app.state.thread_root(&s.id))
                    .is_some();
            // A coloured guide, not blank indentation: two spaces alone were not
            // enough to tell a topic's rows apart from the loose ones below it.
            // Loose rows keep no lead at all — the offset is half the signal, so
            // padding them to match would throw the distinction away again.
            let nest = if in_category {
                Span::styled("│ ", Style::default().fg(CATEGORY))
            } else {
                Span::raw("")
            };
            let mut spans = vec![
                Span::styled(mark_glyph, mark_style),
                rail,
                nest,
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
            // Only where it differs from the account this provider starts on,
            // so the row that is somewhere else is the one that stands out.
            if let Some(name) = account_marks.as_ref().and_then(|m| m.label_for(s)) {
                spans.push(Span::styled(
                    format!("  {name}"),
                    Style::default().fg(CATEGORY),
                ));
            }
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
                format!("  {}", usage_cell(s)),
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
    let block_h = walker::HEIGHT + GAP + 2; // strip + gap + tagline + legend
                                            // Record whether the animated hero is actually on screen, so the event loop
                                            // only forces ~12fps redraws of the list when there's something to animate.
    app.hero_visible = region_h >= block_h + 2;
    if app.hero_visible {
        let inner_x = area.x + 1;
        let inner_w = area.width.saturating_sub(2);
        // The character walks along the floor of the panel: the strip is pinned
        // to the bottom so its ground line sits right above the border, and the
        // tagline/legend stack above it rather than below.
        let strip_y = bottom.saturating_sub(walker::HEIGHT);
        let tagline_y = strip_y.saturating_sub(GAP + 2);
        let legend_y = tagline_y + 1;
        draw_walker(
            f,
            Rect {
                x: inner_x,
                y: strip_y,
                width: inner_w,
                height: walker::HEIGHT,
            },
            app,
        );
        // A true first run (never collected a single session) has nothing for
        // the status legend below to describe — show a call to action instead
        // of a legend for statuses that don't exist yet.
        let (line1, line2) = if app.all_sessions.is_empty() {
            (
                Line::from(Span::styled("No sessions yet.", Style::default().fg(DIM))),
                Line::from(vec![
                    Span::styled("Press ", Style::default().fg(DIM)),
                    Span::styled(
                        "n",
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        " to start your first Codex, Claude, Kiro, or Cursor session.",
                        Style::default().fg(DIM),
                    ),
                ]),
            )
        } else {
            (
                Line::from(Span::styled(
                    "Run many Codex · Claude · Kiro · Cursor sessions like tabs",
                    Style::default().fg(DIM),
                )),
                Line::from(vec![
                    Span::styled("● blocked", Style::default().fg(Color::Rgb(245, 180, 90))),
                    Span::styled("   ● working", Style::default().fg(Color::Green)),
                    Span::styled("   ● idle", Style::default().fg(IDLE)),
                    Span::styled("   ○ done", Style::default().fg(Color::Rgb(150, 120, 120))),
                ]),
            )
        };
        f.render_widget(
            Paragraph::new(line1).alignment(Alignment::Center),
            Rect {
                x: inner_x,
                y: tagline_y,
                width: inner_w,
                height: 1,
            },
        );
        f.render_widget(
            Paragraph::new(line2).alignment(Alignment::Center),
            Rect {
                x: inner_x,
                y: legend_y,
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
/// hand-tuned layouts; a multi-select launch can easily pick far more
/// sessions than that (20+ is routine), so anything larger falls back to a near-square
/// grid sized by `sqrt(n)` — still biased wide (floor) or tall (ceil) to match
/// the chosen layout.
fn grid_rows(n: usize, layout: PaneLayout) -> usize {
    match (n, layout) {
        (_, PaneLayout::Single) | (1, _) => 1,
        (2, _) | (3, _) => 1,
        (4, _) | (5, _) | (6, _) => 2,
        _ => ((n as f64).sqrt().floor() as usize).max(1),
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
                "Press n to start a new Codex, Claude, Kiro, or Cursor session.",
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
        render_pane(
            f,
            app,
            &sid,
            idx,
            panes.len(),
            area,
            focused_view,
            true,
            None,
        );
        return;
    }

    // One pane per session in a uniform grid. Category is shown, not laid out:
    // it colours the border and names itself in the title, so grouping costs no
    // rows and every session stays visible — the thing stacking took away.
    let rects = compute_pane_rects(area, panes.len(), app.effective_layout());
    for (idx, sid) in panes.iter().enumerate() {
        let pane_area = rects.get(idx).copied().unwrap_or(area);
        let cat = app
            .category_of_session(sid)
            .map(|id| (app.category_label(&id), category_color(&id)));
        render_pane(
            f,
            app,
            sid,
            idx,
            panes.len(),
            pane_area,
            focused_view && idx == app.focused,
            false,
            cat.as_ref().map(|(l, c)| (l.as_str(), *c)),
        );
    }
}

/// Colours a category's cell is drawn in. Assigned from the category id so a
/// topic keeps its colour across restarts without anything being stored, and
/// picked to stay apart from the focus and zoom border colours.
const CELL_COLORS: [Color; 6] = [
    Color::Rgb(180, 142, 173), // mauve
    Color::Rgb(163, 190, 140), // green
    Color::Rgb(208, 135, 112), // clay
    Color::Rgb(136, 192, 208), // ice
    Color::Rgb(235, 203, 139), // sand
    Color::Rgb(180, 168, 220), // periwinkle
];

fn category_color(cat_id: &str) -> Color {
    let sum = cat_id
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31) + b as u32);
    CELL_COLORS[sum as usize % CELL_COLORS.len()]
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
    // The category this pane's cell belongs to: its name goes in front of the
    // session name and its colour on the border, so a stack is identifiable
    // without costing the grid a header row.
    cell: Option<(&str, Color)>,
) {
    let ended = app.ended.contains(sid);
    let (dot, dot_color) = pane_dot(app, sid, ended);
    let name = app.session_display_name(sid, pane_area.width.saturating_sub(20) as usize);
    // A plain-colored name blends into the border line and is easy to miss when
    // several panes are open — a bold, high-contrast chip (bg = the same color
    // as the status dot) makes "which session is this" readable at a glance
    // without stealing a content row from the terminal view.
    //
    // Focus gets its own glyph (▸), not just the accent-colored border: the
    // idle status dot is drawn in that same accent color, so a focused+idle
    // pane would otherwise read as just "blue" — shape, not hue, is what
    // actually disambiguates focus from status here.
    let mut title_spans: Vec<Span> = Vec::new();
    if let Some((label, color)) = cell {
        title_spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    let title = Line::from(vec![
        if pane_focused {
            Span::styled(
                "▸ ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("")
        },
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
            let hidden = total.saturating_sub(1);
            let label = if hidden > 0 {
                format!(" 🔍 ZOOMED · {hidden} hidden ")
            } else {
                " 🔍 ZOOMED ".to_string()
            };
            Span::styled(
                label,
                Style::default()
                    .fg(Color::Rgb(15, 17, 22))
                    .bg(ZOOM)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("")
        },
        // Passive "N new" badge: shown when this pane has detected, not-yet-
        // opened `.html` candidates. Rendered as PREVIEW-cyan text (not a filled
        // chip) so it reads as a quiet, lower-emphasis hint.
        match app.html_candidates.get(sid).map(Vec::len) {
            Some(n) if n > 0 => Span::styled(
                format!(" 🌐 {n} new "),
                Style::default().fg(PREVIEW).add_modifier(Modifier::BOLD),
            ),
            _ => Span::raw(""),
        },
        if ended {
            Span::styled("(ended) ", Style::default().fg(DIM))
        } else {
            Span::raw("")
        },
    ]);
    let mut title = title;
    if !title_spans.is_empty() {
        title_spans.extend(title.spans);
        title = Line::from(title_spans);
    }
    // Border shape marks focus, border colour marks category and attention.
    let (border_type, weight) = pane_border_for(pane_focused, zoomed);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .title(title)
        .border_style(match (zoomed, pane_focused) {
            (true, _) => Style::default().fg(ZOOM).add_modifier(Modifier::BOLD),
            (_, true) => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            // Never dimmed: at this weight a dim frame reads as a grey slab.
            _ => Style::default()
                .fg(pane_border_color(app, sid, ended, cell.map(|(_, c)| c)))
                .add_modifier(weight),
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

    // A child that died without drawing anything leaves the terminal view
    // blank, which reads as "nothing happened" rather than "it failed". Its
    // stderr went to a file (sharing the PTY makes codex abort), so show that
    // instead of the empty screen.
    if let Some(reason) = app.pane_error.get(sid) {
        render_pane_failure(f, reason, inner);
        return;
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

/// Border colour for a pane with no category — a neutral that still reads as a
/// drawn line, well clear of the dim grey used for de-emphasised text.
const UNCATEGORIZED_BORDER: Color = Color::Rgb(140, 148, 162);

/// Border colour for a session that has stopped and wants you. With every pane
/// at the same weight this is what makes one stand out, so it is the one colour
/// on screen no category is ever assigned.
const WAITING_BORDER: Color = Color::Rgb(224, 128, 92);

/// Which border a pane gets, and whether it is emboldened.
///
/// Every pane gets a half-block frame — the heaviest border a terminal can draw
/// without spending a second row. Line-drawing glyphs, even `Thick`, read as
/// hairlines next to a full-height pane of text, which left the grid looking
/// like a wireframe.
///
/// That uniform weight costs the attention channel, so attention moves to
/// colour: a session waiting on you takes the alert colour (see
/// `pane_border_color`), everything else keeps its category's. The status dot in
/// each title carries the same signal in text, so nothing depends on colour
/// alone.
///
/// The focused pane keeps the mirrored `QuadrantInside` set: same weight, but
/// the corners point the other way, so it is identifiable even in one colour.
fn pane_border_for(focused: bool, zoomed: bool) -> (BorderType, Modifier) {
    if zoomed || focused {
        return (BorderType::QuadrantInside, Modifier::BOLD);
    }
    (BorderType::QuadrantOutside, Modifier::BOLD)
}

/// A pane's border colour: the alert colour while it waits on you, otherwise its
/// category's (or a neutral when it has none). Focus and zoom are handled by the
/// caller, which overrides both.
fn pane_border_color(app: &App, sid: &str, ended: bool, cat: Option<Color>) -> Color {
    if ended
        || matches!(
            app.session_status(sid),
            SessionStatus::Blocked | SessionStatus::Ended
        )
    {
        return WAITING_BORDER;
    }
    cat.unwrap_or(UNCATEGORIZED_BORDER)
}

/// What a pane shows instead of an empty screen when its child failed to start.
fn render_pane_failure(f: &mut Frame, reason: &str, area: Rect) {
    let mut lines = vec![
        Line::from(Span::styled(
            "this session did not start",
            Style::default()
                .fg(WAITING_BORDER)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    lines.extend(
        reason
            .lines()
            .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(DIM)))),
    );
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "ctrl-q closes this pane · enter on the row tries again",
        Style::default().fg(DIM),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn pane_dot(app: &App, sid: &str, ended: bool) -> (&'static str, Color) {
    if ended {
        return ("○ ", Color::Rgb(150, 120, 120));
    }
    match app.session_status(sid) {
        SessionStatus::Blocked => ("● ", Color::Rgb(245, 180, 90)),
        SessionStatus::Working => ("● ", Color::Green),
        SessionStatus::Idle => ("● ", IDLE),
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
    let opts = ["codex", "claude", "kiro", "cursor"];
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

/// Handing a conversation over is the only way to carry it to another login,
/// since a session cannot be resumed on an account whose home never held it —
/// so the target is an account, not just a provider.
fn handoff_popup(f: &mut Frame, choice: usize, source: Option<&Session>, app: &App) {
    let targets = app.handoff_targets();
    let source_account = source.map(|s| app.account_of_session(s));

    let mut lines: Vec<Line> = Vec::new();
    let mut last_provider: Option<Agent> = None;
    for (i, account) in targets.iter().enumerate() {
        if last_provider != Some(account.provider) {
            lines.push(Line::from(Span::styled(
                account.provider.as_str().to_uppercase(),
                Style::default().fg(CATEGORY).add_modifier(Modifier::BOLD),
            )));
            last_provider = Some(account.provider);
        }
        let selected = i == choice;
        // Only the login it is already on is a no-op; another account of the
        // same provider is a genuine target.
        let same = source.map(|s| s.agent) == Some(account.provider)
            && source_account.as_ref().map(|a| a.name.as_str()) == Some(account.name.as_str());
        let marker = if selected { "  ▶ " } else { "    " };
        let suffix = if same { " (this session)" } else { "" };
        let mut style = if same {
            Style::default().fg(DIM)
        } else {
            Style::default()
        };
        if selected {
            style = style.fg(ACCENT).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(Span::styled(
            format!("{marker}{}{suffix}", account.name),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "enter start with handoff prompt   esc cancel",
        Style::default().fg(DIM),
    )));

    let width = lines
        .iter()
        .map(|line| line.width())
        .max()
        .unwrap_or(44)
        .max(44) as u16
        + 3;
    let area = centered(
        f.area(),
        width.min(f.area().width),
        (lines.len() as u16 + 2).min(f.area().height),
    );
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Handoff ");
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left),
        area,
    );
}

/// The width `help_popup` asks for, before the frame clamps it.
const HELP_WIDTH: u16 = 96;

fn help_popup(f: &mut Frame) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Keyboard shortcuts ");
    let lines = help_lines();
    let width = HELP_WIDTH.min(f.area().width);
    let area = centered(f.area(), width, wrapped_popup_height(&lines, width));
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Every row of the shortcuts popup. Split out from the drawing so a test can
/// check the real list against the height it will be given.
fn help_lines() -> Vec<Line<'static>> {
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
    vec![
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
        item("u", "accounts: which login each provider starts a session on"),
        item("h", "handoff selected session to another provider"),
        item("e", "edit selected session label"),
        item("x", "close/archive selected session"),
        item("i", "toggle the 'in progress' mark on selected session"),
        item(
            "c",
            "send a catch-up prompt to selected session (confirms if busy)",
        ),
        // One key, resolved by what the cursor is on (see main.rs). The context
        // rides in the key column so two rows can't read as two bindings.
        item(
            "t (session)",
            "put it in a topic category (all marked, in multi-select)",
        ),
        item(
            "t (header)",
            "auto-sync on/off, sync now, rename, remove",
        ),
        Line::from(""),
        section("View"),
        item(
            "→ / ←",
            "move around the tree only: → unfolds a category then steps inside, ← folds it. Neither opens a session — that is enter",
        ),
        item("/", "search visible sessions"),
        item("d", "change working directory scope"),
        item("a", "toggle archived sessions"),
        Line::from(""),
        section("Terminal / Modal"),
        item("ctrl-x", "return from terminal to session list"),
        item("tab / shift-tab", "cycle live panes (when 2+ open)"),
        item("ctrl-w", "cycle live panes (always)"),
        item("ctrl-z", "zoom the focused pane full-size (toggle back to split)"),
        item("ctrl-q", "close focused pane"),
        item(
            "ctrl-t",
            "transition report: topic/RUNBOOK/files -> review the assembled prompt -> enter sends, e edits first",
        ),
        item(
            "ctrl-p",
            "open a local .html file in the browser: pick from detected files (badge shows the count), or tab to type a path",
        ),
        item(
            "ctrl-y",
            "copy a link out of the focused pane's most recent answer that had one: a single link copies straight away, several open a picker (enter takes one, a takes all)",
        ),
        item("shift/alt/ctrl-enter", "insert newline in text modals"),
        item("esc", "cancel modal or close this help"),
        item("?", "show or close this help"),
    ]
}

/// Rows a bordered popup needs to show `lines` at `width` without clipping —
/// screen rows, not entries, because a line longer than the popup is wide takes
/// two of them.
///
/// This was a constant of slack added to `lines.len()` twice over, and both
/// times an entry was added the slack ran out and silently cut off the last
/// row. A guess cannot be kept in sync with the text above it; a measurement
/// can.
fn wrapped_popup_height(lines: &[Line], width: u16) -> u16 {
    let inner = width.saturating_sub(2).max(1);
    let rows: u16 = lines
        .iter()
        .map(|line| (line.width() as u16).div_ceil(inner).max(1))
        .sum();
    rows.saturating_add(2)
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

/// The usage column for one session row.
///
/// Each agent records something different and the column shows what it has:
/// codex and claude count tokens per request, kiro reports only how full its
/// context window is, and cursor records nothing at all — its figure is
/// estimated from the transcript, so it carries a `~` to keep it from reading
/// as a count. Nothing to show reads "—", never `0`, which would claim the
/// session spent nothing.
fn usage_cell(s: &Session) -> String {
    match s.agent {
        Agent::Kiro => match s.context_pct {
            Some(p) => format!("{p:.0}%"),
            None => "—".to_string(),
        },
        Agent::Cursor if s.tokens.total == 0 => "—".to_string(),
        Agent::Cursor => format!("~{}", human_tokens(s.tokens.total)),
        Agent::Codex | Agent::Claude => human_tokens(s.tokens.total),
    }
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

/// What the Accounts screen can do, in the order a reader needs it: pick one,
/// use one now, then the rarer housekeeping.
const ACCOUNT_HINTS: &[&str] = &[
    "w use this one",
    "enter session on it",
    "a add",
    "r rename",
    "l sign in",
    "d off",
    "x remove",
    "esc close",
];

/// Pack hints into rows of at most `width` columns, keeping each whole.
///
/// Truncation is not an option here: the last hint is `esc close`, and a user
/// who cannot see it has no way out that the screen admits to.
fn fold_hints(hints: &[&str], width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for hint in hints {
        match rows.last_mut() {
            Some(row) if row.chars().count() + 3 + hint.chars().count() <= width => {
                row.push_str("   ");
                row.push_str(hint);
            }
            _ => rows.push((*hint).to_string()),
        }
    }
    rows
}

/// The Accounts screen (`u`): which logins exist per provider, and which one
/// the next session takes. Management lives here rather than in the footer,
/// which has to stay small enough to leave the panes room.
fn accounts_popup(f: &mut Frame, app: &App) {
    use crate::app::accounts_panel::AccountRow;

    let Some(panel) = app.accounts_panel.as_ref() else {
        return;
    };

    // Typing a name replaces the list — one thing to look at at a time.
    if let Some(name) = &panel.new_name {
        use crate::app::accounts_panel::NameFor;
        let (title, prompt) = match panel.naming {
            Some(NameFor::Rename(i)) => (
                " Rename account ",
                match app.accounts.get(i) {
                    Some(account) => format!("New name for {}:", account.name),
                    None => "New name:".to_string(),
                },
            ),
            Some(NameFor::NewAccount(agent)) => (
                " Add account ",
                format!("Name for the new {} account:", agent.as_str()),
            ),
            None => (" Add account ", "Name:".to_string()),
        };
        let area = centered(f.area(), 58, 6);
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(prompt, Style::default().fg(DIM))),
                Line::from(Span::styled(
                    format!("{name}▏"),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "letters, digits, - and _",
                    Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
                )),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT))
                    .title(title),
            ),
            area,
        );
        return;
    }

    let rows = app.account_rows();
    let name_width = app
        .accounts
        .iter()
        .map(|a| a.name.chars().count())
        .max()
        .unwrap_or(7)
        .clamp(7, 20);

    let all_rows = app.quota_rows_all();
    let build = |show_origin: bool| {
        let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let selected = i == panel.selected;
            match row {
                AccountRow::Header(agent) => lines.push(Line::from(Span::styled(
                    format!(" {}", agent.as_str().to_uppercase()),
                    Style::default().fg(CATEGORY).add_modifier(Modifier::BOLD),
                ))),
                AccountRow::Entry(idx) => {
                    let Some(account) = app.accounts.get(*idx) else {
                        continue;
                    };
                    let panes = app.pane_count_on(account);
                    // The one question this screen exists to answer is which
                    // account the next session takes, so that is the column,
                    // not a role name the reader has to translate.
                    let in_use = app.account_for(account.provider).name == account.name;
                    let state = if account.disabled {
                        ("off", ERROR)
                    } else if in_use {
                        ("in use", IDLE)
                    } else {
                        ("reserve", DIM)
                    };
                    let activity = match panes {
                        0 => "idle".to_string(),
                        1 => "1 pane".to_string(),
                        n => format!("{n} panes"),
                    };
                    // The usage this login has left, where the decision to
                    // switch accounts is actually made.
                    let usage: Vec<&mindplayer_core::limits::QuotaRow> = all_rows
                        .iter()
                        .filter(|r| r.agent == account.provider && r.account == account.name)
                        .collect();
                    let origin = if show_origin && account.is_inherited() {
                        " (this machine's own login)"
                    } else {
                        ""
                    };
                    let style = if selected {
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(DIM)
                    };
                    let mut spans = vec![
                        Span::styled(if selected { "  ▶ " } else { "    " }, style),
                        Span::styled(format!("{:<name_width$}  ", account.name), style),
                        Span::styled(format!("{:<8}  ", state.0), Style::default().fg(state.1)),
                    ];
                    match usage.first() {
                        Some(row) => spans.extend(quota_row_spans(row, 9)),
                        None => spans.push(Span::styled(
                            format!("{:<9}   ─", ""),
                            Style::default().fg(DIM),
                        )),
                    }
                    spans.push(Span::styled(
                        format!("   {activity}{origin}"),
                        Style::default().fg(DIM),
                    ));
                    lines.push(Line::from(spans));
                    // A provider can meter more than one window (Claude's five
                    // hour and weekly), and hiding the second would hide the
                    // one that is actually binding.
                    for row in usage.iter().skip(1) {
                        let mut extra =
                            vec![Span::raw(format!("    {:<name_width$}  {:<8}  ", "", ""))];
                        extra.extend(quota_row_spans(row, 9));
                        lines.push(Line::from(extra));
                    }
                }
            }
        }
        lines
    };

    // The note saying an account is this machine's own login is the first
    // thing to go when the terminal is narrow: a name or a state cut in half
    // is unreadable, while losing the note only loses a nicety.
    let widest = |lines: &[Line]| lines.iter().map(|line| line.width()).max().unwrap_or(0);
    let mut lines = build(true);
    if widest(&lines) as u16 + 3 > f.area().width {
        lines = build(false);
    }

    if let Some(error) = &panel.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(ERROR),
        )));
    }
    // The widest row decides the popup, and the hint is folded to whatever
    // width is left rather than being cut off — a key the user cannot see is
    // the same as a key that does not exist.
    let listed = lines
        .iter()
        .map(|line| line.width())
        .max()
        .unwrap_or(0)
        .max(
            ACCOUNT_HINTS
                .iter()
                .map(|h| h.chars().count())
                .max()
                .unwrap_or(0),
        );
    let outer = (listed as u16 + 3).min(f.area().width);
    let inner = outer.saturating_sub(3) as usize;

    lines.push(Line::from(""));
    for row in fold_hints(ACCOUNT_HINTS, inner) {
        lines.push(Line::from(Span::styled(
            format!(" {row}"),
            Style::default().fg(DIM),
        )));
    }

    let height = (lines.len() as u16 + 2).min(f.area().height);
    let area = centered(f.area(), outer, height);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .title(" Accounts "),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_codex_accounts_render_weekly_and_monthly_in_the_footer() {
        use mindplayer_core::accounts::Account;
        use mindplayer_core::limits::QuotaRow;
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = App::new();
        app.screen = Screen::Main;
        app.accounts = ["sendbird-kr", "sendbird-com"]
            .into_iter()
            .map(|name| {
                let mut account = Account::inherited(Agent::Codex);
                account.name = name.into();
                account
            })
            .collect();
        app.limits = Some(
            ["sendbird-kr", "sendbird-com"]
                .into_iter()
                .flat_map(|account| {
                    let exhausted = account == "sendbird-com";
                    [
                        QuotaRow {
                            label: "codex weekly".into(),
                            agent: Agent::Codex,
                            account: account.into(),
                            used_percent: (!exhausted).then_some(0.0),
                            ..Default::default()
                        },
                        QuotaRow {
                            label: "codex monthly".into(),
                            agent: Agent::Codex,
                            account: account.into(),
                            used_percent: Some(if exhausted { 100.0 } else { 10.0 }),
                            detail: "reported usage/limit".into(),
                            resets: Some("10-01 09:00".into()),
                        },
                    ]
                })
                .collect(),
        );
        for width in [100, 140] {
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let lines: Vec<String> = (0..30)
                .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
                .collect();
            for account in ["sendbird-kr", "sendbird-com"] {
                for period in ["weekly", "monthly"] {
                    assert!(
                        lines
                            .iter()
                            .any(|line| line.contains(&format!("{account} {period}"))),
                        "{lines:#?}"
                    );
                }
            }
            let exhausted = lines
                .iter()
                .find(|line| line.contains("sendbird-com monthly"))
                .unwrap();
            assert!(
                exhausted.contains("100.0%")
                    && exhausted.contains("limit reached")
                    && exhausted.contains("10-01 09:00"),
                "{exhausted}"
            );
            if width == 100 {
                for line in lines.iter().filter(|line| line.contains("sendbird-")) {
                    println!("{}", line.trim_end());
                }
            }
        }
    }

    #[test]
    fn codex_monthly_limit_and_unknown_weekly_are_distinct_for_both_accounts() {
        use mindplayer_core::limits::QuotaRow;
        for account in ["sendbird-kr", "sendbird-com"] {
            let weekly = QuotaRow {
                label: format!("{account} weekly"),
                agent: Agent::Codex,
                account: account.into(),
                detail: "not reported".into(),
                ..Default::default()
            };
            let monthly = QuotaRow {
                label: format!("{account} monthly"),
                agent: Agent::Codex,
                account: account.into(),
                used_percent: Some(100.0),
                detail: "limit reached".into(),
                resets: Some("10-01 09:00".into()),
            };
            let weekly_line = Line::from(quota_row_spans(&weekly, 20));
            let monthly_spans = quota_row_spans(&monthly, 20);
            let monthly_line = Line::from(monthly_spans.clone());
            assert!(weekly_line.to_string().contains("—"));
            assert!(!weekly_line.to_string().contains("0.0%"));
            assert!(monthly_line.to_string().contains("100.0%"));
            assert!(monthly_line.to_string().contains("limit reached"));
            assert!(monthly_line.to_string().contains("10-01 09:00"));
            assert!(monthly_spans.iter().any(|span| {
                span.content.contains("limit reached") && span.style.fg == Some(ERROR)
            }));
            assert!(monthly_line.width() <= 100, "{monthly_line}");
        }
    }

    fn session(agent: Agent, total: u64) -> Session {
        Session {
            id: "id".into(),
            agent,
            cwd: PathBuf::from("/work"),
            file: PathBuf::from("/work/session"),
            started_at: None,
            last_active: None,
            last_prompt_at: None,
            tokens: mindplayer_core::TokenUsage {
                total,
                ..Default::default()
            },
            title: "t".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }
    }

    /// A measured count and an estimate share one column, so the estimate is
    /// marked: cursor keeps no usage of its own and its figure is inferred from
    /// the transcript, which must not read as the number the other agents
    /// actually reported.
    #[test]
    fn an_estimated_total_is_marked_and_a_measured_one_is_not() {
        assert_eq!(usage_cell(&session(Agent::Cursor, 6_200_000)), "~6.2M");
        assert_eq!(usage_cell(&session(Agent::Claude, 6_200_000)), "6.2M");
    }

    /// Zero here means "nothing on disk to estimate from", and a session that
    /// spent nothing is a different claim than one we cannot read.
    #[test]
    fn a_cursor_session_with_nothing_to_estimate_shows_no_number() {
        assert_eq!(usage_cell(&session(Agent::Cursor, 0)), "—");
    }

    /// A line that fits costs one row; one that overflows costs as many as it
    /// wraps into. The old sizing counted entries, so long descriptions were
    /// free — which is how the last shortcut kept falling off the popup.
    #[test]
    fn popup_height_counts_wrapped_rows_not_entries() {
        let inner = 20u16;
        let width = inner + 2;
        let short = Line::from("x".repeat(inner as usize));
        let long = Line::from("x".repeat(inner as usize + 1));
        assert_eq!(
            wrapped_popup_height(std::slice::from_ref(&short), width),
            3,
            "1 row + 2 borders"
        );
        assert_eq!(wrapped_popup_height(&[long], width), 4, "wraps to 2 rows");
        assert_eq!(
            wrapped_popup_height(&[Line::from(""), short], width),
            4,
            "an empty spacer still costs its row"
        );
    }

    /// The guard that stops this from breaking a third time: whatever the
    /// shortcut list grows into, the popup it is drawn in must be tall enough
    /// for all of it at the width the popup actually asks for.
    #[test]
    fn every_shortcut_fits_in_the_help_popup() {
        let lines = help_lines();
        let inner = HELP_WIDTH - 2;
        let needed: u16 = lines
            .iter()
            .map(|l| (l.width() as u16).div_ceil(inner).max(1))
            .sum();
        assert_eq!(
            wrapped_popup_height(&lines, HELP_WIDTH),
            needed + 2,
            "help popup must fit every row plus its borders"
        );
    }

    /// Two rows both labelled `t` read as a duplicate binding; the context
    /// belongs in the key column instead. See main.rs — one key, resolved by
    /// what the cursor is on.
    #[test]
    fn no_shortcut_key_is_listed_twice() {
        let keys: Vec<String> = help_lines()
            .iter()
            .filter(|l| l.spans.len() == 2)
            .map(|l| l.spans[0].content.trim().to_string())
            .collect();
        let mut seen = std::collections::HashSet::new();
        for key in &keys {
            assert!(seen.insert(key.clone()), "`{key}` is listed twice");
        }
        assert!(keys.iter().any(|k| k == "ctrl-y"), "ctrl-y is documented");
    }

    /// Every pane is drawn at the same weight; the focused one differs by the
    /// direction of its corners, not by being heavier. If both ever resolved to
    /// the same set, focus would be legible only by colour.
    #[test]
    fn focus_uses_a_different_border_set_at_the_same_weight() {
        let (plain, _) = pane_border_for(false, false);
        let (focused, _) = pane_border_for(true, false);
        let (zoomed, _) = pane_border_for(false, true);
        assert_eq!(plain, BorderType::QuadrantOutside);
        assert_eq!(focused, BorderType::QuadrantInside);
        assert_eq!(zoomed, focused, "zoom reads as focus");
        assert_ne!(plain, focused);
    }

    /// The colour has to carry attention now that weight is uniform, so the
    /// waiting colour must not collide with any colour a category can take.
    #[test]
    fn no_category_can_be_mistaken_for_the_waiting_colour() {
        assert_ne!(WAITING_BORDER, UNCATEGORIZED_BORDER);
        for c in CELL_COLORS {
            assert_ne!(
                c, WAITING_BORDER,
                "a category would look like it is waiting"
            );
        }
    }

    /// Borders are never dimmed. Fading a quiet pane's frame was what made the
    /// old grid look unfinished rather than calm.
    #[test]
    fn no_border_colour_is_the_dim_grey_used_for_text() {
        assert_ne!(UNCATEGORIZED_BORDER, DIM);
        for id in ["cat_1", "cat_2", "cat_3", "pulse", "tower"] {
            assert_ne!(category_color(id), DIM, "{id}");
        }
    }

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
    fn three_panes_tile_the_body() {
        let area = body();
        let rects = compute_pane_rects(area, 3, PaneLayout::Horizontal);
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0].x + rects[0].width, rects[1].x);
        assert_eq!(rects[1].x + rects[1].width, rects[2].x);
        assert_eq!(rects.iter().map(|r| r.width).sum::<u16>(), area.width);
        assert!(rects.iter().all(|r| r.height == area.height));
    }

    /// Every cell of an `n`-pane grid stays inside `area`, has no zero-size
    /// pane, and the cells cover `area` exactly (no gaps / no overlap, checked
    /// via summed cell area) — for both split layouts.
    fn assert_tiles_exactly(area: Rect, n: usize) {
        for layout in [PaneLayout::Horizontal] {
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
        // A real multi-select launch routinely accumulates this many panes —
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
