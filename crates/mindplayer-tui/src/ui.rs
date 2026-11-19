//! Rendering for every screen. `render` also records the right-pane size back
//! into `App` so the PTY can be spawned/resized at the correct dimensions.

use crate::app::{App, CategoryMenu, Focus, PaneLayout, Row, Screen, SessionStatus, MAX_PANES};
use crate::mascot;
use crate::terminal_view::TerminalView;
use crate::walker;
use chrono::{DateTime, Utc};
use mindplayer_core::tokens::human_tokens;
use mindplayer_core::{Agent, Session};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use ratatui::Frame;
use std::collections::HashMap;
use std::path::PathBuf;

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
// Cyan highlights the link-copy picker.
const PREVIEW: Color = Color::Rgb(82, 196, 214);
// Rose highlights errors.
const ERROR: Color = Color::Rgb(245, 130, 120);
// The observer is a local read-only tool, not another agent terminal. Keep its
// colour and surface separate from session/focus chrome so the two cannot be
// mistaken for peer panes when they share a row.
const OBSERVER: Color = Color::Rgb(74, 210, 177);
const OBSERVER_BG: Color = Color::Rgb(21, 35, 40);
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

fn agent_tag(agent: Agent) -> (&'static str, Color) {
    match agent {
        Agent::Codex => ("codex ", ACCENT),
        Agent::Claude => ("claude", Color::Magenta),
        Agent::Kiro => ("kiro  ", Color::Cyan),
        Agent::Cursor => ("cursor", Color::Rgb(242, 170, 76)),
        Agent::Omp => ("omp   ", Color::Green),
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
        VERCEL_HEAD => Color::White,
        _ => Color::Rgb(180, 142, 240),
    }
}

const VERCEL_HEAD: &str = "vercel";

/// Authentication method and independent enforced budgets, never the local target.
fn vercel_footer_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    use mindplayer_core::limits::compact_decimal;
    use mindplayer_core::vercel_usage::VercelUsageError;

    let reading = (app.vercel_cwd.as_ref() == Some(&app.cwd))
        .then_some(app.vercel_budget.as_ref())
        .flatten();
    let mut blocked = false;
    let text = match reading {
        None => "vercel · loading · blocked state unknown".to_string(),
        Some(Err(VercelUsageError::NotConfigured)) => {
            "vercel · not configured · blocked state unknown".to_string()
        }
        Some(Err(_)) => "vercel · lookup unavailable · blocked state unknown".to_string(),
        Some(Ok(usage)) => {
            let key_state = match &usage.key {
                Ok(budget) => budget.used.map(|used| used >= budget.limit),
                Err(VercelUsageError::NoBudget) => Some(false),
                Err(_) => None,
            };
            let user_state = match &usage.user {
                Ok(Some(budget)) => budget.used.map(|used| used >= budget.limit),
                Ok(None) | Err(VercelUsageError::NoBudget) => Some(false),
                Err(_) => None,
            };
            blocked = key_state == Some(true) || user_state == Some(true);
            let state = if blocked {
                "blocked"
            } else if key_state.is_some() && user_state.is_some() {
                "not blocked by budgets"
            } else {
                "blocked state unknown"
            };
            let mut text = format!("vercel API key · {state}");
            match &usage.user {
                Ok(Some(budget)) => {
                    text.push_str(&format!(" · user {} ", budget.refresh_period));
                    match budget.used {
                        Some(used) => text.push_str(&format!(
                            "${used:.2}/${}{}",
                            compact_decimal(budget.limit),
                            if user_state == Some(true) {
                                " exceeded"
                            } else {
                                ""
                            },
                        )),
                        None => text.push_str(&format!(
                            "spend unknown / ${} limit (no meter)",
                            compact_decimal(budget.limit),
                        )),
                    }
                }
                Ok(None) => text.push_str(" · team attributed"),
                Err(VercelUsageError::NoBudget) => text.push_str(" · user no active budget"),
                Err(VercelUsageError::UserNotIdentified) => {
                    text.push_str(" · user attribution unknown")
                }
                Err(_) => text.push_str(" · user lookup unavailable"),
            }
            match &usage.key {
                Ok(budget) if key_state == Some(true) => text.push_str(&format!(
                    " · key {} ${:.2}/${} exceeded",
                    budget.refresh_period,
                    budget.used.expect("blocked key has a meter"),
                    compact_decimal(budget.limit),
                )),
                Ok(budget)
                    if key_state == Some(false)
                        && !matches!(&usage.user, Ok(Some(user)) if user.used.is_some()) =>
                {
                    text.push_str(&format!(
                        " · key spend ${:.2}",
                        budget.used.expect("known key meter")
                    ));
                }
                Ok(_) if key_state.is_none() => text.push_str(" · key spend unknown"),
                Err(VercelUsageError::NoBudget) => text.push_str(" · key no active budget"),
                Err(_) => text.push_str(" · key lookup unavailable"),
                _ => {}
            }
            text
        }
    };
    // Wrap, never truncate: even exceptionally long amounts survive narrow terminals.
    let width = usize::from(width.max(1));
    let style = Style::default().fg(if blocked { ERROR } else { DIM });
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut cells = 0;
    for word in text.split_whitespace() {
        let word_cells = Span::raw(word).width();
        if cells > 0 && cells + 1 + word_cells > width {
            lines.push(Line::styled(std::mem::take(&mut line), style));
            cells = 0;
        }
        if cells > 0 {
            line.push(' ');
            cells += 1;
        }
        for grapheme in Span::raw(word).styled_graphemes(Style::default()) {
            let cell_width = Span::raw(grapheme.symbol).width();
            if cells + cell_width > width {
                lines.push(Line::styled(std::mem::take(&mut line), style));
                cells = 0;
            }
            line.push_str(grapheme.symbol);
            cells += cell_width;
        }
    }
    if !line.is_empty() {
        lines.push(Line::styled(line, style));
    }
    lines
}

/// Detail text turns red when the reading means work is stopped.
fn quota_detail_color(row: &mindplayer_core::limits::QuotaRow) -> Color {
    if row.blocked || (row.used_percent.is_some_and(|used| used >= 100.0) && !row.spend_cap) {
        ERROR
    } else {
        DIM
    }
}

/// One footer row per account window: name, gauge, percentage, then the figure
/// behind it. A row whose provider reported no window gets an em dash where the
/// gauge would be — an empty gauge would read as "plenty left".
fn quota_row_spans(
    row: &mindplayer_core::limits::QuotaRow,
    name_width: usize,
    hint: Option<(String, Color)>,
) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        format!("  {:<name_width$} ", row.label),
        Style::default().fg(quota_label_color(&row.label)),
    )];
    match row.used_percent {
        Some(used) => {
            let filled = ((used / 100.0) * QUOTA_GAUGE_CELLS as f64).round() as usize;
            let filled = filled.min(QUOTA_GAUGE_CELLS);
            let tier = if row.blocked {
                ERROR
            } else if row.spend_cap && used > 90.0 {
                ZOOM
            } else {
                quota_tier(used)
            };
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
    if let Some((text, color)) = hint {
        spans.push(Span::styled(
            format!("  {text}"),
            Style::default().fg(color),
        ));
    } else if !row.detail.is_empty() {
        spans.push(Span::styled(
            format!("  {}", row.detail),
            Style::default().fg(quota_detail_color(row)),
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

/// What the footer says about a failed reading: what happened and what to do, in place of the raw cause.
fn failure_hint(app: &App, row: &mindplayer_core::limits::QuotaRow) -> Option<(String, Color)> {
    use mindplayer_core::limits::FailureKind;
    let kind = row.failure?;
    Some(match kind {
        FailureKind::SignIn => {
            let inherited = row.account.is_empty()
                || app
                    .accounts
                    .iter()
                    .any(|a| a.provider == row.agent && a.name == row.account && a.is_inherited());
            let action = if inherited {
                let account = mindplayer_core::accounts::Account::inherited(row.agent);
                let login = mindplayer_core::resume::login(row.agent, PathBuf::new(), &account);
                format!("run: {} {}", login.program, login.args.join(" "))
            } else {
                "press u, then l on this account".to_string()
            };
            (
                format!("sign-in needed · {action} · Ctrl-R after login"),
                ZOOM,
            )
        }
        FailureKind::Temporary => ("temporarily unavailable · Ctrl-R to retry".to_string(), DIM),
        FailureKind::RateLimited => ("rate limited · Ctrl-R when ready".to_string(), DIM),
        FailureKind::Unsupported => ("usage not available for this account".to_string(), DIM),
        FailureKind::NotInstalled => (
            format!(
                "{} not installed · reinstall it, or press u then d to hide",
                row.agent.program()
            ),
            ZOOM,
        ),
        FailureKind::Unreadable => (
            format!("can't read {} output · details: u", row.agent.program()),
            ZOOM,
        ),
        FailureKind::Internal => ("mindplayer error · details: u".to_string(), ERROR),
    })
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
            "  Codex / Claude / Kiro / Cursor / OMP session manager",
            Style::default().fg(DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

pub(crate) fn pane_provider(app: &App, sid: &str) -> Option<Agent> {
    app.pane_accounts
        .get(sid)
        .map(|(agent, _)| *agent)
        .or_else(|| {
            app.all_sessions
                .iter()
                .find(|session| session.id == sid)
                .map(|session| session.agent)
        })
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

/// Asked when a provider says it has no account behind it, so nothing keeps
/// reaching for one that is not there.
fn unusable_account_popup(f: &mut Frame, app: &App) {
    let Some(account) = app.unusable_prompt.as_ref() else {
        return;
    };
    let who = format!("{} · {}", account.provider.as_str(), account.name);
    let area = centered(f.area(), 58, 7);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                who,
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "reports no account. Sign in now?",
                Style::default(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "y sign in   ·   n turn it off   ·   any other key: not now",
                Style::default().fg(DIM),
            )),
        ])
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Account unavailable "),
        ),
        area,
    );
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
    let error_rows = if app.status.is_empty() {
        0
    } else {
        (Span::raw(&app.status)
            .width()
            .div_ceil(usize::from(f.area().width.max(1)))
            + 1)
        .min(4) as u16
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1 + error_rows),
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

    if app.status.is_empty() {
        footer(
            chunks[2],
            f,
            "↑↓ choose   enter scan   c character   q quit",
        );
    } else {
        let error_area = Rect {
            height: error_rows,
            ..chunks[2]
        };
        f.render_widget(
            Paragraph::new(app.status.as_str())
                .style(Style::default().fg(ERROR))
                .wrap(Wrap { trim: false }),
            error_area,
        );
        footer(
            Rect {
                y: chunks[2].y + error_rows,
                height: 1,
                ..chunks[2]
            },
            f,
            "↑↓ choose · enter retry · q quit",
        );
    }

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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Sessions · loading ");
    f.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::styled(
                format!("  {spin} Collecting sessions · {}", app.scope_label()),
                Style::default().fg(ACCENT),
            ),
            Line::styled("  Reading session metadata…", Style::default().fg(DIM)),
        ])
        .block(block)
        .wrap(Wrap { trim: false }),
        chunks[1],
    );
    footer(
        chunks[2],
        f,
        "Sessions appear here when collection finishes.",
    );
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
    let summary_head = if app.show_archived {
        format!(
            "{} archived · page {}",
            app.archive_matches,
            app.manual_usage_summary(app.visible_sessions())
        )
    } else {
        app.summary_head()
    };
    status.push(Span::styled(summary_head, Style::default().fg(DIM)));
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
    // Preserve every account window and reset; only one account's adjacent
    // windows may share a row when their complete cell widths fit.
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
    let all_usage = app.quota_rows_all();
    let mut plain_labels = Vec::with_capacity(quota_rows.len());
    for row in &mut quota_rows {
        // Keep the footer compact; the Accounts screen retains the reported
        // usage/limit figures. The exhausted state must still be explicit.
        if row.agent == Agent::Codex && row.label == "codex credits" && row.used_percent.is_some() {
            let shared_login = row
                .detail
                .starts_with("same login as ")
                .then(|| row.detail.clone());
            row.detail = if row.blocked {
                "blocked".into()
            } else if row.used_percent.is_some_and(|used| used >= 100.0) {
                if row.spend_cap {
                    "credit cap reached".into()
                } else {
                    "limit reached".into()
                }
            } else {
                String::new()
            };
            if let Some(shared_login) = shared_login {
                if !row.detail.is_empty() {
                    row.detail.push_str(" · ");
                }
                row.detail.push_str(&shared_login);
            }
        }
        let mut rest = mindplayer_core::limits::label_without_provider(row).to_string();
        let mut plain = rest.clone();
        if !row.account.is_empty() && app.provider_has_several_logins(row.agent) {
            let mut name = row.account.clone();
            let bare = row.account.clone();
            let owner = app
                .accounts
                .iter()
                .find(|a| a.provider == row.agent && a.name == row.account)
                .filter(|a| a.provider == Agent::Claude);
            if let Some(owner) = owner {
                if let Some(plan) = mindplayer_core::claude_auth::known_plan_label(
                    owner
                        .claude_plan
                        .as_deref()
                        .filter(|plan| *plan != "unknown")
                        .or(row.plan.as_deref()),
                ) {
                    name = format!("{name} ({plan})");
                }
                if let Some(first) = app.same_subscription_as(owner, &all_usage, false) {
                    if !row.detail.is_empty() {
                        row.detail.push_str(" · ");
                    }
                    row.detail.push_str(&format!("= {first}"));
                }
            }
            plain = if rest.is_empty() {
                bare
            } else {
                format!("{bare} {rest}")
            };
            rest = if rest.is_empty() {
                name
            } else {
                format!("{name} {rest}")
            };
        }
        row.label = rest;
        plain_labels.push(plain);
    }
    let platform_width = quota_rows
        .iter()
        .map(|r| r.agent.as_str().chars().count())
        .chain(std::iter::once(VERCEL_HEAD.chars().count()))
        .max()
        .unwrap_or(0);
    let widest_label = |rows: &[mindplayer_core::limits::QuotaRow]| {
        rows.iter()
            .map(|r| r.label.chars().count())
            .max()
            .unwrap_or(0)
    };
    // A row that would wrap loses the plan suffix, so the mark and the figures stay on one line.
    let overflows = |rows: &[mindplayer_core::limits::QuotaRow]| {
        let width = widest_label(rows);
        rows.iter().any(|row| {
            let cells: usize = quota_row_spans(row, width, failure_hint(app, row))
                .iter()
                .map(Span::width)
                .sum();
            platform_width + 2 + cells > usize::from(f.area().width)
        })
    };
    if overflows(&quota_rows) {
        for (row, plain) in quota_rows.iter_mut().zip(plain_labels) {
            row.label = plain;
        }
    }
    let name_width = widest_label(&quota_rows);
    let mut quota_lines: Vec<Line<'static>> = Vec::new();
    let mut previous = None;
    for row in &quota_rows {
        let spans = quota_row_spans(row, name_width, failure_hint(app, row));
        let cells: usize = spans.iter().map(Span::width).sum();
        if previous == Some((row.agent, row.account.as_str()))
            && quota_lines
                .last()
                .is_some_and(|line| line.width() + 3 + cells <= usize::from(f.area().width))
        {
            let line = quota_lines.last_mut().expect("previous account line");
            line.spans
                .push(Span::styled(" · ", Style::default().fg(DIM)));
            line.spans.extend(spans);
        } else {
            let mut line = Line::from(Span::styled(
                format!(" {:<platform_width$} ", row.agent.as_str()),
                Style::default()
                    .fg(quota_label_color(row.agent.as_str()))
                    .add_modifier(Modifier::BOLD),
            ));
            line.spans.extend(spans);
            quota_lines.push(line);
        }
        previous = Some((row.agent, row.account.as_str()));
    }
    quota_lines.extend(vercel_footer_lines(app, f.area().width));
    let quota = Paragraph::new(quota_lines).wrap(Wrap { trim: false });
    let quota_h = quota.line_count(f.area().width).min(u16::MAX as usize) as u16;
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

    let keys: String = if app.focus == Focus::List && app.show_archived {
        let pages = app
            .archive_matches
            .div_ceil(crate::app::working_set::ARCHIVE_PAGE_SIZE)
            .max(1);
        let page = app.archive_page.saturating_add(1).min(pages);
        if app.search_query.is_some() {
            format!("PgUp/PgDn page {page}/{pages} · enter restore · type to filter · esc cancel")
        } else {
            format!("[ ] or PgUp/PgDn page {page}/{pages} · enter restore · / search")
        }
    } else if app.search_query.is_some() && app.multi_select {
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
            Focus::Terminal
                if app.focused_pane().and_then(|sid| pane_provider(app, sid))
                    == Some(Agent::Omp) =>
            {
                "ctrl-x list · tab/shift-tab pane · ctrl-z zoom · ctrl-q close · other keys: OMP"
                    .to_string()
            }
            Focus::Terminal => {
                "ctrl-g trace · ctrl-x list · tab pane · ctrl-z zoom · ctrl-q close".to_string()
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
        f.render_widget(quota, footer_rows[1]);
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(keys, Style::default().fg(DIM))))
            .alignment(Alignment::Right),
        footer_rows[2],
    );
    if show_more_keys {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "i in progress · c catch-up · t category · ←→ fold    ctrl-r usage · ? help",
                Style::default().fg(Color::Rgb(90, 95, 108)),
            )))
            .alignment(Alignment::Right),
            footer_rows[3],
        );
    }

    if app.unusable_prompt.is_some() {
        unusable_account_popup(f, app);
    } else if app.help_visible {
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
        new_session_popup(f, choice, &app.new_session_choice_labels());
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
    } else if let Some(id) = &app.catchup_confirm {
        let title = app
            .all_sessions
            .iter()
            .find(|s| &s.id == id)
            .map(|s| s.title.as_str())
            .unwrap_or("this session");
        catchup_confirm_popup(f, title);
    }
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

/// Pick a link when an answer contains two or more.
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

/// Visible tree edges, lane counts, and source identity counts.
#[derive(Default)]
pub(crate) struct ListThreadMetadata {
    visible_child_count: usize,
    child_prefix: Option<&'static str>,
    source_count: usize,
}

#[cfg(test)]
impl ListThreadMetadata {
    pub(crate) fn visible_child_count(&self) -> usize {
        self.visible_child_count
    }
}

/// Keeps only requested session IDs; off-screen sources still detect collisions.
pub(crate) fn list_render_metadata(
    app: &App,
    rows: impl Iterator<Item = usize> + Clone,
) -> HashMap<&str, ListThreadMetadata> {
    let mut metadata = HashMap::<_, ListThreadMetadata>::new();
    if app.state.handoff_links.is_empty() {
        return metadata;
    }
    // Off-screen sources contribute identity counts without per-row allocations.
    for session in rows.clone().filter_map(|row| app.session_at(row)) {
        metadata.entry(session.id.as_str()).or_default();
    }
    if metadata.is_empty() {
        return metadata;
    }
    for session in &app.all_sessions {
        if let Some(meta) = metadata.get_mut(session.id.as_str()) {
            meta.source_count += 1;
        }
    }
    // Handoff lanes are root-linked. Only draw a tree for a contiguous run
    // whose unambiguous parent is on screen; hidden rows and group boundaries
    // must never make a lane appear attached to an unrelated preceding row.
    let mut parent: Option<&Session> = None;
    let mut last_child = None;
    for row in rows {
        if row == app.recent_count {
            parent = None;
            last_child = None;
        }
        let Some(session) = app.session_at(row) else {
            parent = None;
            last_child = None;
            continue;
        };
        if metadata[session.id.as_str()].source_count != 1
            || app.handoff_archive_source_conflicts(session)
        {
            parent = None;
            last_child = None;
            continue;
        }
        let connected_parent = parent
            .filter(|parent| app.state.handoff_parent(&session.id) == Some(parent.id.as_str()));
        if let Some(parent) = connected_parent {
            if let Some(previous) = last_child {
                metadata
                    .get_mut(previous)
                    .expect("visible child")
                    .child_prefix = Some("  ├─ ");
            }
            metadata
                .get_mut(session.id.as_str())
                .expect("visible child")
                .child_prefix = Some("  └─ ");
            metadata
                .get_mut(parent.id.as_str())
                .expect("visible parent")
                .visible_child_count += 1;
            last_child = Some(session.id.as_str());
        } else {
            parent = Some(session);
            last_child = None;
        }
    }
    metadata
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
    // Hidden PTYs conservatively retain the state column.
    let all_saved = app.ptys.is_empty();
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
            app.visible_aggregate.session_count()
        ))
        .title_bottom(
            Line::from(format!(
                " {} visible / {} running{} ",
                app.visible_aggregate.session_count(),
                app.live_pty_count(),
                if all_saved {
                    " · no sessions open here"
                } else {
                    ""
                },
            ))
            .alignment(Alignment::Right),
        );

    let content_width = usize::from(area.width.saturating_sub(4));
    let mark_width = if app.multi_select { 2 } else { 0 };
    let show_provider = area.width >= 40;
    let mut show_id = area.width >= 58;
    let mut show_age = show_provider;
    let mut account_width = if show_provider {
        account_marks
            .as_ref()
            .map_or("unverified".len(), |m| m.width())
            + 1
    } else {
        0
    };
    let mut status_width = if all_saved || !show_provider { 0 } else { 10 };
    let mut metadata_width = 2
        + usize::from(show_provider) * 7
        + account_width
        + status_width
        + usize::from(show_age) * 5
        + usize::from(show_id) * 9;
    let metadata_budget = content_width.saturating_sub(5 + mark_width + 12);
    if metadata_width > metadata_budget && show_id {
        show_id = false;
        metadata_width -= 9;
    }
    if metadata_width > metadata_budget && show_age {
        show_age = false;
        metadata_width -= 5;
    }
    let account_reduction = account_width.min(metadata_width.saturating_sub(metadata_budget));
    account_width -= account_reduction;
    metadata_width -= account_reduction;
    if metadata_width > metadata_budget && status_width > 0 {
        status_width = 2;
        metadata_width -= 8;
    }
    let max_title = content_width.saturating_sub(5 + mark_width + metadata_width);
    let recent_count = app.recent_count.min(app.visible.len());
    if app.list_row_items.len() != app.visible.len() {
        app.rebuild_list_items();
    }
    let recent_sessions = app.list_recent_sessions;
    let older_sessions = app.list_older_sessions;
    let older_divider = (recent_count > 0 && recent_count < app.visible.len())
        .then(|| app.list_row_items[recent_count] - 1);
    let rendered_rows = app.list_item_rows.len();
    let selected_item =
        (app.selected < app.visible.len()).then(|| app.list_row_items[app.selected]);
    let viewport_rows = usize::from(area.height.saturating_sub(2));
    let first_item =
        selected_item.map_or(0, |item| (item + 1).saturating_sub(viewport_rows.max(1)));
    let end_item = first_item.saturating_add(viewport_rows).min(rendered_rows);
    let logical_row = |item: usize| app.list_item_rows[item];
    let metadata = list_render_metadata(app, (first_item..end_item).filter_map(logical_row));
    let mut items: Vec<ListItem> = Vec::with_capacity(end_item - first_item);
    for item in first_item..end_item {
        let Some(row) = logical_row(item) else {
            if item != 0 && older_divider != Some(item) {
                items.push(ListItem::new(""));
                continue;
            }
            let is_recent = item == 0 && recent_count > 0;
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
            continue;
        };
        let Some(kind) = app.row_at(row) else {
            continue;
        };
        let is_recent = row < recent_count;
        // Category header rows: the fold marker plus a session count. Rendered
        // as a normal list item so the cursor can sit on it (that is what makes
        // `←`/`→` able to fold without stealing `→` from session rows).
        if let Row::Header(cat) = kind {
            let count = match cat.as_deref() {
                Some(id) => app.category_session_count(Some(id)),
                None => app.loose_session_count(is_recent),
            };
            let (glyph, label) = match cat {
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
                spans.push(Span::styled(format!("  {count}"), Style::default().fg(DIM)));
            }
            if let Some(id) = cat.as_deref() {
                if app.state.category_auto_sync(id) {
                    spans.push(Span::styled("  ⇄ auto", Style::default().fg(Color::Green)));
                }
            }
            items.push(ListItem::new(Line::from(spans)));
            continue;
        }
        let Some(s) = app.session_at(row) else {
            continue;
        };
        {
            let marked = app.marked.contains(&s.id);
            let meta = metadata.get(s.id.as_str());
            let visible_child_count = meta.map_or(0, |meta| meta.visible_child_count);
            let (tag, tag_color) = agent_tag(s.agent);
            // Clear, fixed-width status badge so it's easy to scan a column of
            // running / working / done sessions.
            let status = app.session_status_for(s);
            let (badge, badge_color, badge_bold) = match status {
                SessionStatus::Blocked => ("! blocked", WAITING_BORDER, true),
                SessionStatus::Working => ("● working", WORKING_BORDER, true),
                SessionStatus::Completed => ("✓ done   ", COMPLETED_BORDER, true),
                SessionStatus::Idle => ("○ idle   ", UNCATEGORIZED_BORDER, false),
                SessionStatus::Ended => ("○ ended  ", ENDED_BORDER, false),
                SessionStatus::Inactive => (" not open", DIM, false),
            };
            let mut badge_style = Style::default().fg(badge_color);
            if badge_bold {
                badge_style = badge_style.add_modifier(Modifier::BOLD);
            }
            let connected = meta
                .is_some_and(|meta| meta.child_prefix.is_some() || meta.visible_child_count > 0);
            let (thread_prefix, title_style) = match meta.and_then(|meta| meta.child_prefix) {
                Some(prefix) => (prefix, Style::default().fg(Color::Rgb(190, 196, 210))),
                None if visible_child_count > 0 => {
                    ("  ┌─ ", Style::default().add_modifier(Modifier::BOLD))
                }
                None if app.state.handoff_parent(&s.id).is_some()
                    || s.title.starts_with("(handoff)") =>
                {
                    ("  ↳  ", Style::default())
                }
                None => ("     ", Style::default()),
            };
            let title_suffix = if visible_child_count > 0 {
                format!("  [{visible_child_count} lanes]")
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
            // Each row shows its own source's time; the status guard keeps a colliding ID from borrowing a live prompt time.
            let live_now = !matches!(status, SessionStatus::Inactive | SessionStatus::Ended);
            // A running session's file mtime is always "just now" (the agent
            // keeps writing), so showing that here would say nothing useful —
            // time since the last genuine prompt (how long it's been working
            // solo, or how long it's been sitting idle since you last typed)
            // is the actually informative number for a live row. Kiro can't
            // derive this at all, and a session with no scan yet won't have it
            // either — fall back to the old plain "now" rather than a bare "—"
            // for what's still very much a live, just-created row.
            let when = if live_now {
                match s.last_prompt_at {
                    Some(t) => relative_time(Some(t), now),
                    None => "now".to_string(),
                }
            } else {
                relative_time(s.last_active, now)
            };
            // A 1-col rail, independent of recent/older and the live status
            // badge — it's the one thing that survives a session going
            // Idle/Ended and staying buried in "older".
            let rail = if app.state.is_in_progress(&s.id) {
                Span::styled("┃", Style::default().fg(IN_PROGRESS))
            } else {
                Span::raw(" ")
            };
            let measured = app.measured_usage(s);
            let usage = if measured.is_some() || s.context_pct.is_some() {
                format!(" · {}", usage_cell(s, measured))
            } else {
                String::new()
            };
            let extra_width = max_title.saturating_sub(12);
            let usage_width = Span::raw(&usage).width();
            let usage_width = if usage_width <= extra_width {
                usage_width
            } else {
                0
            };
            let cwd_budget = extra_width.saturating_sub(usage_width);
            let cwd = if app.list_multiple_cwds && cwd_budget >= 5 {
                let leaf = s
                    .cwd
                    .file_name()
                    .unwrap_or(s.cwd.as_os_str())
                    .to_string_lossy();
                format!(" · {}", truncate_cells(&leaf, (cwd_budget - 3).min(20)))
            } else {
                String::new()
            };
            let suffix_width = usage_width + Span::raw(&cwd).width();
            let title = truncate_cells(
                &format!("{}{title_suffix}", s.title),
                max_title - suffix_width,
            );
            let padding = max_title.saturating_sub(Span::raw(&title).width() + suffix_width);
            let mut spans = Vec::with_capacity(11);
            if app.multi_select {
                spans.push(Span::styled(mark_glyph, mark_style));
            }
            spans.push(rail);
            if show_provider {
                spans.push(Span::styled(
                    format!("{tag} "),
                    Style::default().fg(tag_color),
                ));
            }
            spans.push(Span::styled(
                &thread_prefix[1..],
                Style::default().fg(if connected { ACCENT } else { DIM }),
            ));
            spans.push(Span::styled(title, title_style));
            spans.push(Span::raw(" ".repeat(padding)));
            if usage_width > 0 {
                spans.push(Span::styled(
                    truncate_cells(&usage, usage_width),
                    Style::default().fg(DIM),
                ));
            }
            if !cwd.is_empty() {
                spans.push(Span::styled(cwd, Style::default().fg(DIM)));
            }
            spans.push(Span::raw("  "));
            spans.push(account_cell(
                account_marks.as_ref(),
                s,
                account_width,
                app.omp_account_label_for_source(s),
            ));
            if status_width > 0 {
                let label = if status_width == 2 {
                    format!("{} ", badge.chars().next().unwrap_or(' '))
                } else {
                    format!("{badge} ")
                };
                spans.push(Span::styled(label, badge_style));
            }
            if show_age {
                spans.push(Span::styled(
                    format!("{when:>4} "),
                    Style::default().fg(DIM),
                ));
            }
            if show_id {
                spans.push(Span::styled(
                    format!(" {}", short(&s.id)),
                    Style::default().fg(Color::Rgb(104, 185, 132)),
                ));
            }
            items.push(ListItem::new(Line::from(spans)).style(if connected {
                Style::default().bg(Color::Rgb(39, 47, 56))
            } else {
                Style::default()
            }));
        }
    }

    // Record the visible row count (inside the borders) so PageUp/PageDown can
    // step by a screenful.
    app.list_rows = area.height.saturating_sub(2);

    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(selected_item.map(|item| item - first_item));
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(ACCENT)
                .bg(Color::Rgb(44, 56, 88))
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
    let region_top = content_top + rendered_rows.min(viewport_rows) as u16;
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
                    "Run many Codex · Claude · Kiro · Cursor · OMP sessions like tabs",
                    Style::default().fg(DIM),
                )),
                Line::from(vec![
                    Span::styled("! blocked", Style::default().fg(WAITING_BORDER)),
                    Span::styled("   ● working", Style::default().fg(WORKING_BORDER)),
                    Span::styled("   ○ idle", Style::default().fg(UNCATEGORIZED_BORDER)),
                    Span::styled("   ✓ done", Style::default().fg(COMPLETED_BORDER)),
                    Span::styled("   ○ ended", Style::default().fg(ENDED_BORDER)),
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
    // Trace does not own a PTY. Clear its hit target before every render so a
    // just-hidden or resized view can never steal mouse input.
    app.observe_bounds = None;
    app.observe_prompt_bounds = None;
    app.observe_detail_bounds = None;
    if app.observer_open() {
        app.observe_bounds = Some((area.x, area.y, area.height, area.width));
        observe_panel(f, app, area);
        return;
    }
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

#[derive(Default)]
struct TraceTurn {
    prompt: String,
    prompt_observed: bool,
    events: Vec<String>,
    usage: Option<crate::observe::Usage>,
}

fn trace_turns(lines: &[String]) -> Vec<TraceTurn> {
    let mut turns: Vec<TraceTurn> = Vec::new();
    for line in lines {
        if let Some(prompt) = line.strip_prefix("user: ") {
            turns.push(TraceTurn {
                prompt: prompt.to_string(),
                prompt_observed: true,
                events: Vec::new(),
                usage: None,
            });
            continue;
        }
        if turns.is_empty() {
            turns.push(TraceTurn {
                prompt: "Earlier prompt omitted by bounded tail".to_string(),
                prompt_observed: false,
                events: Vec::new(),
                usage: None,
            });
        }
        if let Some(usage) = crate::observe::prompt_usage_from_line(line) {
            turns.last_mut().unwrap().usage = Some(usage);
            continue;
        }
        turns.last_mut().unwrap().events.push(line.clone());
    }
    turns.reverse();
    turns
}

/// Estimate only the text the user typed, rather than the full model request.
///
/// Provider logs expose request-level input usage but not the tokenized user
/// message. UTF-8 bytes divided by four is deliberately marked with `~`: it is
/// useful for comparing prompt text without pretending to be a provider count,
/// and behaves more reasonably for Korean than an ASCII character heuristic.
fn estimated_prompt_tokens(prompt: &str) -> u64 {
    let bytes = prompt.trim().len() as u64;
    if bytes == 0 {
        0
    } else {
        bytes.div_ceil(4)
    }
}

const BIG_DIGITS: [[&str; 5]; 10] = [
    ["███", "█ █", "█ █", "█ █", "███"],
    [" █ ", "██ ", " █ ", " █ ", "███"],
    ["███", "  █", "███", "█  ", "███"],
    ["███", "  █", "███", "  █", "███"],
    ["█ █", "█ █", "███", "  █", "  █"],
    ["███", "█  ", "███", "  █", "███"],
    ["███", "█  ", "███", "█ █", "███"],
    ["███", "  █", "  █", "  █", "  █"],
    ["███", "█ █", "███", "█ █", "███"],
    ["███", "█ █", "███", "  █", "███"],
];

fn big_token_lines(tokens: u64) -> Vec<Line<'static>> {
    let digits = tokens.to_string();
    (0..5)
        .map(|row| {
            let mut rendered = "  ".to_string();
            for digit in digits.bytes() {
                rendered.push_str(BIG_DIGITS[(digit - b'0') as usize][row]);
                rendered.push(' ');
            }
            if row == 2 {
                rendered.push_str(" TOKENS");
            }
            Line::from(Span::styled(
                rendered,
                Style::default()
                    .fg(Color::Rgb(237, 200, 121))
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect()
}

fn trace_event_line(line: &str) -> Line<'static> {
    let (kind, text, color) = if let Some(text) = line.strip_prefix("assistant: ") {
        ("ASSISTANT", text, Color::White)
    } else if line.starts_with("function_call ")
        || line.starts_with("custom_tool_call ")
        || line.starts_with("tool_use ")
    {
        ("TOOL", line, ACCENT)
    } else if line.starts_with("function_call_output:")
        || line.starts_with("custom_tool_output:")
        || line.starts_with("tool_result:")
    {
        ("RESULT", line, Color::Rgb(131, 210, 154))
    } else {
        ("EVENT", line, DIM)
    };
    Line::from(vec![
        Span::styled(
            format!("{kind:<10}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(text.to_string()),
    ])
}

/// Maximum vertical offset for Trace's execution pane, measured in rendered
/// terminal rows. A single event often wraps across many rows; counting only
/// logical `Line`s makes overflowing content appear unscrollable.
/// The EXECUTION block, shared by the render and the scroll range so the two agree on its chrome.
fn trace_detail_block() -> Block<'static> {
    Block::default()
        .padding(Padding::horizontal(2))
        .title(Span::styled(" EXECUTION ", Style::default().fg(DIM)))
}

fn trace_detail_max_scroll(detail: &[Line<'_>], width: u16, height: u16) -> usize {
    // Measured against the block's own inner area: its title takes a row the old arithmetic missed.
    let inner = trace_detail_block().inner(Rect::new(0, 0, width, height));
    Paragraph::new(detail.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(inner.width)
        .saturating_sub(inner.height as usize)
}

fn observe_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(id) = app.focused_pane().map(str::to_owned) else {
        return;
    };
    let Some((notice, observed_lines)) = app
        .observers
        .get(&id)
        .map(|observer| (observer.notice.clone(), observer.lines.clone()))
    else {
        return;
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .title(Line::from(vec![
            Span::styled(
                " TRACE ",
                Style::default()
                    .fg(Color::Black)
                    .bg(OBSERVER)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " FULL SCREEN · LIVE LOCAL LOG · READ ONLY ",
                Style::default().fg(OBSERVER).add_modifier(Modifier::BOLD),
            ),
        ]))
        .border_style(Style::default().fg(OBSERVER))
        .style(Style::default().bg(OBSERVER_BG));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let header = vec![
        Line::from(vec![
            Span::styled(
                "SOURCE  ",
                Style::default().fg(OBSERVER).add_modifier(Modifier::BOLD),
            ),
            Span::styled("focused session · ", Style::default().fg(DIM)),
            Span::styled(id, Style::default().fg(OBSERVER)),
        ]),
        Line::from(Span::styled(notice, Style::default().fg(DIM))),
    ];
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((header.len() as u16).min(inner.height.saturating_sub(3))),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);
    f.render_widget(Paragraph::new(header), parts[0]);

    let turns = trace_turns(&observed_lines);
    if turns.is_empty() {
        f.render_widget(
            Paragraph::new("Waiting for a recorded prompt…")
                .style(Style::default().fg(DIM))
                .alignment(Alignment::Center),
            parts[1],
        );
    } else {
        let selected = app.observe_turn.min(turns.len().saturating_sub(1));
        let (direction, constraints) = if parts[1].width >= 96 {
            (
                Direction::Horizontal,
                [Constraint::Percentage(32), Constraint::Percentage(68)],
            )
        } else {
            (
                Direction::Vertical,
                [
                    Constraint::Length(7.min(parts[1].height / 2)),
                    Constraint::Min(0),
                ],
            )
        };
        let columns = Layout::default()
            .direction(direction)
            .constraints(constraints)
            .split(parts[1]);
        app.observe_prompt_bounds = Some((
            columns[0].x,
            columns[0].y,
            columns[0].height,
            columns[0].width,
        ));
        app.observe_detail_bounds = Some((
            columns[1].x,
            columns[1].y,
            columns[1].height,
            columns[1].width,
        ));

        let rail = turns
            .iter()
            .enumerate()
            .map(|(index, turn)| {
                let marker = if index == selected { "▶" } else { " " };
                let style = if index == selected {
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Rgb(43, 53, 72))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(DIM)
                };
                let token_label = turn
                    .usage
                    .map(|usage| human_tokens(usage.total))
                    .unwrap_or_else(|| "…".to_string());
                let prefix = format!(
                    "{marker} #{:02} {token_label:>5}  ",
                    turns.len().saturating_sub(index)
                );
                let prompt_width = (columns[0].width as usize).saturating_sub(prefix.len() + 1);
                Line::from(Span::styled(
                    format!("{prefix}{}", truncate(&turn.prompt, prompt_width)),
                    style,
                ))
            })
            .collect::<Vec<_>>();
        f.render_widget(
            Paragraph::new(rail).block(
                Block::default()
                    .borders(Borders::RIGHT)
                    .border_style(Style::default().fg(Color::Rgb(65, 73, 88)))
                    .title(Span::styled(" PROMPTS ", Style::default().fg(DIM))),
            ),
            columns[0],
        );

        let turn = &turns[selected];
        let roomy_trace = columns[1].height >= 22;
        let mut detail = vec![
            Line::from(Span::styled(
                "SELECTED PROMPT · OBSERVED",
                Style::default().fg(OBSERVER).add_modifier(Modifier::BOLD),
            )),
            Line::from(turn.prompt.clone()),
            Line::from(""),
        ];
        if let Some(usage) = turn.usage {
            if roomy_trace {
                detail.push(Line::from(vec![
                    Span::styled(
                        "THIS TURN TOTAL  ",
                        Style::default().fg(OBSERVER).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            "{} TOKENS · {} MODEL CALL{} · PROVIDER RECORDED",
                            human_tokens(usage.total),
                            usage.calls,
                            if usage.calls == 1 { "" } else { "S" }
                        ),
                        Style::default()
                            .fg(Color::Rgb(237, 200, 121))
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                detail.extend(big_token_lines(usage.total));
                detail.push(Line::from(""));
                let mut usage_parts = vec![
                    format!("input {}", usage.input),
                    format!("cache {} (input subset)", usage.cached),
                    format!("output {}", usage.output),
                ];
                if usage.reasoning > 0 {
                    usage_parts.push(format!("reasoning {}", usage.reasoning));
                }
                if usage.latest > 0 {
                    usage_parts.push(format!("latest call {}", human_tokens(usage.latest)));
                }
                detail.push(Line::from(Span::styled(
                    usage_parts.join(" · "),
                    Style::default().fg(DIM),
                )));
            } else {
                detail.push(Line::from(Span::styled(
                    format!(
                        "THIS TURN TOTAL {} TOKENS · {} calls · latest {} · input {} · cache {} · output {} · reasoning {}",
                        human_tokens(usage.total),
                        usage.calls,
                        human_tokens(usage.latest),
                        usage.input,
                        usage.cached,
                        usage.output,
                        usage.reasoning
                    ),
                    Style::default().fg(DIM),
                )));
            }
        } else {
            detail.push(Line::from(Span::styled(
                if roomy_trace {
                    "THIS TURN TOTAL  waiting for provider usage record"
                } else {
                    "TURN USAGE waiting for provider usage record"
                },
                Style::default().fg(DIM),
            )));
        }
        if turn.prompt_observed {
            detail.push(Line::from(Span::styled(
                format!(
                    "YOUR TEXT  ~{} tokens · local estimate",
                    estimated_prompt_tokens(&turn.prompt)
                ),
                Style::default().fg(DIM),
            )));
        }
        if roomy_trace {
            detail.push(Line::from(""));
        }
        if turn.events.is_empty() {
            detail.push(Line::from(Span::styled(
                "No recorded events after this prompt yet.",
                Style::default().fg(DIM),
            )));
        } else {
            detail.extend(turn.events.iter().map(|event| trace_event_line(event)));
        }
        detail.push(Line::from(""));
        detail.push(Line::from(Span::styled(
            "Private reasoning is not displayed. Only recorded prompts, tool calls, results, and usage are shown.",
            Style::default().fg(DIM),
        )));
        let max_scroll = trace_detail_max_scroll(&detail, columns[1].width, columns[1].height);
        app.observe_scroll = app.observe_scroll.min(max_scroll);
        let offset = app.observe_scroll.min(u16::MAX as usize) as u16;
        f.render_widget(
            Paragraph::new(detail)
                .block(trace_detail_block())
                .wrap(Wrap { trim: false })
                .scroll((offset, 0)),
            columns[1],
        );
    }
    f.render_widget(
        Paragraph::new(
            "READ ONLY · Ctrl-G toggle · ↑/↓ prompt · Wheel: prompts select, execution scrolls · Esc back",
        )
            .style(Style::default().fg(DIM).bg(OBSERVER_BG)),
        parts[2],
    );
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
    // session name, with its colour confined to the title, so a stack is identifiable
    // without costing the grid a header row.
    cell: Option<(&str, Color)>,
) {
    let ended = app.ended.contains(sid);
    let (dot, dot_color) = pane_dot(app, sid, ended);
    let name = app.session_display_name(sid, pane_area.width.saturating_sub(20) as usize);
    let count = format!(" {}/{} ", idx + 1, total);
    let ended_label = if ended { "(ended) " } else { "" };
    // A plain-colored name blends into the border line and is easy to miss when
    // several panes are open — a bold, high-contrast chip (bg = the same color
    // as the status dot) makes "which session is this" readable at a glance
    // without stealing a content row from the terminal view.
    //
    // Focus uses a glyph and mirrored corners, never a status-colour override.
    // Status/counts come before truncatable category/name, even in a narrow pane.
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
            count.clone(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(ended_label, Style::default().fg(DIM)),
        if zoomed {
            let hidden = total.saturating_sub(1);
            let mut label = if hidden > 0 {
                format!(" 🔍 ZOOMED · {hidden} hidden ")
            } else {
                " 🔍 ZOOMED ".to_string()
            };
            let prefix_width = usize::from(pane_focused) * 2
                + Span::raw(dot).width()
                + Span::raw(&count).width()
                + Span::raw(ended_label).width();
            if prefix_width + Span::raw(&label).width()
                > usize::from(pane_area.width.saturating_sub(2))
            {
                label = " ZOOMED ".into();
            }
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
        if let Some((label, color)) = cell {
            Span::styled(
                format!("{label} "),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("")
        },
        Span::styled(
            format!(" {name} "),
            Style::default()
                .fg(Color::Rgb(15, 17, 22))
                .bg(dot_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    // Shape marks focus; colour always marks status, including in zoom.
    let (border_type, weight) = pane_border_for(pane_focused, zoomed);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .title(title)
        .border_style(
            Style::default()
                .fg(pane_border_color(app, sid, ended))
                .add_modifier(weight),
        );
    if let Some(provider) = pane_provider(app, sid) {
        // Use the existing bottom border: the account stays visible without taking
        // native terminal rows or competing with status/counts in the top title.
        let label = truncate_pane_account_label(
            if provider == Agent::Omp {
                app.ptys
                    .get(sid)
                    .and_then(|pty| pty.omp_account_label())
                    .unwrap_or("계정 미확인")
            } else {
                app.pane_account_label(sid).unwrap_or("계정 미확인")
            },
            usize::from(
                pane_area
                    .width
                    .saturating_sub(7 + provider.as_str().len() as u16),
            ),
        );
        block = block.title_bottom(Line::styled(
            format!(" {} · {label} ", provider.as_str()),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
    }
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
        render_pane_failure(
            f,
            reason,
            inner,
            pane_provider(app, sid) == Some(Agent::Omp),
        );
        return;
    }

    let selection = app.selection_for_pane(sid);
    if let Some(pty) = app.ptys.get_mut(sid) {
        // Resize before sampling the screen: the first zoom/resize frame must
        // use this pane's geometry, not cells from its previous dimensions.
        pty.resize(inner.height.max(1), inner.width.max(1));
        if let Ok(parser) = pty.parser().lock() {
            let screen = parser.render_screen();
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
                app.account_auth_message(sid).unwrap_or("starting..."),
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
const WAITING_BORDER: Color = Color::Rgb(255, 183, 131);
const WORKING_BORDER: Color = Color::Rgb(240, 212, 120);
const COMPLETED_BORDER: Color = Color::Rgb(145, 217, 174);
const ENDED_BORDER: Color = Color::Rgb(174, 183, 198);

/// Which border a pane gets, and whether it is emboldened.
///
/// Every pane gets a half-block frame — the heaviest border a terminal can draw
/// without spending a second row. Line-drawing glyphs, even `Thick`, read as
/// hairlines next to a full-height pane of text, which left the grid looking
/// like a wireframe.
///
/// That uniform weight costs the attention channel, so attention moves to
/// colour: each live state has a consistent colour (see `pane_dot`). The glyph in
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

/// Status owns the entire frame. Focus, zoom and category cannot override it.
fn pane_border_color(app: &App, sid: &str, ended: bool) -> Color {
    pane_dot(app, sid, ended).1
}

/// What a pane shows instead of an empty screen when its child failed to start.
fn render_pane_failure(f: &mut Frame, reason: &str, area: Rect, omp_pane: bool) {
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
        if omp_pane {
            "ctrl-q / alt-q closes this pane · enter on the row tries again"
        } else {
            "ctrl-q closes this pane · enter on the row tries again"
        },
        Style::default().fg(DIM),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn pane_dot(app: &App, sid: &str, ended: bool) -> (&'static str, Color) {
    if ended {
        return ("○ ", ENDED_BORDER);
    }
    match app.session_status(sid) {
        SessionStatus::Blocked => ("! ", WAITING_BORDER),
        SessionStatus::Working => ("● ", WORKING_BORDER),
        SessionStatus::Completed => ("✓ ", COMPLETED_BORDER),
        SessionStatus::Idle => ("○ ", UNCATEGORIZED_BORDER),
        SessionStatus::Ended => ("○ ", ENDED_BORDER),
        SessionStatus::Inactive => ("○ ", UNCATEGORIZED_BORDER),
    }
}

fn new_session_popup(f: &mut Frame, choice: usize, opts: &[String]) {
    // Two for the border, and as tall as the logins on offer.
    let height = (opts.len() as u16).saturating_add(2).max(3);
    let area = centered(f.area(), 40, height);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" New session ")
        .title_bottom(if opts.get(choice).is_some_and(|label| label == "omp") {
            " enter start · auth/model in OMP "
        } else {
            " enter start · l another account "
        });
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
    let mut selected_line = 0;
    for (i, account) in targets.iter().enumerate() {
        if last_provider != Some(account.provider) {
            lines.push(Line::from(Span::styled(
                account.provider.as_str().to_uppercase(),
                Style::default().fg(CATEGORY).add_modifier(Modifier::BOLD),
            )));
            last_provider = Some(account.provider);
        }
        let selected = i == choice;
        if selected {
            selected_line = lines.len();
        }
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
        let name = if account.provider == Agent::Omp {
            "omp"
        } else {
            &account.name
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{name}{suffix}"),
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
    let visible_rows = area.height.saturating_sub(2) as usize;
    let scroll = selected_line.saturating_add(1).saturating_sub(visible_rows) as u16;
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Handoff ");
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((scroll, 0))
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
        item("ctrl-g", "live pane: open full-screen Trace for the focused session"),
        item("esc / ctrl-g", "Trace: return to the live session"),
        item("up/down", "Trace: select prompt; wheel/page keys scroll events"),
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
        item("ctrl-r", "refresh account usage now"),
        item("r", "manually measure selected session tokens (— = unmeasured)"),
        item("a", "toggle archived sessions"),
        Line::from(""),
        section("Terminal / Modal"),
        item("ctrl-x", "return from terminal to session list"),
        item("tab / shift-tab", "next / previous live pane when 2+ open"),
        item("ctrl-w / alt-w", "cycle live panes (alt-w in OMP)"),
        item("ctrl-z", "zoom the focused pane full-size (toggle back to split)"),
        item("ctrl-q / alt-q", "close focused pane (alt-q also works in OMP)"),
        item(
            "ctrl-y",
            "copy a link from the focused pane's most recent answer outside OMP",
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

fn truncate_pane_account_label(label: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let safe: String = label
        .chars()
        .filter(|ch| {
            !ch.is_control()
                && !matches!(*ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect();
    let label = if safe.trim().is_empty() {
        "계정 미확인"
    } else {
        safe.trim()
    };
    let span = Span::raw(label);
    if span.width() <= max {
        return label.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for grapheme in span.styled_graphemes(Style::default()) {
        let cells = Span::raw(grapheme.symbol).width();
        if width + cells > max - 1 {
            break;
        }
        out.push_str(grapheme.symbol);
        width += cells;
    }
    out.push('…');
    out
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

/// Colors for a provider's logins in list order; the machine's own login stays quiet.
const ACCOUNT_TONES: [Color; 4] = [DIM, CATEGORY, PREVIEW, ZOOM];

/// Truncate by terminal cells without splitting a grapheme cluster.
fn truncate_cells(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let span = Span::raw(text);
    if span.width() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for grapheme in span.styled_graphemes(Style::default()) {
        let cells = Span::raw(grapheme.symbol).width();
        if width + cells > max - 1 {
            break;
        }
        out.push_str(grapheme.symbol);
        width += cells;
    }
    out.push('…');
    out
}

/// Managed source labels and observed native OMP identities share one column.
fn account_cell(
    marks: Option<&crate::app::accounts_panel::AccountMarks>,
    s: &Session,
    allocated_width: usize,
    omp_label: Option<&str>,
) -> Span<'static> {
    if allocated_width == 0 {
        return Span::raw("");
    }
    let width = allocated_width - 1;
    let account = if s.agent == Agent::Omp {
        Some((omp_label.unwrap_or("unverified"), 0))
    } else {
        marks.and_then(|marks| marks.label_for(s))
    };
    match account {
        Some((name, tone)) => {
            let label = truncate_cells(name, width);
            let padding = width.saturating_sub(Span::raw(&label).width()) + 1;
            Span::styled(
                format!("{label}{}", " ".repeat(padding)),
                Style::default().fg(if s.agent == Agent::Omp && omp_label.is_none() {
                    DIM
                } else {
                    ACCOUNT_TONES[tone % ACCOUNT_TONES.len()]
                }),
            )
        }
        None => Span::raw(" ".repeat(width + 1)),
    }
}

/// Manual measurements distinguish unknown from zero and disclose bounded reads.
fn usage_cell(s: &Session, measured: Option<&mindplayer_core::catalog::UsageSnapshot>) -> String {
    if s.agent == Agent::Kiro {
        return s
            .context_pct
            .map_or_else(|| "—".into(), |p| format!("ctx {p:.0}%"));
    }
    let Some(value) = measured else {
        return "—".into();
    };
    format!(
        "{}{} tok{}",
        if s.agent == Agent::Cursor { "~" } else { "" },
        human_tokens(value.tokens.total),
        if value.partial { " partial" } else { "" }
    )
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
    "v verify / bind account identity",
    "a add",
    "r rename",
    "l sign in",
    "d off",
    "x erase login",
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

/// The plan and identity state shown for a Claude account on the Accounts screen.
fn claude_identity_note(
    app: &App,
    account: &mindplayer_core::accounts::Account,
    rows: &[mindplayer_core::limits::QuotaRow],
) -> (String, Color) {
    let plan = app.claude_plan_label(account, rows);
    let (state, color) = if app.usage_login_changed(account, rows) {
        ("login changed · press v".to_string(), ERROR)
    } else if let Some(first) = app.same_subscription_as(account, rows, true) {
        (format!("same subscription as {first}"), ZOOM)
    } else if account.identity_fingerprint().is_some() {
        ("identity bound".to_string(), IDLE)
    } else {
        ("identity unverified".to_string(), ERROR)
    };
    (format!("{plan} · {state}"), color)
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
        let mut lines = vec![
            Line::from(Span::styled(prompt, Style::default().fg(DIM))),
            Line::from(Span::styled(
                format!("{name}▏"),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "letters, digits, - and _",
                Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
            )),
        ];
        // The list is hidden while a name is typed, so a refusal is drawn here.
        if let Some(error) = &panel.error {
            for row in wrapped_text_rows(error, 54) {
                lines.push(Line::from(Span::styled(row, Style::default().fg(ERROR))));
            }
        }
        let area = centered(f.area(), 58, lines.len() as u16 + 3);
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(lines).block(
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
                        Some(row) => spans.extend(quota_row_spans(row, 9, None)),
                        None => spans.push(Span::styled(
                            format!("{:<9}   ─", ""),
                            Style::default().fg(DIM),
                        )),
                    }
                    spans.push(Span::styled(
                        format!("   {activity}{origin}"),
                        Style::default().fg(DIM),
                    ));
                    let mut note = None;
                    if account.provider == Agent::Claude {
                        note = Some(claude_identity_note(app, account, &all_rows));
                    } else if matches!(account.provider, Agent::Codex) {
                        spans.push(Span::styled(
                            format!(
                                "   {}",
                                account.identity_fingerprint().map_or(
                                    "identity unverified",
                                    |id| {
                                        if app.accounts.iter().any(|other| {
                                            other.provider == account.provider
                                                && other.name != account.name
                                                && other.identity_fingerprint() == Some(id)
                                        }) {
                                            "identity shared"
                                        } else {
                                            "identity bound"
                                        }
                                    }
                                )
                            ),
                            Style::default().fg(if account.identity_fingerprint().is_some() {
                                IDLE
                            } else {
                                ERROR
                            }),
                        ));
                    }
                    // The note sits on the row when it fits and under the name when it does not, so it is never cut off.
                    let mut note_below = None;
                    if let Some((text, color)) = note {
                        let inline: usize =
                            spans.iter().map(Span::width).sum::<usize>() + 3 + text.chars().count();
                        if inline + 3 <= usize::from(f.area().width) {
                            spans.push(Span::styled(
                                format!("   {text}"),
                                Style::default().fg(color),
                            ));
                        } else {
                            note_below = Some((text, color));
                        }
                    }
                    lines.push(Line::from(spans));
                    if let Some((text, color)) = note_below {
                        lines.push(Line::from(vec![
                            Span::raw(format!("    {:<name_width$}  {:<8}  ", "", "")),
                            Span::styled(text, Style::default().fg(color)),
                        ]));
                    }
                    // A provider can meter more than one window (Claude's five
                    // hour and weekly), and hiding the second would hide the
                    // one that is actually binding.
                    for row in usage.iter().skip(1) {
                        let mut extra =
                            vec![Span::raw(format!("    {:<name_width$}  {:<8}  ", "", ""))];
                        extra.extend(quota_row_spans(row, 9, None));
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
        for row in wrapped_text_rows(error, f.area().width.saturating_sub(4) as usize) {
            lines.push(Line::from(Span::styled(row, Style::default().fg(ERROR))));
        }
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
#[path = "list_perf_tests.rs"]
mod list_perf_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::PtySession;
    use std::path::Path;

    fn startup_app() -> App {
        let mut app = isolated_app();
        app.screen = Screen::Main;
        app.focus = Focus::List;

        app.cwd = PathBuf::from("/work");
        app.accounts = vec![mindplayer_core::accounts::Account::inherited(Agent::Codex)];
        app.limits = Some(Vec::new());
        app.quota_cache = None;
        app.all_sessions = [Agent::Codex, Agent::Claude, Agent::Kiro, Agent::Cursor]
            .into_iter()
            .enumerate()
            .map(|(index, agent)| {
                let mut session = session(agent, 0);
                session.id = format!("startup-{index}");
                session.title = format!("real-{}", agent.as_str());
                session.last_active = Some(Utc::now());
                session
            })
            .collect();
        app.rebuild_visible();
        app
    }

    fn startup_frame(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn frame_text(buffer: &ratatui::buffer::Buffer) -> String {
        (buffer.area.y..buffer.area.bottom())
            .map(|row| rendered_row(buffer, row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn list_fixture(ids: &[&str]) -> App {
        let mut app = isolated_app();
        app.focus = Focus::List;
        app.all_sessions = ids
            .iter()
            .map(|id| {
                let mut source = session(Agent::Claude, 0);
                source.id = (*id).into();
                source.title = (*id).into();
                source.file = PathBuf::from(format!("/work/{id}"));
                source.last_active = Some(Utc::now());
                source
            })
            .collect();
        app.rebuild_visible();
        app
    }

    fn list_frame(app: &mut App, height: u16) -> ratatui::buffer::Buffer {
        list_frame_at(app, height, Utc::now())
    }

    fn list_frame_at(app: &mut App, height: u16, now: DateTime<Utc>) -> ratatui::buffer::Buffer {
        use ratatui::{backend::TestBackend, Terminal};
        app.rebuild_list_items();
        let mut terminal = Terminal::new(TestBackend::new(140, height)).unwrap();
        terminal
            .draw(|frame| session_list(frame, app, frame.area(), now))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn list_title_row(buffer: &ratatui::buffer::Buffer, title: &str) -> String {
        frame_text(buffer)
            .lines()
            .find(|line| line.contains(title))
            .unwrap_or_else(|| panic!("missing title {title}: {}", frame_text(buffer)))
            .to_string()
    }

    fn link_list_child(app: &mut App, child: &str, parent: &str) {
        app.state
            .set_handoff_link(child, parent, PathBuf::from("/work/handoff.md"), Utc::now());
    }

    #[test]
    fn session_list_counts_sessions_across_groups_filters_and_folds() {
        let mut app = list_fixture(&["match-first", "match-second", "other"]);
        let first = app
            .state
            .create_category("first-topic", Utc::now())
            .unwrap();
        let second = app
            .state
            .create_category("second-topic", Utc::now())
            .unwrap();
        app.state.assign_category("match-first", &first);
        app.state.assign_category("match-second", &first);
        app.state.assign_category("other", &second);

        for (query, folded, sessions, headers) in [
            (None, false, 3, 2),
            (Some("match"), false, 2, 1),
            (Some("match-first"), false, 1, 1),
            (None, true, 1, 2),
            (Some("match"), true, 0, 1),
            (Some("absent"), false, 0, 0),
            (None, false, 3, 2),
        ] {
            app.search_query = query.map(str::to_string);
            app.state.set_collapsed(&first, folded);
            app.rebuild_visible();
            assert_eq!(app.visible_aggregate.session_count(), sessions);
            assert_eq!(
                app.visible.iter().filter(|row| row.is_header()).count(),
                headers
            );
            let buffer = list_frame(&mut app, 12);
            let header = rendered_row(&buffer, 0);
            let footer = rendered_row(&buffer, 11);
            assert!(
                header.contains(&format!("({sessions})")),
                "query={query:?}, folded={folded}: {header}"
            );
            assert!(
                footer.contains(&format!("{sessions} visible / 0 running")),
                "query={query:?}, folded={folded}: {footer}"
            );
        }
    }

    #[test]
    fn list_saved_status_is_visible_without_claiming_live_activity() {
        let mut app = list_fixture(&["saved-source"]);
        // A fresh transcript is not evidence of a process owned by this instance.
        for last_active in [None, Some(Utc::now())] {
            app.all_sessions[0].last_active = last_active;
            app.rebuild_visible();
            let buffer = list_frame(&mut app, 8);
            let row = list_title_row(&buffer, "saved-source");
            for live in ["working", "idle", "blocked", "done", "ended"] {
                assert!(!row.contains(live), "unowned source claims {live}: {row}");
            }
            assert_eq!(
                app.session_status_for(&app.all_sessions[0]),
                SessionStatus::Inactive
            );
        }
        app.all_sessions.clear();
        app.rebuild_visible();
        assert!(!frame_text(&list_frame(&mut app, 8)).contains("saved-source"));
    }

    #[test]
    fn list_status_preserves_owned_states_and_rejects_source_collisions() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut fixture = QuietPtyFixture::new(8, 40);
        let id = fixture.app.panes[0].clone();
        let mut source = session(Agent::Claude, 0);
        source.id = id.clone();
        source.title = "owned-source".into();
        fixture.app.all_sessions = vec![source.clone()];
        fixture.app.focus = Focus::List;
        fixture.app.rebuild_visible();
        let mut provider_column = None;
        for (status, badge, glyph) in [
            (SessionStatus::Working, "working", '●'),
            (SessionStatus::Idle, "idle", '○'),
            (SessionStatus::Blocked, "blocked", '!'),
            (SessionStatus::Completed, "done", '✓'),
            (SessionStatus::Ended, "ended", '○'),
        ] {
            fixture.app.hook_status.insert(
                id.clone(),
                crate::agent_hooks::HookReading {
                    status,
                    event_epoch_secs: 1,
                    revision: 0,
                },
            );
            assert_eq!(fixture.app.session_status_for(&source), status);
            let row = list_title_row(&list_frame(&mut fixture.app, 8), "owned-source");
            assert!(row.contains(badge), "{row}");
            let column = Span::raw(&row[..row.find("claude").unwrap()]).width();
            assert_eq!(*provider_column.get_or_insert(column), column);
            fixture.app.multi_select = true;
            let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
            terminal
                .draw(|frame| session_list(frame, &mut fixture.app, frame.area(), Utc::now()))
                .unwrap();
            let row = list_title_row(terminal.backend().buffer(), "owned-source");
            assert!(row.contains(glyph), "{row}");
            fixture.app.multi_select = false;
        }
        let mut collision = source.clone();
        collision.file = PathBuf::from("/other-account/session");
        collision.title = "collision-source".into();
        fixture.app.all_sessions.push(collision);
        fixture.app.rebuild_visible();
        for source in &fixture.app.all_sessions {
            assert_eq!(
                fixture.app.session_status_for(source),
                SessionStatus::Inactive
            );
        }
        let buffer = list_frame(&mut fixture.app, 8);
        for title in ["owned-source", "collision-source"] {
            let row = list_title_row(&buffer, title);
            assert!(
                !row.contains("ended"),
                "collision borrowed live status: {row}"
            );
            let column = Span::raw(&row[..row.find("claude").unwrap()]).width();
            assert_eq!(Some(column), provider_column);
        }
    }

    #[test]
    fn list_handoff_timestamps_stay_source_local_while_groups_sort_by_descendants() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let mut app = list_fixture(&[
            "old-parent",
            "recent-child",
            "old-sibling",
            "recent-grandchild",
            "unknown-child",
            "loose-session",
        ]);
        for (source, age) in app.all_sessions.iter_mut().zip([
            Some(4 * 24 * 60),
            Some(7),
            Some(2 * 24 * 60),
            Some(3),
            None,
            Some(24 * 60),
        ]) {
            source.last_active = age.map(|minutes| now - chrono::Duration::minutes(minutes));
            source.last_prompt_at = Some(now - chrono::Duration::hours(1));
        }
        for (child, parent) in [
            ("recent-child", "old-parent"),
            ("old-sibling", "old-parent"),
            ("unknown-child", "old-parent"),
            ("recent-grandchild", "recent-child"),
        ] {
            app.state
                .set_handoff_link(child, parent, PathBuf::new(), now);
        }
        app.rebuild_visible();
        let buffer = list_frame_at(&mut app, 14, now);
        let loose = app.row_of_session("loose-session").unwrap();
        for source in &app.all_sessions {
            let expected = relative_time(source.last_active, now);
            let row = list_title_row(&buffer, &source.title);
            assert!(
                row.split_whitespace().any(|word| word == expected),
                "{} must show its own activity {expected}: {row}",
                source.id
            );
            if source.id != "loose-session" {
                assert!(
                    app.row_of_session(&source.id).unwrap() < loose,
                    "the recent descendant must still lift the whole thread"
                );
            }
        }
    }

    #[test]
    fn list_handoff_live_timestamp_uses_only_the_owned_source_prompt() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let mut fixture = QuietPtyFixture::new(8, 40);
        let child = fixture.app.panes[0].clone();
        let app = &mut fixture.app;
        app.focus = Focus::List;
        app.all_sessions = ["old-parent", child.as_str(), "old-sibling"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                let mut source = session(Agent::Claude, 0);
                source.id = id.into();
                source.title = id.into();
                source.last_active = Some(now - chrono::Duration::days(index as i64 + 2));
                source.last_prompt_at = Some(now - chrono::Duration::minutes(47));
                source
            })
            .collect();
        app.all_sessions[1].last_active = Some(now - chrono::Duration::minutes(1));
        for id in [child.as_str(), "old-sibling"] {
            app.state
                .set_handoff_link(id, "old-parent", PathBuf::new(), now);
        }
        for ended in [false, true] {
            if ended {
                app.ended.insert(child.clone());
            }
            app.rebuild_visible();
            let buffer = list_frame_at(app, 12, now);
            for source in &app.all_sessions {
                let timestamp = if source.id == child && !ended {
                    source.last_prompt_at
                } else {
                    source.last_active
                };
                let expected = relative_time(timestamp, now);
                let row = list_title_row(&buffer, &source.title);
                assert!(
                    row.split_whitespace().any(|word| word == expected),
                    "ended={ended}, {} must show {expected}: {row}",
                    source.id
                );
            }
        }
        app.ended.remove(&child);
        app.all_sessions[1].last_prompt_at = None;
        let buffer = list_frame_at(app, 12, now);
        let row = list_title_row(&buffer, &child);
        let expected = relative_time(Some(now), now);
        assert!(row.split_whitespace().any(|word| word == expected), "{row}");

        let mut collision = app.all_sessions[1].clone();
        collision.file = PathBuf::from("/other-account/collision");
        collision.title = "colliding-child".into();
        collision.last_active = Some(now - chrono::Duration::days(6));
        app.all_sessions.push(collision);
        app.rebuild_visible();
        let buffer = list_frame_at(app, 14, now);
        for source in &app.all_sessions {
            let expected = relative_time(source.last_active, now);
            let row = list_title_row(&buffer, &source.title);
            assert!(
                row.split_whitespace().any(|word| word == expected),
                "ambiguous runtime ownership must leave source activity {expected}: {row}"
            );
        }
    }

    #[test]
    fn list_handoff_siblings_use_branch_then_final_connectors() {
        let mut app = list_fixture(&["tree-parent", "lane-first", "lane-last"]);
        link_list_child(&mut app, "lane-first", "tree-parent");
        link_list_child(&mut app, "lane-last", "tree-parent");
        app.all_sessions[1].last_active = Some(Utc::now());
        app.all_sessions[2].last_active = None;
        app.rebuild_visible();
        let buffer = list_frame(&mut app, 10);
        assert!(list_title_row(&buffer, "lane-first").contains("├─ lane-first"));
        assert!(list_title_row(&buffer, "lane-last").contains("└─ lane-last"));
        assert!(list_title_row(&buffer, "tree-parent").contains("[2 lanes]"));
        assert!(list_title_row(&buffer, "tree-parent").contains("┌─"));
        let thread_backgrounds: Vec<_> = (1..buffer.area.height - 1)
            .filter(|&y| {
                let text = rendered_row(&buffer, y);
                text.contains("tree-parent")
                    || text.contains("lane-first")
                    || text.contains("lane-last")
            })
            .filter(|&y| buffer[(1, y)].symbol() != "▶")
            .map(|y| buffer[(8, y)].bg)
            .collect();
        assert!(thread_backgrounds
            .iter()
            .all(|color| *color == Color::Rgb(39, 47, 56)));
    }

    #[test]
    fn list_title_and_identity_columns_are_cell_aligned_at_supported_widths() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = list_fixture(&["identity-first", "identity-second"]);
        app.all_sessions[0].title = "한글🙂e\u{301}".repeat(40);
        app.all_sessions[1].title = "short title".into();
        app.rebuild_visible();
        for width in [40, 45, 58, 80, 120, 144, 220] {
            let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
            terminal
                .draw(|frame| session_list(frame, &mut app, frame.area(), Utc::now()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let provider_cells: Vec<_> = (1..11)
                .filter_map(|y| {
                    let row = rendered_row(buffer, y);
                    row.contains("claude").then(|| {
                        (1..width - 1)
                            .find(|&x| {
                                buffer[(x, y)].symbol() == "c" && buffer[(x + 1, y)].symbol() == "l"
                            })
                            .unwrap()
                    })
                })
                .collect();
            assert_eq!(provider_cells.len(), 2, "{}", frame_text(buffer));
            assert!(
                provider_cells.iter().all(|&column| column < 8),
                "provider identity must stay in the left gutter at width {width}:\n{}",
                frame_text(buffer)
            );
            assert_eq!(
                provider_cells[0],
                provider_cells[1],
                "{}",
                frame_text(buffer)
            );
            assert!(frame_text(buffer).contains("short title"));
            if width >= 58 {
                assert_eq!(frame_text(buffer).matches("identity").count(), 2);
            }
        }
        for max in 0..12 {
            assert!(Span::raw(truncate_cells("한글🙂e\u{301}", max)).width() <= max);
        }
    }

    #[test]
    fn list_account_cells_keep_full_lookup_identity_and_unicode_width() {
        use mindplayer_core::accounts::Account;
        let mut app = list_fixture(&["account-a", "account-b"]);
        app.accounts = ["업무-계정", "personal-account-full"]
            .into_iter()
            .map(|name| {
                let mut account = Account::inherited(Agent::Claude);
                account.name = name.into();
                account
            })
            .collect();
        for (id, name) in [
            ("account-a", "업무-계정"),
            ("account-b", "personal-account-full"),
        ] {
            app.pane_accounts
                .insert(id.into(), (Agent::Claude, name.into()));
        }
        let marks = app.account_marks().expect("two account labels");
        assert_eq!(
            marks.label_for(&app.all_sessions[1]).unwrap().0,
            "personal-account-full"
        );
        let first = account_cell(Some(&marks), &app.all_sessions[0], marks.width() + 1, None);
        let second = account_cell(Some(&marks), &app.all_sessions[1], marks.width() + 1, None);
        assert_eq!(first.width(), second.width());
        assert_eq!(first.width(), marks.width() + 1);
        assert!(first.content.contains("업무-계정"));
        assert!(second.content.contains('…'));
        assert_ne!(first.style.fg, second.style.fg);
        for width in [80, 120, 144] {
            use ratatui::{backend::TestBackend, Terminal};
            let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
            terminal
                .draw(|frame| session_list(frame, &mut app, frame.area(), Utc::now()))
                .unwrap();
            let text = frame_text(terminal.backend().buffer());
            assert!(
                text.contains("업무-계정") && text.contains("personal-ac"),
                "{text}"
            );
        }
    }

    #[test]
    fn a_closed_omp_row_shows_the_account_its_transcript_last_answered_with() {
        let mut app = list_fixture(&["omp-answered", "omp-never-observed"]);
        for source in &mut app.all_sessions {
            source.agent = Agent::Omp;
        }
        let answered = app.all_sessions[0].clone();
        assert!(app.state.record_omp_account(
            &answered.id,
            &answered.file,
            "person@example.test · Workspace · provider"
        ));
        app.rebuild_visible();
        let buffer = list_frame(&mut app, 12);
        let row = list_title_row(&buffer, "omp-answered");
        assert!(row.contains("person@"), "{row}");
        assert!(!row.contains("unverified"), "{row}");
        let never = list_title_row(&buffer, "omp-never-observed");
        assert!(never.contains("unverified"), "{never}");

        let same_id_other_provider = Session {
            agent: Agent::Claude,
            ..answered
        };
        assert_eq!(
            app.omp_account_label_for_source(&same_id_other_provider),
            None
        );
    }

    #[test]
    fn a_running_omp_pane_answers_for_itself_and_a_stopped_one_falls_back_to_its_transcript() {
        let mut fixture = QuietPtyFixture::new(8, 40);
        let id = fixture.app.panes[0].clone();
        let app = &mut fixture.app;
        app.focus = Focus::List;
        let mut source = session(Agent::Omp, 0);
        source.id = id.clone();
        source.title = "omp-running".into();
        source.file = PathBuf::from("/work/omp-running.jsonl");
        source.last_active = Some(Utc::now());
        app.all_sessions = vec![source.clone()];
        let stored = "previous@example.test · provider";
        assert!(app
            .state
            .record_omp_account(&source.id, &source.file, stored));
        app.rebuild_visible();

        assert_eq!(app.omp_account_label_for_source(&source), None);
        let row = list_title_row(&list_frame(app, 10), "omp-running");
        assert!(row.contains("unverified"), "{row}");

        app.ended.insert(id.clone());
        assert_eq!(app.omp_account_label_for_source(&source), Some(stored));
        app.ended.remove(&id);
        app.ptys.remove(&id);
        assert_eq!(app.omp_account_label_for_source(&source), Some(stored));
        let row = list_title_row(&list_frame(app, 10), "omp-running");
        assert!(row.contains("previous@"), "{row}");
    }

    #[test]
    fn narrow_mixed_account_rows_keep_titles_after_usage_measurement() {
        use mindplayer_core::accounts::Account;
        use ratatui::{backend::TestBackend, Terminal};
        use std::time::{Duration, Instant};

        let mut fixture = QuietPtyFixture::new(8, 40);
        fixture.state_dir = std::env::temp_dir().join(format!(
            "mindplayer-narrow-usage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&fixture.state_dir).unwrap();
        fixture.app.state_path = fixture.state_dir.join("state.json");
        fixture.app.audit_path = fixture.state_dir.join("audit.jsonl");
        let mut first = session(Agent::Claude, 0);
        first.id = fixture.app.panes[0].clone();
        first.title = "FIRST-TASK".into();
        first.file = fixture.state_dir.join("source.jsonl");
        first.cwd = fixture.state_dir.clone();
        std::fs::write(
            &first.file,
            serde_json::json!({
                "type": "assistant",
                "sessionId": first.id,
                "cwd": first.cwd,
                "timestamp": Utc::now().to_rfc3339(),
                "message": {"usage": {"input_tokens": 1000, "output_tokens": 200}}
            })
            .to_string(),
        )
        .unwrap();
        let mut second = first.clone();
        second.id = "second-source".into();
        second.title = "SECOND-TASK".into();
        fixture.app.accounts = ["account-first-long", "account-second-long"]
            .into_iter()
            .map(|name| {
                let mut account = Account::inherited(Agent::Claude);
                account.name = name.into();
                account
            })
            .collect();
        for (source, account) in [
            (&first, "account-first-long"),
            (&second, "account-second-long"),
        ] {
            fixture
                .app
                .pane_accounts
                .insert(source.id.clone(), (Agent::Claude, account.into()));
        }
        fixture.app.all_sessions = vec![first, second];
        fixture.app.focus = Focus::List;
        fixture.app.rebuild_visible();
        fixture.app.measure_selected_usage();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fixture
            .app
            .measured_usage(&fixture.app.all_sessions[0])
            .is_none()
        {
            fixture.app.poll_manual_usage();
            assert!(Instant::now() < deadline, "{}", fixture.app.status);
            std::thread::sleep(Duration::from_millis(1));
        }
        for width in [40, 45, 58, 80] {
            let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
            terminal
                .draw(|frame| session_list(frame, &mut fixture.app, frame.area(), Utc::now()))
                .unwrap();
            let text = frame_text(terminal.backend().buffer());
            assert!(
                text.contains("FIRST-TASK") && text.contains("SECOND-TASK"),
                "{width}: {text}"
            );
        }
    }

    #[test]
    fn narrow_provider_quotas_preserve_complete_amounts_and_reset_times() {
        use mindplayer_core::limits::QuotaRow;
        let mut app = startup_app();

        app.limits = Some(vec![
            QuotaRow {
                label: "cursor included".into(),
                agent: Agent::Cursor,
                used_percent: Some(0.3),
                detail: "$0.06/$20".into(),
                ..Default::default()
            },
            QuotaRow {
                label: "codex weekly".into(),
                agent: Agent::Codex,
                used_percent: Some(32.0),
                resets: Some("10-22 15:14".into()),
                ..Default::default()
            },
        ]);
        for width in [40, 43, 80] {
            let text = frame_text(&startup_frame(&mut app, width, 40));
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(flat.contains("$0.06/$20"), "{width}: {text}");
            assert!(flat.contains("10-22 15:14"), "{width}: {text}");
        }
    }

    #[test]
    fn quota_footer_separates_accounts_but_packs_their_own_windows() {
        use mindplayer_core::{accounts::Account, limits::QuotaRow};
        let mut app = startup_app();

        app.accounts = ["work", "personal"]
            .into_iter()
            .map(|name| {
                let mut account = Account::inherited(Agent::Codex);
                account.name = name.into();
                account
            })
            .collect();
        app.limits = Some(vec![
            QuotaRow {
                agent: Agent::Codex,
                account: "work".into(),
                label: "codex weekly".into(),
                used_percent: Some(100.0),
                detail: "blocked".into(),
                blocked: true,
                ..Default::default()
            },
            QuotaRow {
                agent: Agent::Codex,
                account: "work".into(),
                label: "codex 5h".into(),
                used_percent: Some(12.0),
                ..Default::default()
            },
            QuotaRow {
                agent: Agent::Codex,
                account: "personal".into(),
                label: "codex weekly".into(),
                used_percent: Some(36.0),
                ..Default::default()
            },
        ]);
        for width in [80, 144, 220] {
            let text = frame_text(&startup_frame(&mut app, width, 40));
            let work = text
                .lines()
                .find(|line| line.contains("work weekly"))
                .unwrap();
            let personal = text
                .lines()
                .find(|line| line.contains("personal weekly"))
                .unwrap();
            assert!(!work.contains("personal"), "{width}: {text}");
            assert!(!personal.contains("work"), "{width}: {text}");
            assert!(
                work.contains("100.0%") && work.contains("blocked"),
                "{text}"
            );
            assert!(personal.contains("36.0%"), "{text}");
            if width >= 144 {
                assert!(work.contains("work 5h"), "{width}: {text}");
            }
        }
    }

    fn claude_row(
        account: &str,
        label: &str,
        identity: Option<&str>,
        plan: Option<&str>,
    ) -> mindplayer_core::limits::QuotaRow {
        mindplayer_core::limits::QuotaRow {
            agent: Agent::Claude,
            account: account.into(),
            label: label.into(),
            used_percent: Some(41.0),
            resets: Some("14:20".into()),
            identity: identity.map(str::to_string),
            plan: plan.map(str::to_string),
            ..Default::default()
        }
    }

    fn claude_accounts() -> Vec<mindplayer_core::accounts::Account> {
        use mindplayer_core::accounts::Account;
        let home = PathBuf::from("/fixture/home");
        let mut default = Account::inherited(Agent::Claude);
        default.set_identity_fingerprint(Some("fp-a".into()));
        default.set_claude_login_facts("org-a", "claude_team");
        let mut second = Account::isolated(&home, Agent::Claude, "second").unwrap();
        second.set_identity_fingerprint(Some("fp-a".into()));
        let mut third = Account::isolated(&home, Agent::Claude, "third").unwrap();
        third.set_identity_fingerprint(Some("fp-c".into()));
        let fourth = Account::isolated(&home, Agent::Claude, "fourth").unwrap();
        vec![default, second, third, fourth]
    }

    #[test]
    fn accounts_panel_shows_plans_and_flags_a_shared_or_changed_subscription() {
        for width in [80, 120] {
            let mut app = startup_app();
            app.accounts = claude_accounts();
            app.limits = Some(vec![
                claude_row("default", "claude 5h", Some("fp-a"), Some("claude_team")),
                claude_row("second", "claude 5h", Some("fp-a"), Some("claude_max")),
                claude_row("third", "claude 5h", Some("fp-live-other"), None),
                claude_row("fourth", "claude 5h", None, Some("claude_enterprise")),
            ]);
            app.open_accounts();
            let text = frame_text(&startup_frame(&mut app, width, 45));
            for needle in [
                "team · identity bound",
                "max · same subscription as default",
                "unknown plan · login changed · press v",
                "enterprise · identity unverified",
            ] {
                assert!(text.contains(needle), "{width}: {needle}\n{text}");
            }
            assert!(!text.contains("identity shared"), "{width}: {text}");
        }
    }

    #[test]
    fn accounts_panel_flags_a_shared_subscription_from_stored_bindings_alone() {
        let mut app = startup_app();
        app.accounts = claude_accounts();
        app.limits = Some(Vec::new());
        app.open_accounts();
        let text = frame_text(&startup_frame(&mut app, 120, 45));
        assert!(text.contains("same subscription as default"), "{text}");
        assert!(text.contains("unknown plan"), "{text}");
    }

    #[test]
    fn footer_names_plans_and_marks_an_account_on_an_earlier_accounts_subscription() {
        let mut app = startup_app();
        app.accounts = claude_accounts().into_iter().take(2).collect();
        app.limits = Some(vec![
            claude_row("default", "claude 5h", Some("fp-a"), Some("claude_team")),
            claude_row("default", "claude wk", Some("fp-a"), Some("claude_team")),
            claude_row("second", "claude 5h", Some("fp-a"), Some("claude_max")),
            claude_row("second", "claude wk", Some("fp-a"), Some("claude_max")),
        ]);
        for width in [80, 144] {
            let text = frame_text(&startup_frame(&mut app, width, 40));
            let default = text
                .lines()
                .find(|l| l.contains("default (team) 5h"))
                .unwrap();
            let second = text
                .lines()
                .find(|l| l.contains("second (max) 5h"))
                .unwrap();
            assert!(!default.contains("= "), "{width}: {text}");
            assert!(second.contains("= default"), "{width}: {text}");
        }
    }

    #[test]
    fn footer_rows_with_long_names_keep_figures_and_mark_on_one_line_at_80_columns() {
        use mindplayer_core::accounts::Account;
        let home = PathBuf::from("/fixture/home");
        let mut app = startup_app();
        app.accounts = ["firstname1", "second-long1"]
            .into_iter()
            .map(|name| Account::isolated(&home, Agent::Claude, name).unwrap())
            .collect();
        app.limits = Some(vec![
            claude_row(
                "firstname1",
                "claude 5h",
                Some("fp-a"),
                Some("claude_enterprise"),
            ),
            claude_row(
                "second-long1",
                "claude 5h",
                Some("fp-a"),
                Some("claude_enterprise"),
            ),
        ]);
        for (width, plan_shown) in [(80, false), (144, true)] {
            let text = frame_text(&startup_frame(&mut app, width, 40));
            let row = text
                .lines()
                .find(|l| l.contains("second-long1"))
                .unwrap_or_else(|| panic!("{width}: {text}"));
            for needle in ["%", "resets", "= firstname1"] {
                assert!(
                    row.contains(needle),
                    "{width}: {needle} not on {row:?}\n{text}"
                );
            }
            assert_eq!(row.contains("(enterprise)"), plan_shown, "{width}: {text}");
            assert_eq!(
                text.lines().filter(|l| l.contains("second-long1")).count(),
                1,
                "{width}: the row wrapped\n{text}"
            );
        }
    }

    #[test]
    fn footer_keeps_a_single_login_bare_and_marks_nothing_for_different_logins() {
        let mut app = startup_app();
        app.accounts = claude_accounts().into_iter().take(1).collect();
        app.limits = Some(vec![claude_row(
            "default",
            "claude 5h",
            Some("fp-a"),
            Some("claude_team"),
        )]);
        let text = frame_text(&startup_frame(&mut app, 100, 40));
        assert!(!text.contains("(team)"), "{text}");
        let mut app = startup_app();
        app.accounts = claude_accounts().into_iter().take(2).collect();
        app.limits = Some(vec![
            claude_row("default", "claude 5h", Some("fp-a"), Some("claude_team")),
            claude_row("second", "claude 5h", Some("fp-b"), None),
        ]);
        let text = frame_text(&startup_frame(&mut app, 100, 40));
        assert!(text.contains("default (team) 5h"), "{text}");
        assert!(!text.contains("= default"), "{text}");
        assert!(!text.contains("unknown plan"), "{text}");
    }

    #[test]
    fn scan_loading_and_retry_errors_keep_their_true_state_on_narrow_screens() {
        let mut app = startup_app();
        for width in [40, 80, 144] {
            app.screen = Screen::Scanning;
            let text = frame_text(&startup_frame(&mut app, width, 24));
            assert!(text.contains("Sessions · loading"), "{text}");
            assert!(!text.contains("Collected"), "{text}");
            app.screen = Screen::ScopeSelect;
            app.status = "Session collection failed: catalog unavailable · enter retry".into();
            let text = frame_text(&startup_frame(&mut app, width, 24));
            assert!(text.contains("collection failed"), "{text}");
            assert!(
                text.contains("enter retry") && text.contains("q quit"),
                "{text}"
            );
        }
    }

    #[test]
    fn list_handoff_invisible_parents_never_attach_to_the_previous_row() {
        for hidden in ["missing", "archived", "filtered", "subagent"] {
            let mut app = list_fixture(&["hidden-parent", "visible-child", "unrelated"]);
            link_list_child(&mut app, "visible-child", "hidden-parent");
            match hidden {
                "missing" => {
                    app.all_sessions.remove(0);
                }
                "archived" => app.all_sessions[0].archived = true,
                "filtered" => app.search_query = Some("visible-child".into()),
                "subagent" => app.all_sessions[0].is_subagent = true,
                _ => unreachable!(),
            }
            app.rebuild_visible();
            let buffer = list_frame(&mut app, 10);
            let row = list_title_row(&buffer, "visible-child");
            assert!(
                !row.contains("└─") && !row.contains("├─"),
                "{hidden}: {row}"
            );
            assert_eq!(
                app.state.handoff_parent("visible-child"),
                Some("hidden-parent")
            );
        }
    }

    #[test]
    fn list_handoff_connectors_respect_category_and_band_boundaries() {
        let mut app = list_fixture(&["group-parent", "group-child"]);
        link_list_child(&mut app, "group-child", "group-parent");
        let parent_category = app
            .state
            .create_category("parent-topic", Utc::now())
            .unwrap();
        let child_category = app
            .state
            .create_category("child-topic", Utc::now())
            .unwrap();
        app.state.assign_category("group-parent", &parent_category);
        app.state.assign_category("group-child", &child_category);
        app.rebuild_visible();
        let buffer = list_frame(&mut app, 12);
        let row = list_title_row(&buffer, "group-child");
        // Thread grouping deliberately uses the root's topic, even when a lane
        // has a different stored category. Both rows share this visible header.
        assert!(row.contains("└─ group-child"), "{row}");
        assert!(list_title_row(&buffer, "group-parent").contains("[1 lanes]"));

        // A viewport can contain adjacent source rows separated by a group header
        // or the recent/older divider; neither is a tree edge.
        app.visible = vec![
            Row::Session(0),
            Row::Header(Some(child_category.clone())),
            Row::Session(1),
        ];
        app.recent_count = 3;
        let row = list_title_row(&list_frame(&mut app, 12), "group-child");
        assert!(!row.contains("└─") && !row.contains("├─"), "{row}");
        app.state.clear_category("group-parent");
        app.state.clear_category("group-child");
        app.visible = vec![Row::Session(0), Row::Session(1)];
        app.recent_count = 1;
        let row = list_title_row(&list_frame(&mut app, 12), "group-child");
        assert!(!row.contains("└─") && !row.contains("├─"), "{row}");
    }

    #[test]
    fn list_handoff_viewport_and_collapsed_groups_do_not_leave_dangling_branches() {
        let mut app = list_fixture(&["screen-parent", "screen-first", "screen-last"]);
        link_list_child(&mut app, "screen-first", "screen-parent");
        link_list_child(&mut app, "screen-last", "screen-parent");
        app.all_sessions[1].last_active = Some(Utc::now());
        app.all_sessions[2].last_active = None;
        app.rebuild_visible();
        app.selected = app.row_of_session("screen-last").unwrap();
        let buffer = list_frame(&mut app, 4);
        assert!(!frame_text(&buffer).contains("screen-parent"));
        for title in ["screen-first", "screen-last"] {
            let row = list_title_row(&buffer, title);
            assert!(!row.contains("└─") && !row.contains("├─"), "{row}");
        }
        app.selected = app.row_of_session("screen-parent").unwrap();
        let buffer = list_frame(&mut app, 5);
        assert!(list_title_row(&buffer, "screen-first").contains("└─ screen-first"));
        assert!(!frame_text(&buffer).contains("screen-last"));

        let category = app
            .state
            .create_category("folded-topic", Utc::now())
            .unwrap();
        app.state.assign_category("screen-parent", &category);
        app.state.set_collapsed(&category, true);
        app.rebuild_visible();
        let text = frame_text(&list_frame(&mut app, 8));
        assert!(text.contains("folded-topic"));
        assert!(!text.contains("screen-first") && !text.contains("screen-last"));
        for row in text.lines().filter(|row| row.starts_with('│')) {
            assert!(!row.contains("└─") && !row.contains("├─"), "{row}");
        }
    }

    #[test]
    fn list_handoff_off_page_source_collisions_never_claim_a_unique_edge() {
        for archived in [false, true] {
            for duplicate in [0, 1] {
                let mut app = list_fixture(&["family-parent", "family-child"]);
                link_list_child(&mut app, "family-child", "family-parent");
                for source in &mut app.all_sessions {
                    source.archived = archived;
                }
                app.show_archived = archived;
                let originals = app.all_sessions.clone();
                let mut collision = originals[duplicate].clone();
                collision.file = PathBuf::from("/other-account/hidden-source.jsonl");
                collision.archived = true;
                let mut catalog = originals.clone();
                catalog.push(collision);
                app.archive_catalog = std::sync::Arc::new(catalog);
                app.rebuild_visible();
                let buffer = list_frame(&mut app, 10);
                let child = list_title_row(&buffer, "family-child");
                assert!(
                    !child.contains("└─") && !child.contains("├─"),
                    "off-page duplicate {duplicate}, archive view {archived}: {child}"
                );
                assert!(!list_title_row(&buffer, "family-parent").contains("lanes"));

                // A page and its catalog contain the same source, not two
                // identities. A replaced census must restore that real edge.
                app.archive_catalog = std::sync::Arc::new(originals);
                app.rebuild_visible();
                let buffer = list_frame(&mut app, 10);
                assert!(list_title_row(&buffer, "family-child").contains("└─ family-child"));
            }
        }
    }

    #[test]
    fn native_session_headers_and_help_do_not_offer_jev_controls() {
        let mut app = startup_app();

        app.focus = Focus::Terminal;
        for agent in [Agent::Codex, Agent::Claude, Agent::Kiro, Agent::Cursor] {
            let id = app
                .all_sessions
                .iter()
                .find(|session| session.agent == agent)
                .unwrap()
                .id
                .clone();
            app.panes = vec![id];
            app.focused = 0;
            let text = frame_text(&startup_frame(&mut app, 140, 60));
            assert!(!text.contains("Jev") && !text.contains("F6"), "{text}");
            app.help_visible = true;
            let help = frame_text(&startup_frame(&mut app, 140, 60));
            assert!(!help.contains("F6") && !help.contains("Jev"), "{help}");
            app.help_visible = false;
        }
    }

    fn pane_account_app() -> App {
        let mut app = startup_app();

        app.focus = Focus::Terminal;
        app.all_sessions = ["account-left", "account-right"]
            .into_iter()
            .map(|id| {
                let mut session = session(Agent::Codex, 0);
                session.id = id.into();
                session.title = "a long session title that must leave room for the account".into();
                session.file = PathBuf::new();
                session
            })
            .collect();
        app.panes = app
            .all_sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        app.active = app.panes.first().cloned();
        app
    }

    fn pane_account_borders(app: &mut App, width: u16, zoomed: bool) -> (String, String) {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_pane(
                    frame,
                    app,
                    "account-left",
                    0,
                    2,
                    area,
                    true,
                    zoomed,
                    Some(("a-long-category-name", CATEGORY)),
                );
            })
            .unwrap();
        assert_eq!(app.pane_sizes["account-left"], (6, width - 2));
        (
            rendered_row(terminal.backend().buffer(), 0),
            rendered_row(terminal.backend().buffer(), 7),
        )
    }

    #[test]
    fn a_single_claude_account_is_visible_in_list_and_owned_panes() {
        use mindplayer_core::accounts::Account;
        let mut app = pane_account_app();
        let mut account = Account::inherited(Agent::Claude);
        account.name = "claude-work".into();
        app.accounts = vec![account.clone()];
        for session in &mut app.all_sessions {
            session.agent = Agent::Claude;
            app.pane_accounts
                .insert(session.id.clone(), (Agent::Claude, account.name.clone()));
        }
        app.rebuild_visible();
        let text = frame_text(&list_frame(&mut app, 12));
        assert_eq!(text.matches(&account.name).count(), 2, "{text}");
        for zoomed in [false, true] {
            let (_, border) = pane_account_borders(&mut app, 80, zoomed);
            assert!(border.contains(&account.name), "{border}");
            assert!(!border.contains("codex"), "{border}");
        }
    }

    #[test]
    fn pane_account_labels_follow_each_pane_in_split_and_zoom() {
        let mut app = pane_account_app();
        for (id, label) in [("account-left", "cedar"), ("account-right", "cobalt")] {
            app.pane_accounts
                .insert(id.into(), (Agent::Codex, label.into()));
        }
        for width in [80, 140] {
            app.zoomed = false;
            let buffer = startup_frame(&mut app, width, 24);
            for (id, expected, other, count) in [
                ("account-left", "cedar", "cobalt", "1/2"),
                ("account-right", "cobalt", "cedar", "2/2"),
            ] {
                let (x, y, rows, cols) = app.pane_bounds[id];
                let header = (x..x + cols)
                    .map(|column| buffer[(column, y - 1)].symbol())
                    .collect::<String>();
                let account = (x..x + cols)
                    .map(|column| buffer[(column, y + rows)].symbol())
                    .collect::<String>();
                assert!(
                    account.contains(&format!("codex · {expected}")),
                    "{account}"
                );
                assert!(!account.contains(other), "{account}");
                assert!(header.contains(count), "{header}");
            }
            app.zoomed = true;
            app.focused = 1;
            let text = frame_text(&startup_frame(&mut app, width, 24));
            assert!(text.contains("codex · cobalt"), "{text}");
            assert!(!text.contains("codex · cedar"), "{text}");
            assert!(text.contains("2/2"), "{text}");
            assert!(text.contains("ZOOMED"), "{text}");
            assert!(text.contains("1 hidden"), "{text}");
            app.focused = 0;
        }
    }

    #[test]
    fn pane_account_labels_use_exact_transcript_ownership_without_default_guessing() {
        use mindplayer_core::accounts::Account;
        let mut app = pane_account_app();
        let mut inherited = Account::inherited(Agent::Codex);
        inherited.name = "cedar-default".into();
        let outer = Account::isolated(Path::new("/fixture"), Agent::Codex, "cobalt").unwrap();
        let mut nested = outer.clone();
        nested.name = "nested-owner".into();
        nested.slot = mindplayer_core::accounts::Slot::Isolated {
            path: outer.session_root(Path::new("/fixture")).join("nested"),
        };
        app.accounts = vec![inherited, nested.clone(), outer.clone()];
        app.all_sessions[0].file = nested
            .session_root(Path::new("/fixture"))
            .join("rollout.jsonl");
        let (_, title) = pane_account_borders(&mut app, 100, false);
        assert!(title.contains("codex · nested-owner"), "{title}");
        assert!(!title.contains("cedar-default"), "{title}");
        app.pane_accounts
            .insert("account-left".into(), (Agent::Codex, "exact-pane".into()));
        let (_, title) = pane_account_borders(&mut app, 100, true);
        assert!(title.contains("codex · exact-pane"), "{title}");
        assert!(!title.contains("nested-owner"), "{title}");
        app.pane_accounts.clear();
        for file in [
            PathBuf::new(),
            PathBuf::from("relative/rollout.jsonl"),
            PathBuf::from("/unowned/rollout.jsonl"),
            outer
                .session_root(Path::new("/fixture"))
                .join("../other/rollout.jsonl"),
        ] {
            app.all_sessions[0].file = file;
            let (_, title) = pane_account_borders(&mut app, 100, false);
            assert!(title.contains("codex · 계정 미확인"), "{title}");
            assert!(!title.contains("cedar-default"), "{title}");
        }
        app.pane_accounts.insert(
            "account-left".into(),
            (Agent::Claude, "claude-owner".into()),
        );
        let (_, title) = pane_account_borders(&mut app, 100, false);
        assert!(!title.contains("codex ·"), "{title}");
    }

    #[test]
    fn pane_account_labels_reject_ambiguous_transcript_roots() {
        let mut app = pane_account_app();
        let account = mindplayer_core::accounts::Account::isolated(
            Path::new("/fixture"),
            Agent::Codex,
            "cedar",
        )
        .unwrap();
        let mut same_root = account.clone();
        same_root.name = "cobalt".into();
        app.all_sessions[0].file = account
            .session_root(Path::new("/fixture"))
            .join("rollout.jsonl");
        app.accounts = vec![account, same_root];
        let (_, label) = pane_account_borders(&mut app, 100, false);
        assert!(label.contains("codex · 계정 미확인"), "{label}");
        assert!(
            !label.contains("cedar") && !label.contains("cobalt"),
            "{label}"
        );
        app.pane_accounts
            .insert("account-left".into(), (Agent::Codex, "cedar".into()));
        let (_, label) = pane_account_borders(&mut app, 100, true);
        assert!(label.contains("codex · cedar"), "{label}");
    }

    #[test]
    fn pane_account_labels_truncate_whole_graphemes_and_strip_terminal_controls() {
        for (label, width, expected) in [
            ("任意名", 3, "任…"),
            ("e\u{301}🧑‍💻tail", 2, "e\u{301}…"),
            ("🧑‍💻x", 2, "…"),
            ("a\n\r\t\u{1b}\u{202e}b", 20, "ab"),
            ("\n\u{202e}", 20, "계정 미확인"),
            ("long-label", 1, "…"),
            ("long-label", 0, ""),
        ] {
            let actual = truncate_pane_account_label(label, width);
            assert_eq!(actual, expected, "label={label:?} width={width}");
            assert!(Line::from(actual.as_str()).width() <= width);
        }
    }

    #[test]
    fn pane_account_long_unicode_labels_leave_status_and_counts_visible() {
        let mut app = pane_account_app();
        app.ended.insert("account-left".into());
        app.pane_accounts.insert(
            "account-left".into(),
            (
                Agent::Codex,
                "라벨\n\t\r\u{1b}\u{202e}界e\u{301} 🧑‍💻 arbitrary label".repeat(8),
            ),
        );
        for width in [36, 48, 80] {
            for zoomed in [false, true] {
                let (title, account) = pane_account_borders(&mut app, width, zoomed);
                assert!(account.contains("codex · 라벨"), "{account}");
                assert!(account.contains('…'), "{account}");
                assert!(title.contains('○'), "{title}");
                assert!(title.contains("1/2"), "{title}");
                assert!(title.contains("(ended)"), "{title}");
                assert_eq!(title.contains("ZOOMED"), zoomed, "{title}");
                assert!(!account.chars().any(char::is_control), "{account:?}");
                assert!(!account.contains('\u{202e}'), "{account:?}");
                assert!(Line::from(title.as_str()).width() <= usize::from(width));
                assert!(Line::from(account.as_str()).width() <= usize::from(width));
            }
        }
    }

    #[test]
    fn saved_provider_identity_is_a_binding_not_a_claim_of_live_authentication() {
        for provider in [Agent::Codex, Agent::Claude] {
            let mut app = startup_app();

            let mut account = mindplayer_core::accounts::Account::inherited(provider);
            account.set_identity_fingerprint(Some("saved-identity".into()));
            app.accounts = vec![account];
            app.open_accounts();
            let text = frame_text(&startup_frame(&mut app, 170, 35));
            assert!(text.contains("identity bound"), "{text}");
            assert!(!text.contains("identity verified"), "{text}");
        }
    }

    /// The footer's one Vercel row with its column padding collapsed to single spaces.
    fn vercel_line(frame: &str) -> String {
        let rows: Vec<_> = frame
            .lines()
            .filter(|line| line.trim_start().starts_with(super::VERCEL_HEAD))
            .collect();
        assert_eq!(rows.len(), 1, "{frame}");
        rows[0].split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn vercel_test_budget(
        used: Option<f64>,
        limit: f64,
    ) -> mindplayer_core::vercel_usage::VercelBudget {
        mindplayer_core::vercel_usage::VercelBudget {
            used,
            limit,
            source: mindplayer_core::vercel_usage::BudgetSource::Default,
            refresh_period: "weekly".into(),
        }
    }

    fn vercel_text(app: &App, width: u16) -> String {
        let lines = vercel_footer_lines(app, width);
        assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn vercel_user_block_is_independent_of_key_spend() {
        use mindplayer_core::vercel_usage::{VercelUsage, VercelUsageError};
        let mut app = startup_app();

        app.vercel_cwd = Some(app.cwd.clone());
        for key in [
            Ok(vercel_test_budget(Some(12.25), 1000.0)),
            Ok(vercel_test_budget(None, 1000.0)),
            Err(VercelUsageError::Unavailable),
        ] {
            let key_meter_known = key.as_ref().is_ok_and(|budget| budget.used.is_some());
            app.vercel_budget = Some(Ok(VercelUsage {
                key,
                user: Ok(Some(vercel_test_budget(Some(51.007682468), 50.0))),
            }));
            for width in [20, 40, 80, 144] {
                let text = vercel_text(&app, width);
                assert!(text.contains("API key · blocked"), "{text}");
                assert!(text.contains("user weekly $51.01/$50 exceeded"), "{text}");
                assert!(!text.contains("$1000"), "{text}");
                assert!(
                    !text.contains("$12.25"),
                    "key meter is not user spend: {text}"
                );
            }
            if key_meter_known {
                assert_eq!(vercel_footer_lines(&app, 80).len(), 1);
                let frame = frame_text(&startup_frame(&mut app, 80, 29));
                assert!(
                    vercel_line(&frame).contains("user weekly $51.01/$50 exceeded"),
                    "{frame}"
                );
            }
        }
    }

    #[test]
    fn vercel_key_block_remains_visible_independently_of_user_budget() {
        use mindplayer_core::vercel_usage::VercelUsage;
        let mut app = startup_app();
        app.vercel_cwd = Some(app.cwd.clone());
        app.vercel_budget = Some(Ok(VercelUsage {
            key: Ok(vercel_test_budget(Some(123.75), 100.0)),
            user: Ok(Some(vercel_test_budget(Some(2.0), 50.0))),
        }));
        for width in [20, 40, 80, 144] {
            let text = vercel_text(&app, width);
            assert!(text.contains("API key · blocked"), "{text}");
            assert!(text.contains("key weekly $123.75/$100 exceeded"), "{text}");
            assert!(text.contains("user weekly $2.00/$50"), "{text}");
            assert!(!text.contains("$1000"), "{text}");
        }
    }

    #[test]
    fn vercel_unknown_key_never_promotes_known_user_to_unblocked() {
        use mindplayer_core::vercel_usage::{VercelUsage, VercelUsageError};
        let mut app = startup_app();
        app.vercel_cwd = Some(app.cwd.clone());
        for user in [
            Ok(Some(vercel_test_budget(Some(0.0), 50.0))),
            Ok(Some(vercel_test_budget(Some(25.0), 50.0))),
            Err(VercelUsageError::NoBudget),
        ] {
            app.vercel_budget = Some(Ok(VercelUsage {
                key: Err(VercelUsageError::Unavailable),
                user,
            }));
            let text = vercel_text(&app, 80);
            assert!(text.contains("blocked state unknown"), "{text}");
            assert!(!text.contains("not blocked"), "{text}");
            assert!(text.contains("key lookup unavailable"), "{text}");
        }
    }

    #[test]
    fn vercel_unknown_user_preserves_valid_key_meter_without_inventing_user_spend() {
        use mindplayer_core::vercel_usage::{VercelUsage, VercelUsageError};
        let mut app = startup_app();
        app.vercel_cwd = Some(app.cwd.clone());
        for (user, expected) in [
            (
                Err(VercelUsageError::UserNotIdentified),
                "user attribution unknown",
            ),
            (
                Err(VercelUsageError::Unavailable),
                "user lookup unavailable",
            ),
            (
                Ok(Some(vercel_test_budget(None, 50.0))),
                "spend unknown / $50 limit (no meter)",
            ),
        ] {
            app.vercel_budget = Some(Ok(VercelUsage {
                key: Ok(vercel_test_budget(Some(12.25), 1000.0)),
                user,
            }));
            let text = vercel_text(&app, 80);
            assert!(text.contains(expected), "{text}");
            assert!(text.contains("blocked state unknown"), "{text}");
            assert!(!text.contains("$0.00"), "{text}");
            if !expected.contains("no meter") {
                assert!(text.contains("key spend $12.25"), "{text}");
            }
        }
    }

    #[test]
    fn vercel_team_meter_zero_absence_and_lookup_states_are_distinct() {
        use mindplayer_core::vercel_usage::{VercelUsage, VercelUsageError};
        let mut app = startup_app();
        app.vercel_cwd = Some(app.cwd.clone());
        for (key, expected, known) in [
            (
                Ok(vercel_test_budget(None, 1000.0)),
                "key spend unknown",
                false,
            ),
            (
                Ok(vercel_test_budget(Some(0.0), 1000.0)),
                "key spend $0.00",
                true,
            ),
            (
                Ok(vercel_test_budget(Some(33.0), 1000.0)),
                "key spend $33.00",
                true,
            ),
            (
                Err(VercelUsageError::NoBudget),
                "key no active budget",
                true,
            ),
            (
                Err(VercelUsageError::Unavailable),
                "key lookup unavailable",
                false,
            ),
        ] {
            app.vercel_budget = Some(Ok(VercelUsage {
                key,
                user: Ok(None),
            }));
            let text = vercel_text(&app, 80);
            assert!(
                text.contains("team attributed") && text.contains(expected),
                "{text}"
            );
            assert_eq!(text.contains("not blocked by budgets"), known, "{text}");
            assert!(!text.contains("$1000"), "{text}");
        }
        for (reading, expected) in [
            (None, "loading"),
            (Some(Err(VercelUsageError::NotConfigured)), "not configured"),
            (
                Some(Err(VercelUsageError::Unavailable)),
                "lookup unavailable",
            ),
            (
                Some(Err(VercelUsageError::InvalidResponse)),
                "lookup unavailable",
            ),
        ] {
            app.vercel_budget = reading;
            let text = vercel_text(&app, 80);
            assert!(
                text.contains(expected) && text.contains("blocked state unknown"),
                "{text}"
            );
            assert!(!text.contains("API key") && !text.contains('$'), "{text}");
        }
        app.cwd.push("different-workspace");
        assert!(vercel_text(&app, 80).contains("loading"));
    }

    #[test]
    fn vercel_wraps_long_amounts_without_discarding_digits_or_blocked_state() {
        use mindplayer_core::vercel_usage::VercelUsage;
        let mut app = startup_app();
        app.vercel_cwd = Some(app.cwd.clone());
        app.vercel_budget = Some(Ok(VercelUsage {
            key: Ok(vercel_test_budget(Some(1234567890123456.0), 0.0)),
            user: Ok(None),
        }));
        for width in [8, 12, 20, 40, 80] {
            let text = vercel_text(&app, width);
            let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
            assert!(compact.contains("$1234567890123456.00/$0"), "{text}");
            assert!(
                compact.contains("blocked") && compact.contains("exceeded"),
                "{text}"
            );
        }
    }

    #[test]
    fn provider_windows_share_wide_rows_without_losing_reset_information() {
        use mindplayer_core::limits::QuotaRow;
        let mut app = startup_app();

        app.limits = Some(
            [
                ("claude 5h", 92.0, "01:39"),
                ("claude weekly", 16.0, "11-17"),
            ]
            .into_iter()
            .map(|(label, used, reset)| QuotaRow {
                label: label.into(),
                agent: Agent::Claude,
                used_percent: Some(used),
                resets: Some(reset.into()),
                ..Default::default()
            })
            .collect(),
        );
        for width in [80, 144] {
            let text = frame_text(&startup_frame(&mut app, width, 29));
            for value in ["92.0%", "16.0%", "resets 01:39", "resets 11-17"] {
                assert!(text.contains(value), "{text}");
            }
            let windows: Vec<_> = text
                .lines()
                .filter(|line| line.contains("resets"))
                .collect();
            if width == 144 {
                assert_eq!(windows.len(), 1, "{text}");
            }
        }
    }

    #[test]
    fn shared_codex_login_renders_both_measured_gauges_resets_and_identity_warning() {
        use mindplayer_core::limits::{mark_shared_logins, QuotaRow};
        let mut app = startup_app();
        app.accounts = ["work", "personal"]
            .map(|name| {
                let mut account = mindplayer_core::accounts::Account::inherited(Agent::Codex);
                account.name = name.into();
                account
            })
            .to_vec();
        let mut rows = ["work", "personal"]
            .into_iter()
            .flat_map(|account| {
                ["codex weekly", "codex credits"].map(|label| QuotaRow {
                    label: label.into(),
                    agent: Agent::Codex,
                    account: account.into(),
                    used_percent: Some(32.0),
                    resets: Some("10-22 15:14".into()),
                    identity: Some("same-account-fixture".into()),
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();
        mark_shared_logins(&mut rows);
        app.limits = Some(rows);
        for (width, height) in [(88, 29), (140, 40)] {
            let text = frame_text(&startup_frame(&mut app, width, height));
            let quotas: Vec<_> = text
                .lines()
                .filter(|line| line.contains("weekly") || line.contains("credits"))
                .collect();
            assert_eq!(text.matches("32.0%").count(), 4, "{text}");
            assert_eq!(text.matches("resets 10-22 15:14").count(), 4, "{text}");
            for account in ["work", "personal"] {
                for window in ["weekly", "credits"] {
                    let row = quotas
                        .iter()
                        .find(|line| line.contains(&format!("{account} {window}")))
                        .unwrap();
                    assert!(row.contains("▰▰▱▱▱▱"), "{text}");
                    assert!(row.contains("32.0%"), "{text}");
                    assert!(row.contains("resets 10-22 15:14"), "{text}");
                    if account == "personal" {
                        assert!(row.contains("same login as work"), "{text}");
                    }
                }
            }
        }
    }

    #[test]
    fn native_panes_preserve_four_plus_three_and_full_width_quotas() {
        let mut app = startup_app();
        app.focus = Focus::Terminal;
        app.panes = (0..7).map(|index| format!("pane-{index}")).collect();
        app.layout = PaneLayout::Horizontal;
        app.limits = Some(vec![mindplayer_core::limits::QuotaRow {
            label: "codex weekly".into(),
            agent: Agent::Codex,
            used_percent: Some(32.0),
            resets: Some("10-22 15:14".into()),
            ..Default::default()
        }]);
        for (width, height) in [(88, 29), (140, 40), (196, 55)] {
            let frame = startup_frame(&mut app, width, height);
            let bounds = app.pane_bounds.clone();
            assert_eq!(bounds.len(), 7);
            let first_y = bounds.values().map(|bounds| bounds.1).min().unwrap();
            assert_eq!(
                bounds.values().filter(|bounds| bounds.1 == first_y).count(),
                4
            );
            assert_eq!(
                bounds.values().filter(|bounds| bounds.1 != first_y).count(),
                3
            );
            let rectangles: Vec<_> = bounds
                .values()
                .map(|&(x, y, rows, cols)| Rect::new(x - 1, y - 1, cols + 2, rows + 2))
                .collect();
            for (index, rectangle) in rectangles.iter().enumerate() {
                for other in &rectangles[index + 1..] {
                    assert!(rectangle.intersection(*other).is_empty());
                }
            }
            let text = frame_text(&frame);
            let quota_y = text.lines().position(|row| row.contains("32.0%")).unwrap();
            assert!(rectangles
                .iter()
                .all(|rectangle| rectangle.bottom() <= quota_y as u16));
            assert!(
                text.lines().nth(quota_y).unwrap().contains("10-22 15:14"),
                "{text}"
            );
            assert!(text.contains("ctrl-g trace · ctrl-x list"), "{text}");
        }
    }

    fn paint_native_footer(parser: &mut vt100::Parser, footer: &str) {
        let (rows, _) = parser.screen().size();
        parser.process(
            format!(
            "\x1b[2J\x1b[Hnative transcript\x1b[{};1H› native draft\x1b[{};1H  {footer}\x1b[{};4H",
            rows - 2, rows, rows - 2
        )
            .as_bytes(),
        );
    }

    #[test]
    fn prompt_text_estimate_is_local_and_utf8_aware() {
        assert_eq!(estimated_prompt_tokens(""), 0);
        assert_eq!(estimated_prompt_tokens("trace this command"), 5);
        assert_eq!(estimated_prompt_tokens("잘된건가?"), 4);
    }

    #[test]
    fn turn_token_count_has_a_five_row_visual_treatment() {
        let lines = big_token_lines(42);
        assert_eq!(lines.len(), 5);
        assert!(!lines[2].to_string().contains("~"));
        assert!(lines[2].to_string().contains("TOKENS"));
        assert!(lines.iter().any(|line| line.to_string().contains("███")));
    }

    #[test]
    fn both_codex_accounts_render_weekly_and_credits_in_the_footer() {
        use mindplayer_core::accounts::Account;
        use mindplayer_core::limits::QuotaRow;
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = App::new();

        app.screen = Screen::Main;
        app.accounts = ["work", "personal"]
            .into_iter()
            .map(|name| {
                let mut account = Account::inherited(Agent::Codex);
                account.name = name.into();
                account
            })
            .collect();
        app.limits = Some(
            ["work", "personal"]
                .into_iter()
                .flat_map(|account| {
                    let exhausted = account == "personal";
                    [
                        QuotaRow {
                            label: "codex weekly".into(),
                            agent: Agent::Codex,
                            account: account.into(),
                            used_percent: (!exhausted).then_some(0.0),
                            ..Default::default()
                        },
                        QuotaRow {
                            label: "codex credits".into(),
                            agent: Agent::Codex,
                            account: account.into(),
                            used_percent: Some(if exhausted { 100.0 } else { 10.0 }),
                            detail: "reported usage/limit".into(),
                            resets: Some("10-01 09:00".into()),
                            identity: None,
                            failure: None,
                            spend_cap: true,
                            blocked: false,
                            plan: None,
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
            for account in ["work", "personal"] {
                for period in ["weekly", "credits"] {
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
                .find(|line| line.contains("personal credits"))
                .unwrap();
            assert!(
                exhausted.contains("100.0%")
                    && exhausted.contains("credit cap reached")
                    && exhausted.contains("10-01 09:00"),
                "{exhausted}"
            );
            if width == 100 {
                for line in lines
                    .iter()
                    .filter(|line| line.contains("work") || line.contains("personal"))
                {
                    println!("{}", line.trim_end());
                }
            }
        }
    }

    #[test]
    fn a_soft_credit_cap_is_amber_but_backend_refusal_is_red() {
        let row = mindplayer_core::limits::QuotaRow {
            label: "codex credits".into(),
            used_percent: Some(100.0),
            spend_cap: true,
            ..Default::default()
        };
        let spans = quota_row_spans(&row, 20, None);
        assert_eq!(spans[1].style.fg, Some(ZOOM));
        assert_eq!(spans[2].style.fg, Some(ZOOM));
        let blocked = mindplayer_core::limits::QuotaRow {
            blocked: true,
            ..row
        };
        let spans = quota_row_spans(&blocked, 20, None);
        assert_eq!(spans[1].style.fg, Some(ERROR));
    }

    #[test]
    fn codex_monthly_limit_and_unknown_weekly_are_distinct_for_both_accounts() {
        use mindplayer_core::limits::QuotaRow;
        for account in ["work", "personal"] {
            let weekly = QuotaRow {
                label: format!("{account} weekly"),
                agent: Agent::Codex,
                account: account.into(),
                detail: "not reported".into(),
                ..Default::default()
            };
            let monthly = QuotaRow {
                label: format!("{account} credits"),
                agent: Agent::Codex,
                account: account.into(),
                used_percent: Some(100.0),
                detail: "limit reached".into(),
                resets: Some("10-01 09:00".into()),
                identity: None,
                failure: None,
                spend_cap: false,
                blocked: false,
                plan: None,
            };
            let weekly_line = Line::from(quota_row_spans(&weekly, 20, None));
            let monthly_spans = quota_row_spans(&monthly, 20, None);
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
        let measured = mindplayer_core::catalog::UsageSnapshot {
            tokens: session(Agent::Claude, 6_200_000).tokens,
            context_pct: None,
            partial: false,
        };
        assert_eq!(
            usage_cell(&session(Agent::Cursor, 6_200_000), Some(&measured)),
            "~6.2M tok"
        );
        assert_eq!(
            usage_cell(&session(Agent::Claude, 6_200_000), Some(&measured)),
            "6.2M tok"
        );
    }

    /// Unmeasured is distinct from a successful measurement of zero.
    #[test]
    fn a_cursor_session_with_nothing_to_estimate_shows_no_number() {
        let source = session(Agent::Cursor, 0);
        assert_eq!(usage_cell(&source, None), "—");
        let mut measured = mindplayer_core::catalog::UsageSnapshot {
            tokens: source.tokens,
            context_pct: None,
            partial: false,
        };
        assert_eq!(usage_cell(&source, Some(&measured)), "~0 tok");
        measured.partial = true;
        assert_eq!(usage_cell(&source, Some(&measured)), "~0 tok partial");
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

    /// The EXECUTION block spends a row on its title, so a scroll range worked out from the
    /// whole area stopped one row short and the last line could never be scrolled into view.
    #[test]
    fn the_last_execution_line_can_be_scrolled_into_view() {
        use ratatui::{backend::TestBackend, Terminal};
        let (width, height) = (30u16, 6u16);
        let mut detail: Vec<Line> = (0..12).map(|i| Line::from(format!("line {i}"))).collect();
        detail.push(Line::from("the very last line"));
        let max_scroll = trace_detail_max_scroll(&detail, width, height);

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                f.render_widget(
                    Paragraph::new(detail.clone())
                        .block(trace_detail_block())
                        .wrap(Wrap { trim: false })
                        .scroll((max_scroll as u16, 0)),
                    f.area(),
                );
            })
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            screen.contains("the very last line"),
            "scrolled to the end, the last line is still off screen: {screen:?}"
        );
    }

    #[test]
    fn trace_scroll_range_counts_wrapped_terminal_rows() {
        let detail = vec![Line::from("word ".repeat(40))];
        assert!(
            trace_detail_max_scroll(&detail, 20, 5) > 0,
            "a long logical line occupies multiple rendered rows and must scroll"
        );
        assert_eq!(
            trace_detail_max_scroll(&[Line::from("short")], 20, 5),
            0,
            "content that fits must not invent scrollback"
        );
    }

    struct QuietPtyFixture {
        app: App,
        state_dir: PathBuf,
    }

    fn isolated_app() -> App {
        let mut app = App::new();

        let state_dir =
            std::env::temp_dir().join(format!("mindplayer-ui-geometry-{}", std::process::id()));
        std::fs::create_dir_all(&state_dir).unwrap();
        app.state_path = state_dir.join("state.json");
        app.audit_path = state_dir.join("audit.jsonl");
        app.state = mindplayer_core::State::default();
        app.walker_choice = crate::walker::index_of(crate::walker::DEFAULT_ID);
        app
    }

    impl QuietPtyFixture {
        fn new(rows: u16, cols: u16) -> Self {
            let mut app = isolated_app();
            let state_dir = app
                .state_path
                .parent()
                .expect("isolated state has a parent")
                .to_path_buf();
            app.screen = Screen::Main;
            app.focus = Focus::Terminal;
            app.panes = vec!["ui-geometry-left".into(), "ui-geometry-right".into()];
            app.focused = 0;
            app.active = app.panes.first().cloned();

            for id in &app.panes {
                let command = mindplayer_core::Command {
                    program: "cat".into(),
                    args: Vec::new(),
                    cwd: PathBuf::new(),
                    env: Default::default(),
                };
                let pty = PtySession::spawn(&command, id, rows, cols)
                    .unwrap_or_else(|error| panic!("spawn quiet PTY {id}: {error}"));
                app.ptys.insert(id.clone(), pty);
            }

            Self { app, state_dir }
        }

        fn assert_visible_dimensions(&self) {
            assert_eq!(
                self.app.pane_sizes.len(),
                if self.app.zoomed { 1 } else { 2 },
                "pane size cache must contain exactly the visible panes"
            );
            for (id, &(rows, cols)) in &self.app.pane_sizes {
                let pty = self
                    .app
                    .ptys
                    .get(id)
                    .unwrap_or_else(|| panic!("visible pane {id} has no PTY"));
                assert_eq!(
                    (pty.rows, pty.cols),
                    (rows, cols),
                    "public PTY dimensions drifted from pane geometry for {id}"
                );
                let parser = pty.parser().lock().unwrap();
                assert_eq!(
                    parser.screen().size(),
                    (rows, cols),
                    "parser dimensions drifted from pane geometry for {id}"
                );
            }
        }
    }

    impl Drop for QuietPtyFixture {
        fn drop(&mut self) {
            self.app.ptys.clear();
            let _ = std::fs::remove_dir_all(&self.state_dir);
        }
    }

    fn rendered_row(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
        let mut text = String::new();
        let mut column = buffer.area.x;
        while column < buffer.area.right() {
            let symbol = buffer[(column, row)].symbol();
            text.push_str(symbol);
            column += Span::raw(symbol).width().max(1) as u16;
        }
        text
    }

    #[test]
    fn native_ptys_keep_four_plus_three_geometry_and_full_width_footer() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut fixture = QuietPtyFixture::new(8, 20);
        for index in 2..7 {
            let id = format!("ui-native-grid-{index}");
            let pty = PtySession::spawn(
                &mindplayer_core::Command {
                    program: "cat".into(),
                    args: vec![],
                    cwd: PathBuf::new(),
                    env: Default::default(),
                },
                &id,
                8,
                20,
            )
            .unwrap();
            fixture.app.panes.push(id.clone());
            fixture.app.ptys.insert(id, pty);
        }
        fixture.app.layout = PaneLayout::Horizontal;
        let mut terminal = Terminal::new(TestBackend::new(196, 55)).unwrap();
        terminal
            .draw(|frame| render(frame, &mut fixture.app))
            .unwrap();
        let bounds = fixture.app.pane_bounds.clone();
        let first_y = bounds.values().map(|bounds| bounds.1).min().unwrap();
        assert_eq!(
            bounds.values().filter(|bounds| bounds.1 == first_y).count(),
            4
        );
        assert_eq!(
            bounds.values().filter(|bounds| bounds.1 != first_y).count(),
            3
        );
        let full_width_footer = rendered_row(terminal.backend().buffer(), 54);
        for (id, &(x, _, rows, cols)) in &bounds {
            let pty = fixture.app.ptys.get_mut(id).unwrap();
            assert_eq!((pty.rows, pty.cols), (rows, cols));
            assert!(x + cols < 196);
            paint_native_footer(
                &mut pty.parser().lock().unwrap(),
                "gpt-5.1 medium · 98% left",
            );
        }
        terminal
            .draw(|frame| render(frame, &mut fixture.app))
            .unwrap();
        assert_eq!(fixture.app.pane_bounds, bounds);
        assert_eq!(
            rendered_row(terminal.backend().buffer(), 54),
            full_width_footer
        );
        for (id, &(_, _, rows, cols)) in &bounds {
            let pty = &fixture.app.ptys[id];
            assert_eq!((pty.rows, pty.cols), (rows, cols));
            assert_eq!(pty.parser().lock().unwrap().screen().size(), (rows, cols));
        }
    }

    #[test]
    fn first_draw_keeps_visible_pty_geometry_in_sync_across_resize_and_zoom() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut fixture = QuietPtyFixture::new(8, 20);
        let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut fixture.app))
            .unwrap();
        fixture.assert_visible_dimensions();

        terminal.backend_mut().resize(84, 26);
        terminal
            .draw(|frame| render(frame, &mut fixture.app))
            .unwrap();
        fixture.assert_visible_dimensions();

        fixture.app.toggle_zoom();
        terminal.backend_mut().resize(108, 30);
        terminal
            .draw(|frame| render(frame, &mut fixture.app))
            .unwrap();
        fixture.assert_visible_dimensions();

        fixture.app.toggle_zoom();
        terminal.backend_mut().resize(72, 24);
        terminal
            .draw(|frame| render(frame, &mut fixture.app))
            .unwrap();
        fixture.assert_visible_dimensions();
    }

    #[test]
    fn pane_presents_completed_synchronized_frames_not_partial_repaints() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut fixture = QuietPtyFixture::new(8, 20);
        let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
        terminal
            .draw(|frame| render(frame, &mut fixture.app))
            .unwrap();
        let id = fixture.app.panes[0].clone();
        let parser = fixture.app.ptys[&id].parser().clone();
        parser
            .lock()
            .unwrap()
            .process(b"\x1b[HOLD-FRAME\x1b[?2026h\x1b[HPARTIAL-NEW");
        terminal
            .draw(|frame| render(frame, &mut fixture.app))
            .unwrap();
        let text = |terminal: &Terminal<TestBackend>| -> String {
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect()
        };
        assert!(text(&terminal).contains("OLD-FRAME"));
        assert!(!text(&terminal).contains("PARTIAL-NEW"));
        parser.lock().unwrap().process(b"\x1b[?2026l");
        terminal
            .draw(|frame| render(frame, &mut fixture.app))
            .unwrap();
        assert!(text(&terminal).contains("PARTIAL-NEW"));
        assert!(!text(&terminal).contains("OLD-FRAME"));
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

    #[test]
    fn pane_status_border_survives_focus_zoom_category_and_narrow_width() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut fixture = QuietPtyFixture::new(6, 10);
        let id = fixture.app.panes[0].clone();
        for (status, expected) in [
            (SessionStatus::Idle, Color::Rgb(140, 148, 162)),
            (SessionStatus::Working, Color::Rgb(240, 212, 120)),
            (SessionStatus::Completed, Color::Rgb(145, 217, 174)),
            (SessionStatus::Blocked, Color::Rgb(255, 183, 131)),
            (SessionStatus::Ended, Color::Rgb(174, 183, 198)),
        ] {
            fixture.app.hook_status.insert(
                id.clone(),
                crate::agent_hooks::HookReading {
                    status,
                    event_epoch_secs: 1,
                    revision: 0,
                },
            );
            for width in [12, 24, 48] {
                for (focused, zoomed) in [(false, false), (true, false), (true, true)] {
                    let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
                    terminal
                        .draw(|frame| {
                            render_pane(
                                frame,
                                &mut fixture.app,
                                &id,
                                0,
                                7,
                                Rect::new(0, 0, width, 8),
                                focused,
                                zoomed,
                                Some(("long-category-name", CATEGORY)),
                            )
                        })
                        .unwrap();
                    assert_eq!(
                        terminal.backend().buffer()[(0, 2)].fg,
                        expected,
                        "{status:?} width={width} focused={focused} zoomed={zoomed}"
                    );
                    assert_eq!(terminal.backend().buffer()[(width - 1, 2)].fg, expected);
                }
            }
        }
    }

    #[test]
    fn narrow_pane_keeps_status_glyph_before_long_category() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut fixture = QuietPtyFixture::new(6, 10);
        let id = fixture.app.panes[0].clone();
        fixture.app.hook_status.insert(
            id.clone(),
            crate::agent_hooks::HookReading {
                status: SessionStatus::Blocked,
                event_epoch_secs: 1,
                revision: 0,
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(12, 8)).unwrap();
        terminal
            .draw(|frame| {
                render_pane(
                    frame,
                    &mut fixture.app,
                    &id,
                    0,
                    7,
                    Rect::new(0, 0, 12, 8),
                    false,
                    false,
                    Some(("long-category-name", CATEGORY)),
                )
            })
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(1, 0)].symbol(), "!");
    }

    #[test]
    fn codex_stop_without_a_correlated_completion_is_only_idle() {
        let mut fixture = QuietPtyFixture::new(6, 10);
        let id = fixture.app.panes[0].clone();
        let mut codex = session(Agent::Codex, 0);
        codex.id = id.clone();
        fixture.app.all_sessions = vec![codex];
        fixture.app.hook_status.insert(
            id.clone(),
            crate::agent_hooks::HookReading {
                status: SessionStatus::Completed,
                event_epoch_secs: 1,
                revision: 0,
            },
        );
        assert_eq!(fixture.app.session_status(&id), SessionStatus::Idle);
    }

    #[test]
    fn bracketed_multiline_paste_keeps_completion_until_enter() {
        let mut fixture = QuietPtyFixture::new(6, 10);
        let id = fixture.app.panes[0].clone();
        fixture.app.hook_status.insert(
            id.clone(),
            crate::agent_hooks::HookReading {
                status: SessionStatus::Completed,
                event_epoch_secs: 1,
                revision: 0,
            },
        );
        fixture.app.ptys[&id]
            .parser()
            .lock()
            .unwrap()
            .process(b"\x1b[?2004h");
        assert!(fixture.app.paste_to_pty("draft\nsecond line"));
        assert_eq!(fixture.app.session_status(&id), SessionStatus::Completed);
        fixture.app.send_to_pty(b"\r");
        assert_eq!(fixture.app.session_status(&id), SessionStatus::Idle);
        fixture.app.hook_status.insert(
            id.clone(),
            crate::agent_hooks::HookReading {
                status: SessionStatus::Working,
                event_epoch_secs: 2,
                revision: 1,
            },
        );
        assert_eq!(fixture.app.session_status(&id), SessionStatus::Working);
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
