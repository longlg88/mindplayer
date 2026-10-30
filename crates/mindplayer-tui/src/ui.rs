//! Rendering for every screen. `render` also records the right-pane size back
//! into `App` so the PTY can be spawned/resized at the correct dimensions.

use crate::app::{App, CategoryMenu, Focus, PaneLayout, Row, Screen, SessionStatus, MAX_PANES};
use crate::local_router::{RoutingStage, RoutingStatus};
use crate::mascot;
use crate::startup_routing::{RoutingMode, StartupStage};
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
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph, Wrap,
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

/// Provider column text for the Vercel key's budget row.
const VERCEL_HEAD: &str = "vercel";

/// The Vercel key's budget as a quota row, so it shares the provider rows' columns.
fn vercel_quota_row(app: &App) -> mindplayer_core::limits::QuotaRow {
    use mindplayer_core::limits::{compact_decimal, QuotaRow};
    use mindplayer_core::vercel_usage::VercelUsageError;

    let reading = if app.vercel_cwd.as_ref() == Some(&app.cwd) {
        app.vercel_budget.as_ref()
    } else {
        None
    };
    let (label, used_percent, detail) = match reading {
        Some(Ok(usage)) => match &usage.key {
            Ok(budget) => {
                let limit = usage.target.unwrap_or(budget.limit);
                let limit_label = if usage.target.is_some() {
                    "local target"
                } else {
                    "API key limit"
                };
                let used = budget.used / limit * 100.0;
                (
                    format!("key {}", budget.refresh_period)
                        .trim_end()
                        .to_string(),
                    (limit > 0.0 && used.is_finite()).then_some(used),
                    format!(
                        "${:.2}/${} {limit_label}",
                        budget.used,
                        compact_decimal(limit)
                    ),
                )
            }
            Err(VercelUsageError::NoBudget) => {
                ("key".to_string(), None, "no active budget".to_string())
            }
            Err(_) => ("key".to_string(), None, "unavailable".to_string()),
        },
        Some(Err(_)) => (String::new(), None, "unavailable".to_string()),
        None => (String::new(), None, "loading".to_string()),
    };
    QuotaRow {
        label,
        used_percent,
        detail,
        ..Default::default()
    }
}

/// The Vercel row in the provider rows' columns; a figure that would run past the edge moves to its own lines so no amount is cut short.
fn vercel_footer_lines(
    row: &mindplayer_core::limits::QuotaRow,
    platform_width: usize,
    name_width: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let head = Span::styled(
        format!(" {VERCEL_HEAD:<platform_width$} "),
        Style::default()
            .fg(quota_label_color(VERCEL_HEAD))
            .add_modifier(Modifier::BOLD),
    );
    let width = usize::from(width.max(1));
    let mut full = vec![head.clone()];
    full.extend(quota_row_spans(row, name_width, None));
    if Line::from(full.clone()).width() <= width {
        return vec![Line::from(full)];
    }

    let bare = mindplayer_core::limits::QuotaRow {
        detail: String::new(),
        ..row.clone()
    };
    let mut first = vec![head];
    first.extend(quota_row_spans(&bare, name_width, None));
    let indent = platform_width + 4;
    let room = width.saturating_sub(indent);
    let mut wrapped: Vec<String> = Vec::new();
    for word in row.detail.split_whitespace() {
        let word_width = Line::from(word).width();
        match wrapped.last_mut() {
            Some(line) if Line::from(line.as_str()).width() + 1 + word_width <= room => {
                line.push(' ');
                line.push_str(word);
            }
            _ => wrapped.push(word.to_string()),
        }
    }
    if wrapped.len() > 2
        || wrapped
            .iter()
            .any(|line| Line::from(line.as_str()).width() > room)
    {
        wrapped = vec!["widen terminal".to_string()];
    }
    let style = Style::default().fg(quota_detail_color(row));
    let mut lines = vec![Line::from(first)];
    lines.extend(
        wrapped
            .into_iter()
            .map(|text| Line::from(Span::styled(format!("{:indent$}{text}", ""), style))),
    );
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
    let retry = || match app.limits_retry_at {
        Some(at) if at > std::time::Instant::now() => {
            format!(
                "retrying in {} · Ctrl-R now",
                short_wait(at - std::time::Instant::now())
            )
        }
        _ => "retrying · Ctrl-R now".to_string(),
    };
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
            (format!("sign-in needed · {action}"), ZOOM)
        }
        FailureKind::Temporary => (format!("temporarily unavailable · {}", retry()), DIM),
        FailureKind::RateLimited => (format!("rate limited · {}", retry()), DIM),
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

/// A wait in the largest unit that matters: `45s`, `4m`, `2h`.
fn short_wait(wait: std::time::Duration) -> String {
    let secs = wait.as_secs();
    let minutes = secs.div_ceil(60);
    if secs < 60 {
        format!("{secs}s")
    } else if minutes < 60 {
        format!("{minutes}m")
    } else {
        format!("{}h", secs.div_ceil(3600))
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
            "  Codex / Claude / Kiro / Cursor / OMP session manager",
            Style::default().fg(DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn main_title_bar(area: Rect, f: &mut Frame, app: &App) {
    let Some(mode) = app.startup_routing.mode() else {
        title_bar(area, f);
        return;
    };
    if app.focus == Focus::Terminal
        && app
            .focused_pane()
            .and_then(|sid| pane_provider(app, sid))
            .is_some_and(|agent| agent != Agent::Codex)
    {
        title_bar(area, f);
        return;
    }
    let label = match mode {
        RoutingMode::Native => "CLI 그대로",
        RoutingMode::Jev => "Jev 사용 (Codex)",
        RoutingMode::PerPrompt if codex_input_choice_visible(app) => "Codex · F6 세션 유지",
        RoutingMode::PerPrompt => {
            title_bar(area, f);
            return;
        }
    };
    let mode = Line::from(Span::styled(label, Style::default().fg(ZOOM)));
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(mode.width() as u16 + 2),
        ])
        .split(area);
    title_bar(columns[0], f);
    f.render_widget(Paragraph::new(mode).alignment(Alignment::Right), columns[1]);
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

fn codex_input_choice_visible(app: &App) -> bool {
    app.startup_routing.mode() == Some(RoutingMode::PerPrompt)
        && (app.focus == Focus::List
            || app.focused_pane().and_then(|sid| pane_provider(app, sid)) == Some(Agent::Codex))
}

/// Gallery 07: a character-cell configuration sheet over the read-only list.
/// The existing full-width quota footer stays outside the sheet.
fn startup_configuration_sheet(f: &mut Frame, app: &App, body: Rect) {
    if body.is_empty() || app.startup_routing.stage == StartupStage::Complete {
        return;
    }
    let margin = u16::from(body.width > 2);
    let width = body.width.saturating_sub(margin * 2).min(72);
    let wide = width >= 60;
    let spacious = wide && body.height >= 23;
    let padding = if wide { 3 } else { u16::from(width > 4) };
    let content_width = width.saturating_sub(2 + padding * 2).max(1);
    let dim = Style::default().fg(DIM);
    let strong = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let mut lines = Vec::new();
    if spacious {
        lines.extend([Line::default(), Line::default()]);
    }
    lines.push(Line::styled("적용 범위     이번 MindPlayer 실행", dim));
    if spacious {
        lines.push(Line::default());
    }
    lines.push(Line::styled("세션          기존 재개 + 새 세션", dim));
    if spacious {
        lines.push(Line::default());
    }
    lines.push(Line::styled("─".repeat(usize::from(content_width)), dim));
    if spacious {
        lines.push(Line::default());
    }
    let selected = match app.startup_routing.selection {
        RoutingMode::Native => "◀ Jev 사용 안 함 ▶",
        RoutingMode::Jev => "◀ Jev 사용 ▶",
        RoutingMode::PerPrompt => return,
    };
    if !wide {
        lines.push(Line::styled("모델 판단", strong));
    }
    let model_row = if wide {
        format!("모델 판단     {selected}")
    } else {
        selected.to_string()
    };
    let fill = usize::from(content_width).saturating_sub(Span::raw(&model_row).width());
    lines.push(Line::styled(
        format!("{model_row}{}", " ".repeat(fill)),
        strong.bg(Color::Rgb(48, 60, 96)),
    ));
    if spacious {
        lines.extend([Line::default(), Line::default(), Line::default()]);
    }
    let keys = match app.startup_routing.stage {
        StartupStage::Choice => {
            match app.startup_routing.selection {
                RoutingMode::Native => {
                    lines.push(Line::styled(
                        if wide {
                            "실행          원래 CLI 설정 그대로"
                        } else {
                            "실행  원래 CLI 설정 그대로"
                        },
                        dim,
                    ));
                    lines.push(Line::styled("Jev 호출 없음 · 외부 전송 없음", dim));
                }
                RoutingMode::Jev => {
                    lines.push(Line::styled(
                        if wide {
                            "대상          Codex · 모델 + effort"
                        } else {
                            "대상  Codex · 모델 + effort"
                        },
                        dim,
                    ));
                    lines.push(Line::styled(
                        "입력 프롬프트가 외부 Jev로 전송됩니다.",
                        Style::default().fg(ZOOM),
                    ));
                    lines.push(Line::styled("Enter로 전송 동의", Style::default().fg(ZOOM)));
                    lines.push(Line::styled(
                        "Claude / Kiro / Cursor는 기존 CLI 그대로",
                        dim,
                    ));
                }
                RoutingMode::PerPrompt => return,
            }
            "←→ 변경 · Enter 시작 · Esc 종료"
        }
        StartupStage::Checking => {
            lines.push(Line::styled("Jev 시작 준비 확인 중", strong));
            lines.push(Line::styled("로컬 실행 조건을 확인하고 있습니다.", dim));
            lines.push(Line::styled("아직 프롬프트를 전송하지 않았습니다.", dim));
            "Esc 선택 화면으로 돌아가기"
        }
        StartupStage::Blocked => {
            lines.push(Line::styled(
                "Jev로 시작할 수 없음",
                Style::default().fg(ERROR).add_modifier(Modifier::BOLD),
            ));
            "Esc 선택 화면으로 돌아가기"
        }
        StartupStage::Complete => return,
    };
    if let Some(error) = app.startup_routing.error.or_else(|| {
        (app.startup_routing.stage == StartupStage::Blocked)
            .then_some("Jev 준비 확인을 완료하지 못했습니다.")
    }) {
        // Causes come from fixed startup messages. Bound even a future longer
        // message to two rows and keep control characters out of the terminal.
        let cause: String = error
            .chars()
            .filter(|ch| !ch.is_control())
            .take(240)
            .collect();
        lines.extend(
            wrapped_text_rows(&cause, usize::from(content_width))
                .into_iter()
                .take(2)
                .map(|line| Line::styled(line, Style::default().fg(ERROR))),
        );
    }
    if app.startup_routing.stage == StartupStage::Blocked {
        lines.push(Line::styled("자동으로 미사용 전환하지 않습니다.", dim));
    }
    lines.push(Line::styled("선택 저장     하지 않음", dim));
    let key_line = Paragraph::new(keys).style(dim).wrap(Wrap { trim: false });
    let key_h = key_line.line_count(content_width) as u16;
    let content_h = Paragraph::new(lines.clone())
        .wrap(Wrap { trim: false })
        .line_count(content_width) as u16;
    let height = (content_h + key_h + 3).max(21).min(body.height);
    let area = Rect::new(
        body.x + body.width.saturating_sub(width) / 2,
        body.y + body.height.saturating_sub(height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" MindPlayer / 실행 설정표 ")
        .padding(Padding::horizontal(padding));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(key_h + 1)])
        .split(inner);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rows[0]);
    f.render_widget(key_line, rows[1]);
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
            Span::styled("   omp  ", Style::default().fg(Color::Green)),
            Span::raw(format!("{:>3}", a.omp_count)),
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
                    "   (codex {} · claude {} · kiro {} · cursor {} · omp {})",
                    human_tokens(a.codex.total),
                    human_tokens(a.claude.total),
                    if a.kiro_count > 0 { "—" } else { "0" },
                    if a.cursor_count > 0 { "—" } else { "0" },
                    human_tokens(a.omp.total),
                ),
                Style::default().fg(DIM),
            ),
        ])),
        rows[4],
    );

    footer(chunks[2], f, "enter open mindplayer   q quit");
}

fn main_view(f: &mut Frame, app: &mut App) {
    let startup_active = app.startup_routing.active();
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
        if !row.account.is_empty() && app.provider_has_several_logins(row.agent) {
            rest = if rest.is_empty() {
                row.account.clone()
            } else {
                format!("{} {rest}", row.account)
            };
        }
        row.label = rest;
    }
    let vercel_row = vercel_quota_row(app);
    let name_width = quota_rows
        .iter()
        .chain(std::iter::once(&vercel_row))
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0);
    let platform_width = quota_rows
        .iter()
        .map(|r| r.agent.as_str().chars().count())
        .chain(std::iter::once(VERCEL_HEAD.chars().count()))
        .max()
        .unwrap_or(0);
    let vercel_lines = vercel_footer_lines(&vercel_row, platform_width, name_width, f.area().width);
    let quota_h = (quota_rows.len() + vercel_lines.len()).min(u16::MAX as usize) as u16;
    let footer_h: u16 = status_h + quota_h + 1 + u16::from(show_more_keys);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(footer_h),
        ])
        .split(f.area());
    main_title_bar(outer[0], f, app);

    // Read the clock once per frame and thread it down, so the sort key and the
    // displayed relative times can't disagree (and we don't call Utc::now()
    // multiple times for a single render).
    let now = Utc::now();

    // Full-screen switch: the list OR the live session fills the body — no split.
    // Background sessions keep running regardless of which view is shown.
    if startup_active {
        if !outer[1].is_empty() {
            session_list(f, app, outer[1], now);
        }
    } else {
        match app.focus {
            Focus::List => session_list(f, app, outer[1], now),
            Focus::Terminal => live_pane(f, app, outer[1]),
        }
    }

    let keys: String = if startup_active {
        "이번 실행에만 적용 · 선택 저장 없음".to_string()
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
                "ctrl-x list · tab pane · ctrl-z zoom · ctrl-q close · other keys: OMP".to_string()
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
        // The provider is named once per group. A blank there reads as "the
        // one above", which is what makes the lines a group at a glance.
        let mut previous: Option<Agent> = None;
        let mut lines: Vec<Line> = quota_rows
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
                spans.extend(quota_row_spans(row, name_width, failure_hint(app, row)));
                Line::from(spans)
            })
            .collect();
        lines.extend(vercel_lines);
        f.render_widget(Paragraph::new(lines), footer_rows[1]);
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(keys, Style::default().fg(DIM))))
            .alignment(Alignment::Right),
        footer_rows[2],
    );
    if show_more_keys && !startup_active {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "i in progress · c catch-up · t category · ←→ fold    ctrl-r usage · ? help",
                Style::default().fg(Color::Rgb(90, 95, 108)),
            )))
            .alignment(Alignment::Right),
            footer_rows[3],
        );
    }

    if startup_active {
        startup_configuration_sheet(f, app, outer[1]);
        return;
    }

    if app.unusable_prompt.is_some() {
        unusable_account_popup(f, app);
    } else if app.help_visible {
        help_popup(f, app);
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
    let focused = app.focus == Focus::List && !app.startup_routing.active();
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
    // One directory on every row says nothing, so the column only appears once the list spans two.
    let show_cwd = area.width >= 78 && listed_cwds_differ(app);
    let account_width = account_marks.as_ref().map_or(0, |m| m.width() + 1);
    let identity_width = usize::from(show_id) * 11 + usize::from(show_cwd) * 11 + account_width;
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
            let count = match cat.as_deref() {
                Some(id) => app.category_session_count(Some(id)),
                None => app.loose_session_count(is_recent),
            };
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
                SessionStatus::Blocked => ("! blocked", WAITING_BORDER, true),
                SessionStatus::Working => ("● working", WORKING_BORDER, true),
                SessionStatus::Completed => ("✓ done   ", COMPLETED_BORDER, true),
                SessionStatus::Idle => ("○ idle   ", UNCATEGORIZED_BORDER, false),
                SessionStatus::Ended => ("○ ended  ", ENDED_BORDER, false),
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
                account_cell(account_marks.as_ref(), s),
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
    if !items.is_empty() && !app.startup_routing.active() {
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
    app.hero_visible = region_h >= block_h + 2 && !app.startup_routing.active();
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
    let codex_pane = pane_provider(app, sid) == Some(Agent::Codex);
    if codex_pane {
        // Use the existing bottom border: the account stays visible without taking
        // native terminal rows or competing with status/counts in the top title.
        let label = truncate_pane_account_label(
            app.pane_account_label(sid).unwrap_or("계정 미확인"),
            usize::from(pane_area.width.saturating_sub(12)),
        );
        block = block.title_bottom(Line::styled(
            format!(" codex · {label} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
    }
    let inner = block.inner(pane_area);
    f.render_widget(block.clone(), pane_area);
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
        let routing_status = codex_pane.then(|| pty.routing_status()).flatten();
        if let Ok(parser) = pty.parser().lock() {
            let screen = parser.render_screen();
            f.render_widget(TerminalView::new(screen).with_selection(selection), inner);
            if let Some(overlay) = routing_status
                .as_ref()
                .and_then(|status| routing_overlay(screen, status, selection.is_some()))
            {
                let color = routing_status
                    .as_ref()
                    .map_or(DIM, |status| routing_status_color(status.stage));
                match overlay {
                    RoutingOverlay::ModelRow {
                        row,
                        column,
                        label,
                        suffix,
                    } => {
                        paint_routing_model_row(f, inner, row, column, &label, suffix, color);
                    }
                    RoutingOverlay::Border(label) => {
                        // Reuse the existing border; this adds no terminal row
                        // and leaves the PTY's dimensions and screen untouched.
                        f.render_widget(
                            block.clone().title_bottom(Line::from(Span::styled(
                                label,
                                Style::default().fg(color),
                            ))),
                            pane_area,
                        );
                    }
                }
            }
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

#[derive(Debug, PartialEq, Eq)]
enum RoutingOverlay {
    ModelRow {
        row: u16,
        column: u16,
        label: String,
        // Native suffix source column, target column and cell count. The
        // renderer copies its existing cells, including context/quota styles.
        suffix: Option<(u16, u16, u16)>,
    },
    Border(String),
}

fn routing_status_color(stage: RoutingStage) -> Color {
    match stage {
        RoutingStage::Completed => COMPLETED_BORDER,
        RoutingStage::Judging
        | RoutingStage::Requested
        | RoutingStage::Accepted
        | RoutingStage::Running => WORKING_BORDER,
        RoutingStage::Failed => ERROR,
        RoutingStage::Ready | RoutingStage::Interrupted | RoutingStage::Closed => DIM,
    }
}

fn routing_labels(status: &RoutingStatus) -> Vec<String> {
    // Lifecycle comes from correlated protocol events, independently of the
    // optional server model-change report. Requested effort is not a receipt.
    let lifecycle = match status.stage {
        RoutingStage::Ready => return vec!["입력 대기".into()],
        RoutingStage::Judging => return vec!["Jev 판단 중".into()],
        RoutingStage::Requested => "요청 중",
        RoutingStage::Accepted => "요청 수락",
        RoutingStage::Running => "실행 중",
        RoutingStage::Completed => "완료",
        RoutingStage::Interrupted => "중단",
        RoutingStage::Closed => return vec!["연결 종료".into()],
        RoutingStage::Failed => {
            return vec![match status.reason.as_deref() {
                Some("submission-uncertain-no-replay") => "송신 결과 미확인 · 재전송 안 함".into(),
                Some("execution-failed") => "실행 실패".into(),
                Some("submission-rejected") => "요청 거부".into(),
                Some("jev-tls-error") => "Jev 인증서 오류 · 미전송".into(),
                Some("jev-invalid-response") => "Jev 응답 오류 · 미전송".into(),
                _ if status.turn_id.is_none() => "미전송".into(),
                _ => "실행 실패".into(),
            }]
        }
    };
    let requested = status.model.as_deref().map(|model| {
        let effort = status
            .effort
            .as_ref()
            .map_or(String::new(), |effort| format!(" {effort}"));
        format!("요청 {model}{effort}")
    });
    let reported = status
        .observed_model
        .as_deref()
        .filter(|_| status.server_model_reported)
        .map(|model| format!("서버 변경 {model}"));
    let retention = match status.reason.as_deref() {
        Some("external-transfer-policy-unset") => Some("Jev 전송 금지 · 네이티브"),
        Some(
            "context-insufficient"
            | "context-truncated"
            | "context-unavailable"
            | "context-incomplete",
        ) => Some("유지 · 맥락 부족"),
        _ => None,
    };
    let selection = retention
        .or_else(|| (status.reason.as_deref() == Some("catalog-guided-jev")).then_some("Jev 선택"));
    let label = |selection: Option<&str>, requested: Option<&str>, reported: Option<&str>| {
        [Some(lifecycle), selection, requested, reported]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let mut labels = vec![label(selection, requested.as_deref(), reported.as_deref())];
    if reported.is_some() {
        labels.push(label(selection, requested.as_deref(), None));
    }
    if selection == Some("Jev 선택") {
        labels.push(label(None, requested.as_deref(), reported.as_deref()));
        labels.push(label(None, requested.as_deref(), None));
    }
    // Keep policy/retention reasons in compact labels too. Ordinary turns can
    // shrink to the lifecycle when a pane cannot fit the requested model.
    labels.push(label(retention, None, None));
    labels.dedup();
    labels
}

fn fit_routing_label(status: &RoutingStatus, width: u16) -> Option<String> {
    routing_labels(status).into_iter().find(|label| {
        !label.chars().any(char::is_control) && Line::from(label.as_str()).width() <= width as usize
    })
}

/// Recognize only the native GPT footer grammar. An arbitrary line mentioning
/// a model, an editable prompt, or a menu entry is never an overlay target.
fn native_model_columns(line: &str) -> Option<(u16, u16, u16)> {
    let trimmed = line.trim_start();
    let model = trimmed.split_whitespace().next()?;
    let lower = model.to_ascii_lowercase();
    if !lower.starts_with("gpt-")
        || !lower.as_bytes().get(4).is_some_and(u8::is_ascii_digit)
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
    {
        return None;
    }
    let mut rest = trimmed[model.len()..].trim_start();
    if let Some(token) = rest.split_whitespace().next() {
        if matches!(
            token,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
        ) {
            rest = rest[token.len()..].trim_start();
        }
    }
    rest = rest
        .strip_prefix('·')
        .or_else(|| rest.strip_prefix('|'))?
        .trim_start();
    let first = rest.split(['·', '|']).next()?.trim().to_ascii_lowercase();
    let percent = first.find('%')?;
    let numeric_left = !first[..percent].trim().is_empty()
        && first[..percent]
            .trim()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && matches!(
            first[percent + 1..].trim(),
            "left" | "context left" | "context remaining"
        );
    if !numeric_left && !first.starts_with("context ") && !first.starts_with("weekly ") {
        return None;
    }
    let column = Line::from(&line[..line.len() - trimmed.len()]).width() as u16;
    let suffix_column = Line::from(&line[..line.len() - rest.len()]).width() as u16;
    let suffix_width = Line::from(rest.trim_end()).width() as u16;
    Some((column, suffix_column, suffix_width))
}

fn routing_overlay(
    screen: &vt100::Screen,
    status: &RoutingStatus,
    selected: bool,
) -> Option<RoutingOverlay> {
    let (rows, cols) = screen.size();
    if selected
        || screen.scrollback() > 0
        || rows < 4
        || crate::pty::routing_overlay_blocked(screen)
    {
        return None;
    }
    let cursor_row = screen.cursor_position().0;
    let candidates: Vec<_> = screen
        .rows(0, cols)
        .enumerate()
        .skip(rows.saturating_sub(8) as usize)
        .filter_map(|(row, line)| {
            native_model_columns(&line).map(|columns| (row as u16, columns, line))
        })
        .collect();
    if let [(row, (column, suffix_column, suffix_width), line)] = candidates.as_slice() {
        // The native UI may briefly repaint or move its input here. Hidden
        // cursors still carry an address, so exclude the row unconditionally.
        if *row == cursor_row {
            return None;
        }
        let labels = if status.stage == RoutingStage::Ready {
            let model_prefix = line.trim_start().split(['·', '|']).next()?.trim();
            vec![format!("{model_prefix} · 입력 대기")]
        } else {
            routing_labels(status)
        };
        let fits = |label: &&String, width: u16| {
            !label.chars().any(char::is_control)
                && Line::from(label.as_str()).width() <= width as usize
        };
        // Prefer a compact model-bearing label that keeps both native usage
        // values, before spending the whole row on the detailed status.
        let available = cols.saturating_sub(*column);
        let label = labels
            .iter()
            .find(|label| {
                status
                    .model
                    .as_deref()
                    .is_none_or(|model| label.contains(model))
                    && fits(
                        label,
                        available.saturating_sub(suffix_width.saturating_add(2)),
                    )
            })
            .or_else(|| labels.iter().find(|label| fits(label, available)));
        if let Some(label) = label {
            let label_end = column.saturating_add(Line::from(label.as_str()).width() as u16);
            let target = (*suffix_column).max(label_end.saturating_add(2));
            let suffix = (target.saturating_add(*suffix_width) <= cols).then_some((
                *suffix_column,
                target,
                *suffix_width,
            ));
            return Some(RoutingOverlay::ModelRow {
                row: *row,
                column: *column,
                label: label.clone(),
                suffix,
            });
        }
        if status.stage == RoutingStage::Ready {
            return None;
        }
    }
    fit_routing_label(status, cols).map(RoutingOverlay::Border)
}

fn paint_routing_model_row(
    f: &mut Frame,
    inner: Rect,
    row: u16,
    column: u16,
    label: &str,
    suffix: Option<(u16, u16, u16)>,
    color: Color,
) {
    let y = inner.y + row;
    let native_suffix = suffix.map(|(source, target, width)| {
        let cells: Vec<_> = (source..source + width)
            .map(|column| f.buffer_mut()[(inner.x + column, y)].clone())
            .collect();
        (target, cells)
    });
    for x in column..inner.width {
        f.buffer_mut()[(inner.x + x, y)].set_symbol(" ");
    }
    f.render_widget(
        Paragraph::new(Span::styled(label, Style::default().fg(color))),
        Rect::new(inner.x + column, y, inner.width.saturating_sub(column), 1),
    );
    if let Some((target, cells)) = native_suffix {
        for (offset, cell) in cells.into_iter().enumerate() {
            f.buffer_mut()[(inner.x + target + offset as u16, y)] = cell;
        }
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

fn help_popup(f: &mut Frame, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" Keyboard shortcuts ");
    let lines = help_lines(codex_input_choice_visible(app));
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
fn help_lines(codex_input_choice: bool) -> Vec<Line<'static>> {
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
    let mut lines = vec![
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
        item("a", "toggle archived sessions"),
        Line::from(""),
        section("Terminal / Modal"),
        item("ctrl-x", "return from terminal to session list"),
        item("tab / shift-tab", "cycle live panes when 2+ open (OMP keeps shift-tab)"),
        item("ctrl-w / alt-w", "cycle live panes (alt-w in OMP)"),
        item("ctrl-z", "zoom the focused pane full-size (toggle back to split)"),
        item("ctrl-q / alt-q", "close focused pane (alt-q also works in OMP)"),
        item(
            "ctrl-t",
            "transition report outside OMP: topic/RUNBOOK/files -> review -> enter sends, e edits",
        ),
        item(
            "ctrl-p",
            "open a local .html file outside OMP: pick from detected files or tab to type a path",
        ),
        item(
            "ctrl-y",
            "copy a link from the focused pane's most recent answer outside OMP",
        ),
        item("shift/alt/ctrl-enter", "insert newline in text modals"),
        item("esc", "cancel modal or close this help"),
        item("?", "show or close this help"),
    ];
    if codex_input_choice {
        lines.push(item(
            "F6 (Codex)",
            "Jev ON/OFF · 세션 유지 · 키가 있으면 기본 ON · ON은 외부 전송 동의",
        ));
    }
    lines
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

/// The usage column for one session row.
///
/// Each agent records something different and the column shows what it has:
/// codex and claude count tokens per request, kiro reports only how full its
/// context window is, and cursor records nothing at all — its figure is
/// estimated from the transcript, so it carries a `~` to keep it from reading
/// as a count. Nothing to show reads "—", never `0`, which would claim the
/// session spent nothing.
/// Colors for a provider's logins in list order; the machine's own login stays quiet.
const ACCOUNT_TONES: [Color; 4] = [DIM, CATEGORY, PREVIEW, ZOOM];

/// The account column: the login's name on rows of a provider with several, blank padding on the rest, nothing when no provider has a second login.
fn account_cell(
    marks: Option<&crate::app::accounts_panel::AccountMarks>,
    s: &Session,
) -> Span<'static> {
    let Some(marks) = marks else {
        return Span::raw("");
    };
    let width = marks.width();
    match marks.label_for(s) {
        Some((name, tone)) => Span::styled(
            format!("{:<width$} ", truncate(name, width)),
            Style::default().fg(ACCOUNT_TONES[tone % ACCOUNT_TONES.len()]),
        ),
        None => Span::raw(" ".repeat(width + 1)),
    }
}

/// True when the listed sessions sit in more than one directory.
fn listed_cwds_differ(app: &App) -> bool {
    let mut first: Option<&Path> = None;
    (0..app.visible.len())
        .filter_map(|row| app.session_at(row))
        .any(|s| match first {
            None => {
                first = Some(s.cwd.as_path());
                false
            }
            Some(cwd) => cwd != s.cwd.as_path(),
        })
}

fn usage_cell(s: &Session) -> String {
    match s.agent {
        Agent::Kiro => match s.context_pct {
            Some(p) => format!("ctx {p:.0}%"),
            None => "—".to_string(),
        },
        Agent::Cursor if s.tokens.total == 0 => "—".to_string(),
        Agent::Cursor => format!("~{} tok", human_tokens(s.tokens.total)),
        Agent::Codex | Agent::Claude | Agent::Omp => {
            format!("{} tok", human_tokens(s.tokens.total))
        }
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
                    if matches!(account.provider, Agent::Codex | Agent::Claude) {
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
                    lines.push(Line::from(spans));
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
mod tests {
    use super::*;
    use crate::pty::PtySession;
    use crate::startup_routing::StartupRouting;

    fn startup_app() -> App {
        let mut app = isolated_app();
        app.screen = Screen::Main;
        app.focus = Focus::List;
        app.startup_routing = StartupRouting::default();
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

    #[test]
    fn session_jev_starts_at_the_list_with_a_persistent_f6_hint() {
        let mut app = startup_app();
        app.startup_routing = StartupRouting::per_prompt();
        for (width, height) in [(88, 29), (140, 40), (40, 24)] {
            let text = frame_text(&startup_frame(&mut app, width, height));
            assert!(
                text.lines()
                    .next()
                    .unwrap()
                    .contains("Codex · F6 세션 유지"),
                "{text}"
            );
            assert!(!text.contains("MindPlayer / 실행 설정표"), "{text}");
            assert!(!text.contains("Jev 시작 준비"), "{text}");
            assert!(text.contains("enter open"), "{text}");
            assert!(!app.startup_routing.active());
            assert!(app.ptys.is_empty());
        }
    }

    #[test]
    fn session_jev_header_and_help_hide_controls_in_non_codex_panes() {
        let mut app = startup_app();
        app.startup_routing = StartupRouting::per_prompt();
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
            assert_eq!(
                text.contains("Codex · F6 세션 유지"),
                agent == Agent::Codex,
                "{text}"
            );
            app.help_visible = true;
            let help = frame_text(&startup_frame(&mut app, 140, 60));
            assert_eq!(help.contains("F6 (Codex)"), agent == Agent::Codex, "{help}");
            assert!(!help.contains("F7"), "{help}");
            if agent == Agent::Codex {
                assert!(help.contains("ON/OFF"), "{help}");
                assert!(help.contains("세션 유지"), "{help}");
                assert!(help.contains("키가 있으면 기본 ON"), "{help}");
            }
            if agent != Agent::Codex {
                assert!(!help.contains("Jev"), "{help}");
            }
            app.help_visible = false;
        }
    }

    #[test]
    fn claude_panes_never_render_a_stale_jev_status_channel() {
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Judging, 8, 20);
        let id = fixture.id();
        fixture
            .quiet
            .app
            .pane_accounts
            .insert(id.clone(), (Agent::Claude, "native".into()));
        fixture.quiet.app.startup_routing = StartupRouting::per_prompt();
        paint_native_footer(
            &mut fixture.quiet.app.ptys[&id].parser().lock().unwrap(),
            "claude native model",
        );
        let text = frame_text(&startup_frame(&mut fixture.quiet.app, 140, 40));
        assert!(text.contains("native draft"), "{text}");
        assert!(!text.contains("Jev"), "{text}");
        assert!(!text.contains("입력마다 선택"), "{text}");
    }

    fn pane_account_app() -> App {
        let mut app = startup_app();
        app.startup_routing = StartupRouting::per_prompt();
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
            app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
            let mut account = mindplayer_core::accounts::Account::inherited(provider);
            account.set_identity_fingerprint(Some("saved-identity".into()));
            app.accounts = vec![account];
            app.open_accounts();
            let text = frame_text(&startup_frame(&mut app, 170, 35));
            assert!(text.contains("identity bound"), "{text}");
            assert!(!text.contains("identity verified"), "{text}");
        }
    }

    #[test]
    fn startup_gallery_seven_centers_a_run_configuration_sheet_above_the_footer() {
        for (width, height) in [(88, 29), (140, 40)] {
            for selection in [RoutingMode::Native, RoutingMode::Jev] {
                let mut app = startup_app();
                app.startup_routing.selection = selection;
                let buffer = startup_frame(&mut app, width, height);
                let text = frame_text(&buffer);
                let rows: Vec<_> = text.lines().collect();
                let sheet_y = rows
                    .iter()
                    .position(|row| row.contains("MindPlayer / 실행 설정표"))
                    .unwrap();
                assert!(sheet_y > 1 && sheet_y < usize::from(height / 2), "{text}");
                let sheet_x = (width - 72) / 2;
                assert_eq!(buffer[(sheet_x, sheet_y as u16)].symbol(), "┌", "{text}");
                assert_eq!(
                    buffer[(sheet_x + 71, sheet_y as u16)].symbol(),
                    "┐",
                    "{text}"
                );
                assert!(
                    rows[..sheet_y].iter().any(|row| row.contains("Sessions")),
                    "{text}"
                );
                for label in [
                    "적용 범위",
                    "이번 MindPlayer 실행",
                    "세션",
                    "기존 재개 + 새 세션",
                    "모델 판단",
                    "선택 저장",
                    "하지 않음",
                    "←→ 변경",
                    "Enter 시작",
                    "Esc 종료",
                ] {
                    assert!(text.contains(label), "missing {label}: {text}");
                }
                let selected_label = match selection {
                    RoutingMode::Native => "◀ Jev 사용 안 함 ▶",
                    RoutingMode::Jev => "◀ Jev 사용 ▶",
                    RoutingMode::PerPrompt => unreachable!("legacy choice fixture"),
                };
                assert!(text.contains(selected_label), "{text}");
                let model_y = rows
                    .iter()
                    .position(|row| row.contains(selected_label))
                    .unwrap() as u16;
                assert_eq!(buffer[(sheet_x + 4, model_y)].bg, Color::Rgb(48, 60, 96));
                assert!(text.contains("sessions ·"), "{text}");
                assert!(!rows[0].contains("Jev 사용 (Codex)"));
                assert!(!rows[0].contains("CLI 그대로"));
                assert!(app.startup_routing.active());
                assert!(app.ptys.is_empty());
                assert!(app.pending.is_none());
                assert!(!app.hero_visible);
                assert_eq!(app.all_sessions[0].title, "real-codex");
                for row in 1..sheet_y as u16 {
                    assert!(
                        (0..width)
                            .all(|column| { buffer[(column, row)].bg != Color::Rgb(48, 60, 96) }),
                        "startup session rows must be read-only: {text}"
                    );
                }
            }
        }
    }

    #[test]
    fn startup_gallery_seven_wraps_narrow_sheet_without_losing_the_choice_or_exit() {
        for (width, height) in [(40, 24), (52, 29), (60, 24)] {
            for selection in [RoutingMode::Native, RoutingMode::Jev] {
                let mut app = startup_app();
                app.startup_routing.selection = selection;
                let buffer = startup_frame(&mut app, width, height);
                let text = frame_text(&buffer);
                assert!(text.contains("MindPlayer / 실행 설정표"), "{text}");
                assert!(text.contains("적용 범위"), "{text}");
                assert!(text.contains("모델 판단"), "{text}");
                assert!(
                    text.contains(match selection {
                        RoutingMode::Native => "◀ Jev 사용 안 함 ▶",
                        RoutingMode::Jev => "◀ Jev 사용 ▶",
                        RoutingMode::PerPrompt => unreachable!("legacy choice fixture"),
                    }),
                    "{text}"
                );
                assert!(text.contains("Enter"), "{text}");
                assert!(text.contains("Esc 종료"), "{text}");
                assert!(!text.contains("enter open"), "{text}");
                assert!(text.contains("sessions ·"), "{text}");
            }
        }
    }

    #[test]
    fn startup_sheet_waits_for_main_and_never_reappears_after_confirmation() {
        let mut app = startup_app();
        for screen in [Screen::ScopeSelect, Screen::Scanning, Screen::ScanSummary] {
            app.screen = screen;
            let text = frame_text(&startup_frame(&mut app, 88, 29));
            assert!(!text.contains("MindPlayer / 실행 설정표"), "{text}");
            assert!(app.startup_routing.active());
        }
        app.screen = Screen::Main;
        assert!(frame_text(&startup_frame(&mut app, 88, 29)).contains("MindPlayer / 실행 설정표"));
        app.startup_routing.confirm();
        for focus in [Focus::List, Focus::Terminal, Focus::List] {
            app.focus = focus;
            let text = frame_text(&startup_frame(&mut app, 88, 29));
            assert!(
                text.lines().next().unwrap().contains("CLI 그대로"),
                "{text}"
            );
            assert!(!text.contains("MindPlayer / 실행 설정표"), "{text}");
        }
    }

    #[test]
    fn startup_sheet_precedes_unusable_help_and_other_popups_even_with_terminal_focus() {
        let mut app = startup_app();
        app.focus = Focus::Terminal;
        app.panes = vec!["not-started".into()];
        app.help_visible = true;
        app.unusable_prompt = Some(mindplayer_core::accounts::Account::inherited(Agent::Codex));
        app.new_picker = Some(0);
        for stage in [
            StartupStage::Choice,
            StartupStage::Checking,
            StartupStage::Blocked,
        ] {
            app.startup_routing.stage = stage;
            let text = frame_text(&startup_frame(&mut app, 88, 29));
            assert!(text.contains("MindPlayer / 실행 설정표"), "{text}");
            assert!(!text.contains("Account unavailable"), "{text}");
            assert!(!text.contains("Keyboard shortcuts"), "{text}");
            assert!(!text.contains("starting..."), "{text}");
            assert!(app.pane_bounds.is_empty());
            assert!(app.ptys.is_empty());
        }
    }

    #[test]
    fn startup_checking_is_bounded_local_readiness_and_keeps_back_visible() {
        let mut app = startup_app();
        app.startup_routing.selection = RoutingMode::Jev;
        app.startup_routing.stage = StartupStage::Checking;
        for (width, height) in [(88, 29), (140, 40), (40, 24)] {
            let text = frame_text(&startup_frame(&mut app, width, height));
            assert!(text.contains("Jev 시작 준비 확인 중"), "{text}");
            assert!(text.contains("로컬 실행 조건"), "{text}");
            assert!(text.contains("Esc 선택 화면으로 돌아가기"), "{text}");
            assert!(!text.contains("인증 완료"));
            assert!(!text.contains("잔액 확인"));
            assert_eq!(app.startup_routing.mode(), None);
        }
    }

    #[test]
    fn startup_jev_discloses_external_transfer_before_enter_without_a_consent_checkbox() {
        let mut app = startup_app();
        app.startup_routing.selection = RoutingMode::Jev;
        for (width, height) in [(88, 29), (140, 40), (40, 24), (52, 29)] {
            let buffer = startup_frame(&mut app, width, height);
            let text = frame_text(&buffer);
            // Ignore character-cell borders when joining wrapped notice rows.
            let content = text
                .lines()
                .map(|line| line.trim_matches(|ch| ch == '│' || ch == ' '))
                .collect::<Vec<_>>()
                .join(" ");
            let words = content.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(words.contains("외부 Jev로 전송됩니다."), "{text}");
            assert!(text.contains("Codex"), "{text}");
            assert!(text.contains("모델 + effort"), "{text}");
            assert!(text.contains("Claude / Kiro / Cursor"), "{text}");
            assert!(text.contains("Enter로 전송 동의"), "{text}");
            for obsolete in ["[x]", "[ ]", "c 동의", "전송 동의 후 시작"] {
                assert!(!text.contains(obsolete), "{text}");
            }
            assert!(text.contains("Esc 종료"), "{text}");
            assert_eq!(app.startup_routing.mode(), None);
        }
    }

    #[test]
    fn startup_blocked_bounds_the_cause_and_never_falls_back_to_native() {
        let mut app = startup_app();
        app.startup_routing.selection = RoutingMode::Jev;
        app.startup_routing.stage = StartupStage::Blocked;
        app.startup_routing.error = Some(concat!(
            "설정 실패\n\x1b ",
            "원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 ",
            "원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 ",
            "원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 ",
            "원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 원인 ",
            "끝표시"
        ));
        for (width, height) in [(88, 29), (140, 40), (40, 24)] {
            let text = frame_text(&startup_frame(&mut app, width, height));
            assert!(text.contains("Jev로 시작할 수 없음"), "{text}");
            assert!(text.contains("설정 실패"), "{text}");
            assert!(
                !text.contains("끝표시"),
                "a long cause must be capped: {text}"
            );
            assert!(
                text.lines().filter(|row| row.contains("원인")).count() <= 2,
                "{text}"
            );
            assert!(!text.contains('\x1b'));
            assert!(
                text.contains("자동으로 미사용 전환하지 않습니다."),
                "{text}"
            );
            assert!(text.contains("Esc 선택 화면으로 돌아가기"), "{text}");
            assert!(!text.lines().next().unwrap().contains("CLI 그대로"));
            assert!(app.startup_routing.active());
            assert_eq!(app.startup_routing.mode(), None);
            assert_eq!(app.startup_routing.selection, RoutingMode::Jev);
        }
    }

    #[test]
    fn startup_confirmed_mode_is_read_only_in_list_and_live_titles_at_every_width() {
        for mode in [RoutingMode::Native, RoutingMode::Jev] {
            let mut app = startup_app();
            app.startup_routing = StartupRouting::confirmed(mode);
            for (width, height) in [(88, 29), (140, 40), (40, 24)] {
                for focus in [Focus::List, Focus::Terminal, Focus::List] {
                    app.focus = focus;
                    let text = frame_text(&startup_frame(&mut app, width, height));
                    let label = match mode {
                        RoutingMode::Native => "CLI 그대로",
                        RoutingMode::Jev => "Jev 사용 (Codex)",
                        RoutingMode::PerPrompt => unreachable!("legacy mode fixture"),
                    };
                    assert!(text.lines().next().unwrap().contains(label), "{text}");
                    assert!(!text.contains("MindPlayer / 실행 설정표"), "{text}");
                    assert_eq!(app.startup_routing.mode(), Some(mode));
                }
            }
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

    #[test]
    fn vercel_usage_renders_local_target_for_only_the_key() {
        use mindplayer_core::vercel_usage::{VercelBudget, VercelUsage};
        let mut app = startup_app();
        app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
        app.vercel_cwd = Some(app.cwd.clone());
        app.vercel_budget = Some(Ok(VercelUsage {
            key: Ok(VercelBudget {
                used: 33.0,
                limit: 1000.0,
                refresh_period: "monthly".into(),
            }),
            target: Some(200.0),
        }));

        for width in [40, 88, 140] {
            let frame = frame_text(&startup_frame(&mut app, width, 29));
            assert!(
                vercel_line(&frame).starts_with("vercel key monthly ▰▱▱▱▱▱ 16.5%"),
                "{frame}"
            );
            assert!(frame.contains("$33.00/$200 local target"), "{frame}");
            assert!(!frame.contains("vercel user"), "{frame}");
            assert!(!frame.contains("vercel team"), "{frame}");
            assert!(!frame.contains("API key limit"), "{frame}");
            assert!(!frame.contains("$1000"), "{frame}");
        }
    }

    #[test]
    fn vercel_usage_uses_real_key_limit_without_a_local_target() {
        use mindplayer_core::vercel_usage::{VercelBudget, VercelUsage};
        let mut app = startup_app();
        app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
        app.vercel_cwd = Some(app.cwd.clone());
        app.vercel_budget = Some(Ok(VercelUsage {
            key: Ok(VercelBudget {
                used: 33.0,
                limit: 1000.0,
                refresh_period: "monthly".into(),
            }),
            target: None,
        }));
        let frame = frame_text(&startup_frame(&mut app, 140, 29));
        assert_eq!(
            vercel_line(&frame),
            "vercel key monthly ▱▱▱▱▱▱ 3.3% $33.00/$1000 API key limit",
            "{frame}"
        );
        assert!(!frame.contains("local target"), "{frame}");
        assert!(!frame.contains("vercel user"), "{frame}");
        assert!(!frame.contains("vercel team"), "{frame}");

        if let Some(Ok(usage)) = &mut app.vercel_budget {
            usage.key = Ok(VercelBudget {
                used: 123.75,
                limit: 100.0,
                refresh_period: "weekly".into(),
            });
        }
        let frame = frame_text(&startup_frame(&mut app, 140, 29));
        assert_eq!(
            vercel_line(&frame),
            "vercel key weekly ▰▰▰▰▰▰ 123.8% $123.75/$100 API key limit",
            "{frame}"
        );
    }

    #[test]
    fn vercel_usage_keeps_key_failures_and_absence_distinct() {
        use mindplayer_core::vercel_usage::{VercelUsage, VercelUsageError};
        let mut app = startup_app();
        app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
        app.vercel_cwd = Some(app.cwd.clone());
        for (error, expected) in [
            (VercelUsageError::Unavailable, "vercel key — unavailable"),
            (VercelUsageError::NoBudget, "vercel key — no active budget"),
        ] {
            app.vercel_budget = Some(Ok(VercelUsage {
                key: Err(error),
                target: None,
            }));
            let frame = frame_text(&startup_frame(&mut app, 140, 29));
            assert_eq!(vercel_line(&frame), expected, "{frame}");
            assert!(!frame.contains('$'), "{frame}");
        }

        for error in [
            VercelUsageError::NotConfigured,
            VercelUsageError::Unavailable,
            VercelUsageError::InvalidResponse,
            VercelUsageError::KeyNotIdentified,
            VercelUsageError::NoBudget,
        ] {
            app.vercel_budget = Some(Err(error));
            let frame = frame_text(&startup_frame(&mut app, 140, 29));
            assert_eq!(vercel_line(&frame), "vercel — unavailable", "{frame}");
            assert!(!frame.contains('$'), "{frame}");
        }
        app.cwd.push("different-workspace");
        let frame = frame_text(&startup_frame(&mut app, 140, 29));
        assert_eq!(vercel_line(&frame), "vercel — loading", "{frame}");
    }

    #[test]
    fn vercel_usage_never_displays_clipped_amounts() {
        use mindplayer_core::vercel_usage::{VercelBudget, VercelUsage};
        let mut app = startup_app();
        app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
        app.vercel_cwd = Some(app.cwd.clone());
        app.vercel_budget = Some(Ok(VercelUsage {
            key: Ok(VercelBudget {
                used: 1234567890123456.0,
                limit: 0.0,
                refresh_period: "monthly".into(),
            }),
            target: None,
        }));
        let frame = frame_text(&startup_frame(&mut app, 88, 29));
        assert_eq!(
            vercel_line(&frame),
            "vercel key monthly — $1234567890123456.00/$0 API key limit",
            "{frame}"
        );
        let frame = frame_text(&startup_frame(&mut app, 40, 29));
        assert!(frame.contains("$1234567890123456.00/$0"), "{frame}");
        let frame = frame_text(&startup_frame(&mut app, 12, 29));
        assert!(!frame.contains('$'), "{frame}");
    }

    #[test]
    fn vercel_row_shares_the_provider_rows_columns_and_rounds_dollars() {
        use mindplayer_core::limits::QuotaRow;
        use mindplayer_core::vercel_usage::{VercelBudget, VercelUsage};
        let mut app = startup_app();
        app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
        app.limits = Some(vec![
            QuotaRow {
                label: "codex weekly".into(),
                agent: Agent::Codex,
                used_percent: Some(43.0),
                resets: Some("11-04 17:02".into()),
                ..Default::default()
            },
            QuotaRow {
                label: "claude 5h".into(),
                agent: Agent::Claude,
                used_percent: Some(3.0),
                resets: Some("14:30".into()),
                ..Default::default()
            },
        ]);
        app.vercel_cwd = Some(app.cwd.clone());
        app.vercel_budget = Some(Ok(VercelUsage {
            key: Ok(VercelBudget {
                used: 34.336196356,
                limit: 1000.0,
                refresh_period: "monthly".into(),
            }),
            target: Some(200.0),
        }));

        for width in [88, 140] {
            let buffer = startup_frame(&mut app, width, 29);
            let frame = frame_text(&buffer);
            let quota_line = |provider: &str| {
                frame
                    .lines()
                    .find(|line| {
                        line.trim_start().starts_with(provider)
                            && line.chars().any(|c| matches!(c, '▰' | '▱'))
                    })
                    .unwrap_or_else(|| panic!("no {provider} quota row: {frame}"))
                    .to_string()
            };
            let column = |line: &str, mark: fn(char) -> bool| line.chars().position(mark).unwrap();
            let gauge = |c: char| matches!(c, '▰' | '▱');
            let percent = |c: char| c == '%';
            let rows = ["codex", "claude", "vercel"].map(quota_line);
            for row in &rows[1..] {
                assert_eq!(column(row, gauge), column(&rows[0], gauge), "{frame}");
                assert_eq!(column(row, percent), column(&rows[0], percent), "{frame}");
            }
            assert_eq!(
                vercel_line(&frame),
                "vercel key monthly ▰▱▱▱▱▱ 17.2% $34.34/$200 local target",
                "{frame}"
            );
            assert!(!frame.contains("34.336"), "{frame}");

            let y = (buffer.area.y..buffer.area.bottom())
                .find(|&y| {
                    rendered_row(&buffer, y)
                        .trim_start()
                        .starts_with(super::VERCEL_HEAD)
                })
                .unwrap();
            let line = rendered_row(&buffer, y);
            let fg = |mark: fn(char) -> bool| buffer[(column(&line, mark) as u16, y)].fg;
            assert_eq!(fg(|c| c == 'v'), Color::White, "{frame}");
            assert_eq!(fg(gauge), quota_tier(17.2), "{frame}");
            assert_eq!(fg(percent), quota_tier(17.2), "{frame}");
            assert_eq!(fg(|c| c == '$'), DIM, "{frame}");
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
        for startup in [true, false] {
            if !startup {
                app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
            }
            for (width, height) in [(88, 29), (140, 40)] {
                let text = frame_text(&startup_frame(&mut app, width, height));
                let quotas: Vec<_> = text
                    .lines()
                    .filter(|line| line.contains("weekly") || line.contains("credits"))
                    .collect();
                assert_eq!(quotas.len(), 4, "{text}");
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
    }

    #[test]
    fn startup_confirmed_modes_preserve_four_plus_three_and_full_width_quotas() {
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
            app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
            let native = startup_frame(&mut app, width, height);
            let bounds = app.pane_bounds.clone();
            app.startup_routing = StartupRouting::confirmed(RoutingMode::Jev);
            let jev = startup_frame(&mut app, width, height);
            assert_eq!(app.pane_bounds, bounds);
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
            for row in 1..height {
                for column in 0..width {
                    assert_eq!(
                        native[(column, row)],
                        jev[(column, row)],
                        "mode label changed body/footer at {column},{row}"
                    );
                }
            }
            let text = frame_text(&jev);
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

    #[test]
    fn startup_confirmed_jev_preserves_native_input_lifecycle_and_trace() {
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Ready, 8, 20);
        let id = fixture.id();
        fixture.quiet.app.limits = Some(Vec::new());
        fixture.quiet.app.quota_cache = None;
        fixture.quiet.app.startup_routing = StartupRouting::confirmed(RoutingMode::Jev);
        startup_frame(&mut fixture.quiet.app, 140, 40);
        for (stage, label) in [
            (RoutingStage::Ready, "입력 대기"),
            (RoutingStage::Judging, "Jev 판단 중"),
            (RoutingStage::Accepted, "요청 수락"),
            (RoutingStage::Running, "실행 중"),
            (RoutingStage::Completed, "완료"),
            (RoutingStage::Failed, "미전송"),
        ] {
            let pty = fixture.quiet.app.ptys.get_mut(&id).unwrap();
            let mut status = pty.routing_status().unwrap();
            status.stage = stage;
            status.revision += 1;
            std::fs::write(&fixture.path, serde_json::to_vec(&status).unwrap()).unwrap();
            assert!(pty.take_dirty());
            paint_native_footer(
                &mut pty.parser().lock().unwrap(),
                "gpt-5.1 medium · 98% left",
            );
            let before = pty.parser().lock().unwrap().screen().contents();
            let cursor = pty.parser().lock().unwrap().screen().cursor_position();
            let text = frame_text(&startup_frame(&mut fixture.quiet.app, 140, 40));
            assert!(
                text.lines().next().unwrap().contains("Jev 사용 (Codex)"),
                "{text}"
            );
            assert!(text.contains(label), "{stage:?}: {text}");
            assert!(text.contains("› native draft"), "{text}");
            let parser = fixture.quiet.app.ptys[&id].parser().lock().unwrap();
            assert_eq!(parser.screen().contents(), before);
            assert_eq!(parser.screen().cursor_position(), cursor);
        }
        fixture.quiet.app.toggle_observer();
        let text = frame_text(&startup_frame(&mut fixture.quiet.app, 140, 40));
        assert!(text.contains("TRACE"), "{text}");
        assert!(text.contains("READ ONLY"), "{text}");
        assert!(
            text.lines().next().unwrap().contains("Jev 사용 (Codex)"),
            "{text}"
        );
        assert!(fixture.quiet.app.observe_bounds.is_some());
        assert!(!text.contains("MindPlayer 시작"));
    }

    fn routing_status_fixture(stage: RoutingStage) -> RoutingStatus {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 2, "generation": "ui-test", "revision": 1,
            "stage": stage, "threadId": "main-thread", "requestId": 1,
            "clearAncestors": [],
            "turnId": null, "model": "gpt-5.5", "effort": "high",
            "observedModel": null, "reason": null, "serverModelReported": false,
        }))
        .unwrap()
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
    fn routing_model_recognizer_accepts_only_native_footer_grammar() {
        for effort in [
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ] {
            assert!(native_model_columns(&format!(
                "  gpt-5.5 {effort} · 98% context left · weekly 85%"
            ))
            .is_some());
        }
        for line in [
            "GPT-5.5 · 98% left · ~/work",
            "gpt-5.5 high | Context 98% | weekly 85%",
        ] {
            assert!(native_model_columns(line).is_some(), "{line}");
        }
        for line in [
            "› gpt-5.5 high · 98% left",
            "MODEL gpt-5.5 high · 98% left",
            "gpt-5.5 explained this · 98% left",
            "gpt-5.5 high · finished",
            "gpt-example high · 98% left",
            "gpt-5.5 high · 98% of the tests",
        ] {
            assert!(native_model_columns(line).is_none(), "{line}");
        }
    }

    #[test]
    fn routing_overlay_preserves_native_model_until_the_first_selection() {
        let mut parser = vt100::Parser::new(10, 100, 100);
        paint_native_footer(
            &mut parser,
            "gpt-5.1 medium · 98% context left · weekly 85%",
        );
        let status = routing_status_fixture(RoutingStage::Ready);
        let Some(RoutingOverlay::ModelRow { label, .. }) =
            routing_overlay(parser.render_screen(), &status, false)
        else {
            panic!("missing ready footer")
        };
        assert!(label.contains("gpt-5.1 medium"));
        assert!(label.contains("입력 대기"));
        assert!(!label.contains("gpt-5.5"));
    }

    #[test]
    fn routing_overlay_prefers_compact_selected_model_with_both_usage_values() {
        let mut parser = vt100::Parser::new(10, 70, 100);
        paint_native_footer(
            &mut parser,
            "gpt-5.1 medium · 98% context left · weekly 85%",
        );
        let status = routing_status_fixture(RoutingStage::Running);
        let Some(RoutingOverlay::ModelRow { label, suffix, .. }) =
            routing_overlay(parser.render_screen(), &status, false)
        else {
            panic!("missing model overlay")
        };
        assert_eq!(label, "실행 중 · 요청 gpt-5.5 high");
        assert!(
            suffix.is_some(),
            "compact status should retain context and weekly values"
        );
    }

    #[test]
    fn routing_overlay_keeps_the_request_and_optional_server_report_separate() {
        let mut status = routing_status_fixture(RoutingStage::Running);
        let running = routing_labels(&status).join(" / ");
        assert!(running.contains("실행 중"), "{running}");
        assert!(!running.contains("미확인") && !running.contains("서버 변경"));
        status.server_model_reported = true;
        assert!(
            !routing_labels(&status).join(" ").contains("서버 변경"),
            "a flag alone is insufficient"
        );
        status.observed_model = Some("gpt-5.6".into());
        let reported = &routing_labels(&status)[0];
        assert!(
            reported.contains("요청 gpt-5.5 high") && reported.contains("서버 변경 gpt-5.6"),
            "{reported}"
        );
        assert!(!reported.contains("실행 확인") && !reported.contains("실제 실행"));
        status.stage = RoutingStage::Completed;
        status.server_model_reported = false;
        assert!(routing_labels(&status)
            .iter()
            .all(|label| label.contains("완료")
                && !label.contains("서버 변경")
                && !label.contains("미확인")));
    }

    #[test]
    fn routing_lifecycle_labels_do_not_wait_for_a_model_reroute_report() {
        for (stage, expected) in [
            (RoutingStage::Running, "실행 중"),
            (RoutingStage::Completed, "완료"),
            (RoutingStage::Requested, "요청 중"),
            (RoutingStage::Accepted, "요청 수락"),
            (RoutingStage::Judging, "Jev 판단 중"),
        ] {
            let status = routing_status_fixture(stage);
            for label in routing_labels(&status) {
                assert!(label.contains(expected), "{stage:?}: {label}");
                assert!(!label.contains("미확인"), "{stage:?}: {label}");
            }
        }
    }

    #[test]
    fn routing_ack_only_label_is_distinct_from_running() {
        let accepted = serde_json::from_str("\"accepted\"")
            .expect("a start ACK must render as accepted before turn/started");
        let labels = routing_labels(&routing_status_fixture(accepted));
        assert!(labels.iter().all(|label| label.contains("요청 수락")));
        assert!(labels
            .iter()
            .all(|label| !label.contains("실행 중") && !label.contains("미확인")));
        assert_ne!(
            labels,
            routing_labels(&routing_status_fixture(RoutingStage::Running))
        );
    }

    #[test]
    fn routing_requested_model_and_effort_are_explicit_with_optional_jev_selection() {
        for stage in [
            RoutingStage::Requested,
            RoutingStage::Running,
            RoutingStage::Completed,
        ] {
            let mut status = routing_status_fixture(stage);
            status.reason = Some("catalog-guided-jev".into());
            let detailed = &routing_labels(&status)[0];
            assert!(detailed.contains("Jev 선택"), "{detailed}");
            assert!(detailed.contains("요청 gpt-5.5 high"), "{detailed}");
            let compact =
                fit_routing_label(&status, 30).expect("selected request should fit a small pane");
            assert!(compact.contains("요청 gpt-5.5 high"), "{compact}");
            assert!(!compact.contains("미확인"), "{compact}");
        }
    }

    #[test]
    fn routing_reported_model_is_separate_from_requested_effort_and_lifecycle() {
        for (stage, lifecycle) in [
            (RoutingStage::Running, "실행 중"),
            (RoutingStage::Completed, "완료"),
        ] {
            let mut status = routing_status_fixture(stage);
            // Use the serialized contract so this test also detects the old field name.
            let mut value = serde_json::to_value(&status).unwrap();
            value["serverModelReported"] = serde_json::json!(true);
            value["observedModel"] = serde_json::json!("gpt-5.6");
            value["turnId"] = serde_json::json!("turn-1");
            status = serde_json::from_value(value)
                .expect("server reports must not claim execution verification");
            let label = &routing_labels(&status)[0];
            assert!(label.contains(lifecycle), "{label}");
            assert!(label.contains("요청 gpt-5.5 high"), "{label}");
            assert!(label.contains("서버 변경 gpt-5.6"), "{label}");
            assert!(
                !label.contains("gpt-5.6 high"),
                "effort belongs to the request: {label}"
            );
            assert!(
                !label.contains("실행 확인") && !label.contains("실제 실행"),
                "{label}"
            );
        }
    }

    #[test]
    fn routing_lifecycle_fits_narrow_views_and_preserves_the_native_screen() {
        for (stage, lifecycle) in [
            (RoutingStage::Requested, "요청 중"),
            (RoutingStage::Running, "실행 중"),
            (RoutingStage::Completed, "완료"),
        ] {
            let mut status = routing_status_fixture(stage);
            status.reason = Some("catalog-guided-jev".into());
            for width in [120, 70, 48, 32, 20, 12] {
                let mut parser = vt100::Parser::new(10, width, 100);
                paint_native_footer(&mut parser, "gpt-5.1 medium · 98% left");
                let before = parser.screen().contents_formatted();
                let cursor = parser.screen().cursor_position();
                let overlay = routing_overlay(parser.render_screen(), &status, false)
                    .expect("the lifecycle should fit even when the model does not");
                let (label, available) = match overlay {
                    RoutingOverlay::ModelRow { label, column, .. } => (label, width - column),
                    RoutingOverlay::Border(label) => (label, width),
                };
                assert!(label.contains(lifecycle), "width={width}: {label}");
                assert!(!label.contains("미확인"), "width={width}: {label}");
                assert!(Line::from(label.as_str()).width() <= available as usize);
                if width >= 32 {
                    assert!(
                        label.contains("요청 gpt-5.5 high"),
                        "width={width}: {label}"
                    );
                }
                assert_eq!(parser.screen().contents_formatted(), before);
                assert_eq!(parser.screen().cursor_position(), cursor);
            }
        }
    }

    #[test]
    fn routing_failures_and_policy_exceptions_have_distinct_korean_labels() {
        let mut status = routing_status_fixture(RoutingStage::Failed);
        for (reason, expected) in [
            (
                "submission-uncertain-no-replay",
                "송신 결과 미확인 · 재전송 안 함",
            ),
            ("execution-failed", "실행 실패"),
            ("selector-error", "미전송"),
            ("selector-timeout", "미전송"),
            ("jev-tls-error", "Jev 인증서 오류 · 미전송"),
            ("jev-invalid-response", "Jev 응답 오류 · 미전송"),
        ] {
            status.reason = Some(reason.into());
            assert_eq!(routing_labels(&status), [expected]);
        }
        for stage in [
            RoutingStage::Requested,
            RoutingStage::Running,
            RoutingStage::Completed,
        ] {
            status.stage = stage;
            status.reason = Some("external-transfer-policy-unset".into());
            assert!(routing_labels(&status)
                .iter()
                .all(|label| label.contains("Jev 전송 금지") && label.contains("네이티브")));
            status.reason = Some("context-incomplete".into());
            assert!(routing_labels(&status)
                .iter()
                .all(|label| label.contains("유지 · 맥락 부족")));
        }
    }

    #[test]
    fn routing_overlay_yields_to_input_selection_scrollback_menus_and_approvals() {
        let status = routing_status_fixture(RoutingStage::Requested);
        let mut parser = vt100::Parser::new(12, 100, 100);
        paint_native_footer(
            &mut parser,
            "gpt-5.1 medium · 98% context left · weekly 85%",
        );
        assert!(routing_overlay(parser.render_screen(), &status, true).is_none());
        parser.process(b"\x1b[12;1H");
        assert!(
            routing_overlay(parser.render_screen(), &status, false).is_none(),
            "cursor row is never a status target"
        );
        for dialog in [
            "\x1b[8;1H/help  help\x1b[9;1H/model  choose\x1b[10;1Hesc to cancel · ↑↓ to navigate · ↵ to select",
            "\x1b[9;1HEnter to select · arrows to navigate · Esc to cancel",
            "\x1b[9;1HDo you want to proceed?",
        ] {
            paint_native_footer(&mut parser, "gpt-5.1 medium · 98% context left · weekly 85%");
            parser.process(dialog.as_bytes());
            assert!(routing_overlay(parser.render_screen(), &status, false).is_none(), "dialog must own the screen: {dialog}");
        }
        parser.process("\x1b[2J\x1b[Hhistory\r\n".as_bytes());
        for _ in 0..30 {
            parser.process(b"history\r\n");
        }
        paint_native_footer(
            &mut parser,
            "gpt-5.1 medium · 98% context left · weekly 85%",
        );
        parser.set_scrollback(1);
        assert!(parser.render_screen().scrollback() > 0);
        assert!(routing_overlay(parser.render_screen(), &status, false).is_none());
    }

    #[test]
    fn routing_overlay_requires_a_unique_bottom_model_row_and_skips_short_transients() {
        let status = routing_status_fixture(RoutingStage::Judging);
        let mut parser = vt100::Parser::new(20, 100, 100);
        parser.process("\x1b[2;1Hgpt-5.1 high · 98% left\x1b[18;1H> draft".as_bytes());
        assert_eq!(
            routing_overlay(parser.render_screen(), &status, false),
            Some(RoutingOverlay::Border("Jev 판단 중".into()))
        );
        paint_native_footer(&mut parser, "gpt-5.1 high · 98% left");
        parser.process("\x1b[15;1Hgpt-5.2 high · 98% left\x1b[18;1H".as_bytes());
        assert!(matches!(
            routing_overlay(parser.render_screen(), &status, false),
            Some(RoutingOverlay::Border(_))
        ));
        parser.set_size(3, 100);
        assert!(routing_overlay(parser.render_screen(), &status, false).is_none());
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
        app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
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
        assert_eq!(usage_cell(&session(Agent::Cursor, 6_200_000)), "~6.2M tok");
        assert_eq!(usage_cell(&session(Agent::Claude, 6_200_000)), "6.2M tok");
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
        app.startup_routing = StartupRouting::confirmed(RoutingMode::Native);
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

    struct RoutingPtyFixture {
        quiet: QuietPtyFixture,
        root: PathBuf,
        path: PathBuf,
    }

    impl RoutingPtyFixture {
        fn new(stage: RoutingStage, rows: u16, cols: u16) -> Self {
            let mut quiet = QuietPtyFixture::new(rows, cols);
            let root = std::env::temp_dir().join(format!(
                "mp-ui-routing-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let channel = crate::local_router::RoutingStatusChannel::create_at(&root).unwrap();
            let path = channel.path().to_path_buf();
            let mut status = routing_status_fixture(stage);
            status.generation = channel.snapshot().unwrap().generation;
            std::fs::write(&path, serde_json::to_vec(&status).unwrap()).unwrap();
            let id = quiet.app.panes[0].clone();
            quiet
                .app
                .pane_accounts
                .insert(id.clone(), (Agent::Codex, "routing-fixture".into()));
            let pty = quiet.app.ptys.get_mut(&id).unwrap();
            pty.set_routing_status_for_test(channel);
            assert!(pty.take_dirty());
            Self { quiet, root, path }
        }

        fn id(&self) -> String {
            self.quiet.app.panes[0].clone()
        }
    }

    impl Drop for RoutingPtyFixture {
        fn drop(&mut self) {
            self.quiet.app.ptys.clear();
            let _ = std::fs::remove_dir_all(&self.root);
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
    fn routing_completed_is_green_in_the_model_row_and_border_fallback() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Completed, 10, 118);
        let id = fixture.id();
        let parser = fixture.quiet.app.ptys[&id].parser().clone();
        let mut status = fixture.quiet.app.ptys[&id].routing_status().unwrap();
        status.turn_id = Some("completed-turn".into());
        status.revision += 1;
        std::fs::write(&fixture.path, serde_json::to_vec(&status).unwrap()).unwrap();
        assert!(fixture.quiet.app.ptys[&id].take_dirty());
        for model_row in [true, false] {
            for (width, focused, zoomed) in
                [(120, true, false), (48, false, false), (32, true, true)]
            {
                let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
                fixture.quiet.app.zoomed = zoomed;
                fixture
                    .quiet
                    .app
                    .ptys
                    .get_mut(&id)
                    .unwrap()
                    .resize(10, width - 2);
                if model_row {
                    paint_native_footer(&mut parser.lock().unwrap(), "gpt-5.1 medium · 98% left");
                } else {
                    parser
                        .lock()
                        .unwrap()
                        .process(b"\x1b[2J\x1b[Hnative response\x1b[8;1H> draft");
                }
                assert_eq!(
                    matches!(
                        routing_overlay(parser.lock().unwrap().render_screen(), &status, false),
                        Some(RoutingOverlay::ModelRow { .. })
                    ),
                    model_row,
                    "exercise the actual model-row and border branches",
                );
                terminal
                    .draw(|frame| {
                        render_pane(
                            frame,
                            &mut fixture.quiet.app,
                            &id,
                            0,
                            1,
                            Rect::new(0, 0, width, 12),
                            focused,
                            false,
                            None,
                        )
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let complete = buffer
                    .content()
                    .iter()
                    .find(|cell| cell.symbol() == "완")
                    .expect("the completion label must be visible");
                assert_eq!(
                    complete.fg, COMPLETED_BORDER,
                    "model_row={model_row}, width={width}"
                );
                assert_eq!(
                    buffer[(0, 4)].fg,
                    COMPLETED_BORDER,
                    "completed pane border must match"
                );
            }
        }
    }

    #[test]
    fn routing_completion_retires_on_successful_input_and_ignores_late_old_revisions() {
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Completed, 10, 118);
        let id = fixture.id();
        let mut status = fixture.quiet.app.ptys[&id].routing_status().unwrap();
        status.turn_id = Some("first-turn".into());
        for (request, stage, action) in [
            (1, RoutingStage::Completed, Some(b"\r".as_slice())),
            (1, RoutingStage::Completed, None), // late ACK/model report revision
            (2, RoutingStage::Judging, None),
            (2, RoutingStage::Accepted, None),
            (2, RoutingStage::Running, None),
            (2, RoutingStage::Completed, Some(b"\x1b".as_slice())),
            (2, RoutingStage::Completed, None), // delayed completion after Escape
            (3, RoutingStage::Failed, None),
            (4, RoutingStage::Interrupted, None),
        ] {
            status.revision += 1;
            status.request_id = serde_json::json!(request);
            status.stage = stage;
            std::fs::write(&fixture.path, serde_json::to_vec(&status).unwrap()).unwrap();
            assert!(fixture.quiet.app.ptys[&id].take_dirty());
            if let Some(bytes) = action {
                assert_eq!(
                    fixture.quiet.app.session_status(&id),
                    SessionStatus::Completed
                );
                fixture.quiet.app.send_to_pty(bytes);
                if bytes == b"\x1b" {
                    assert_eq!(
                        fixture.quiet.app.session_status(&id),
                        SessionStatus::Completed
                    );
                    fixture
                        .quiet
                        .app
                        .ptys
                        .get_mut(&id)
                        .unwrap()
                        .expire_terminal_reply_for_test(false);
                    fixture.quiet.app.flush_terminal_reply_guards();
                }
            }
            assert_ne!(
                fixture.quiet.app.session_status(&id),
                SessionStatus::Completed,
                "request={request}, stage={stage:?}"
            );
            if request <= 2 && stage == RoutingStage::Completed {
                assert!(
                    fixture.quiet.app.ptys[&id].routing_status().is_none(),
                    "old request must stay retired"
                );
            } else if matches!(
                stage,
                RoutingStage::Judging | RoutingStage::Accepted | RoutingStage::Running
            ) {
                assert_eq!(
                    fixture.quiet.app.session_status(&id),
                    SessionStatus::Working
                );
            }
        }
    }

    #[test]
    fn routing_status_polls_later_panes_even_when_the_first_is_dirty() {
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Completed, 10, 118);
        let first = fixture.id();
        let second = fixture.quiet.app.panes[1].clone();
        let channel =
            crate::local_router::RoutingStatusChannel::create_at(&fixture.root.join("second"))
                .unwrap();
        let path = channel.path().to_path_buf();
        let mut later = channel.snapshot().unwrap();
        fixture
            .quiet
            .app
            .ptys
            .get_mut(&second)
            .unwrap()
            .set_routing_status_for_test(channel);
        let mut earlier = fixture.quiet.app.ptys[&first].routing_status().unwrap();
        earlier.revision += 1;
        std::fs::write(&fixture.path, serde_json::to_vec(&earlier).unwrap()).unwrap();
        later.revision += 1;
        later.stage = RoutingStage::Completed;
        later.thread_id = Some("later-thread".into());
        later.request_id = serde_json::json!(1);
        later.turn_id = Some("later-turn".into());
        std::fs::write(&path, serde_json::to_vec(&later).unwrap()).unwrap();
        assert!(fixture.quiet.app.pty_dirty());
        assert_eq!(
            fixture.quiet.app.ptys[&second].routing_status().unwrap(),
            later
        );
    }

    #[test]
    fn routing_status_retires_for_control_c_after_a_pending_reply_prefix() {
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Completed, 10, 118);
        let id = fixture.id();
        fixture.quiet.app.send_to_pty(b"[");
        assert!(fixture.quiet.app.ptys[&id].routing_status().is_some());
        fixture.quiet.app.send_to_pty(b"\x03");
        assert!(fixture.quiet.app.ptys[&id].routing_status().is_none());
    }

    #[test]
    fn routing_status_retires_when_expired_escape_precedes_another_key() {
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Completed, 10, 118);
        let id = fixture.id();
        fixture.quiet.app.send_to_pty(b"\x1b");
        fixture
            .quiet
            .app
            .ptys
            .get_mut(&id)
            .unwrap()
            .expire_terminal_reply_for_test(false);
        fixture.quiet.app.send_to_pty(b"x");
        assert!(fixture.quiet.app.ptys[&id].routing_status().is_none());
    }

    #[test]
    fn routing_status_retires_escape_after_prefix_only_after_successful_enqueue() {
        for (prefix, closed) in [
            b"\x1b".as_slice(),
            b"[",
            b";",
            b"\x1b[",
            b"[1;",
            b"\x1b[?1;",
        ]
        .into_iter()
        .flat_map(|prefix| [false, true].map(|closed| (prefix, closed)))
        {
            let mut fixture = RoutingPtyFixture::new(RoutingStage::Completed, 10, 118);
            let id = fixture.id();
            let mut previous = fixture.quiet.app.ptys[&id].routing_status().unwrap();
            if closed {
                fixture
                    .quiet
                    .app
                    .ptys
                    .get_mut(&id)
                    .unwrap()
                    .expire_terminal_reply_for_test(true);
            }
            fixture.quiet.app.send_to_pty(prefix);
            assert!(fixture.quiet.app.ptys[&id].routing_status().is_some());
            fixture.quiet.app.send_to_pty(b"\x1b");
            fixture
                .quiet
                .app
                .ptys
                .get_mut(&id)
                .unwrap()
                .expire_terminal_reply_for_test(false);
            fixture.quiet.app.flush_terminal_reply_guards();
            assert_eq!(
                fixture.quiet.app.ptys[&id].routing_status().is_some(),
                closed
            );
            previous.revision += 1;
            std::fs::write(&fixture.path, serde_json::to_vec(&previous).unwrap()).unwrap();
            assert!(fixture.quiet.app.ptys[&id].take_dirty());
            assert_eq!(
                fixture.quiet.app.ptys[&id].routing_status().is_some(),
                closed,
                "late old revisions must not undo the enqueue outcome"
            );
        }
    }

    #[test]
    fn routing_status_preserves_completion_for_alt_and_arrow_keys() {
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Completed, 10, 118);
        let id = fixture.id();
        for bytes in [b"\x1b".as_slice(), b"x", b"\x1b[A", b"\x1b[1;3D"] {
            fixture.quiet.app.send_to_pty(bytes);
            assert!(fixture.quiet.app.ptys[&id].routing_status().is_some());
        }
    }

    #[test]
    fn routing_render_followup_resets_the_previous_turn_model_and_report() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Completed, 10, 118);
        let id = fixture.id();
        let parser = fixture.quiet.app.ptys[&id].parser().clone();
        paint_native_footer(&mut parser.lock().unwrap(), "gpt-5.1 medium · 98% left");
        let before = parser.lock().unwrap().screen().contents_formatted();
        let cursor = parser.lock().unwrap().screen().cursor_position();
        let output_seq = fixture.quiet.app.ptys[&id].output_seq();
        let mut value =
            serde_json::to_value(fixture.quiet.app.ptys[&id].routing_status().unwrap()).unwrap();
        value["turnId"] = serde_json::json!("turn-1");
        value["observedModel"] = serde_json::json!("gpt-5.6");
        value["serverModelReported"] = serde_json::json!(true);
        let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
        for (stage, lifecycle) in [
            ("completed", "완료"),
            ("judging", "Jev 판단 중"),
            ("requested", "요청 중"),
            ("accepted", "요청 수락"),
            ("running", "실행 중"),
            ("completed", "완료"),
        ] {
            value["revision"] = serde_json::json!(value["revision"].as_u64().unwrap() + 1);
            value["stage"] = serde_json::json!(stage);
            if stage == "judging" {
                value["requestId"] = serde_json::json!(2);
                for field in ["turnId", "model", "effort", "observedModel", "reason"] {
                    value[field] = serde_json::Value::Null;
                }
                value["serverModelReported"] = serde_json::json!(false);
            } else if stage == "requested" {
                value["model"] = serde_json::json!("gpt-5.4-mini");
                value["effort"] = serde_json::json!("low");
                value["reason"] = serde_json::json!("catalog-guided-jev");
            } else if stage == "running" {
                value["turnId"] = serde_json::json!("turn-2");
            }
            std::fs::write(&fixture.path, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(
                fixture.quiet.app.ptys[&id].take_dirty(),
                "metadata must redraw {stage}"
            );
            terminal
                .draw(|frame| {
                    render_pane(
                        frame,
                        &mut fixture.quiet.app,
                        &id,
                        0,
                        1,
                        Rect::new(0, 0, 120, 12),
                        true,
                        false,
                        None,
                    )
                })
                .unwrap();
            let footer = rendered_row(terminal.backend().buffer(), 10);
            assert!(footer.contains(lifecycle), "{stage}: {footer}");
            assert!(!footer.contains("미확인"), "{stage}: {footer}");
            if value["requestId"] == 1 {
                assert!(footer.contains("요청 gpt-5.5 high"), "{footer}");
                assert!(footer.contains("서버 변경 gpt-5.6"), "{footer}");
            } else {
                assert!(
                    !footer.contains("gpt-5.5") && !footer.contains("gpt-5.6"),
                    "{footer}"
                );
                if stage != "judging" {
                    assert!(footer.contains("요청 gpt-5.4-mini low"), "{footer}");
                }
            }
            assert_eq!(parser.lock().unwrap().screen().contents_formatted(), before);
            assert_eq!(parser.lock().unwrap().screen().cursor_position(), cursor);
            assert_eq!(fixture.quiet.app.ptys[&id].output_seq(), output_seq);
            assert_eq!(fixture.quiet.app.pane_sizes[&id], (10, 118));
        }
    }

    #[test]
    fn routing_render_replaces_old_model_preserving_native_usage_styles_input_cursor_and_screen() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Requested, 10, 118);
        let id = fixture.id();
        let parser = fixture.quiet.app.ptys[&id].parser().clone();
        paint_native_footer(
            &mut parser.lock().unwrap(),
            "gpt-5.1 medium · \x1b[32m98% context left\x1b[0m · \x1b[35mweekly 85%\x1b[0m",
        );
        let before = parser.lock().unwrap().screen().contents_formatted();
        let cursor = parser.lock().unwrap().screen().cursor_position();
        let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
        terminal
            .draw(|frame| {
                render_pane(
                    frame,
                    &mut fixture.quiet.app,
                    &id,
                    0,
                    1,
                    Rect::new(0, 0, 120, 12),
                    true,
                    false,
                    None,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let footer = rendered_row(buffer, 10);
        assert!(
            footer.contains("요청 gpt-5.5 high") && footer.contains("요청 중"),
            "{footer}"
        );
        assert!(!footer.contains("gpt-5.1"));
        assert!(footer.contains("98% context left") && footer.contains("weekly 85%"));
        assert!(rendered_row(buffer, cursor.0 + 1).contains("native draft"));
        assert!(buffer
            .content()
            .iter()
            .any(|cell| cell.symbol() == "9" && cell.fg == Color::Indexed(2)));
        assert!(buffer
            .content()
            .iter()
            .any(|cell| cell.symbol() == "w" && cell.fg == Color::Indexed(5)));
        assert_eq!(parser.lock().unwrap().screen().contents_formatted(), before);
        assert_eq!(parser.lock().unwrap().screen().cursor_position(), cursor);
        let painted_cursor = terminal.get_cursor_position().unwrap();
        assert_eq!(
            (painted_cursor.x, painted_cursor.y),
            (cursor.1 + 1, cursor.0 + 1)
        );
        assert_eq!(fixture.quiet.app.pane_sizes[&id], (10, 118));
    }

    #[test]
    fn routing_render_resizes_without_repeating_the_old_model_or_mutating_native_rows() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Running, 10, 118);
        let id = fixture.id();
        let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
        for width in [120, 78, 50, 34, 120] {
            terminal.backend_mut().resize(width, 12);
            let pty = fixture.quiet.app.ptys.get_mut(&id).unwrap();
            pty.resize(10, width - 2);
            let parser = pty.parser().clone();
            paint_native_footer(&mut parser.lock().unwrap(), "gpt-5.1 medium · 98% left");
            let before = parser.lock().unwrap().screen().contents_formatted();
            terminal
                .draw(|frame| {
                    render_pane(
                        frame,
                        &mut fixture.quiet.app,
                        &id,
                        0,
                        1,
                        Rect::new(0, 0, width, 12),
                        true,
                        false,
                        None,
                    )
                })
                .unwrap();
            let footer = rendered_row(terminal.backend().buffer(), 10);
            assert!(footer.contains("gpt-5.5 high"), "width={width}: {footer}");
            assert!(!footer.contains("gpt-5.1"), "width={width}: {footer}");
            assert!(
                footer.contains("실행 중") && footer.contains("요청 gpt-5.5 high"),
                "width={width}: {footer}"
            );
            assert!(!footer.contains("미확인"), "width={width}: {footer}");
            assert_eq!(parser.lock().unwrap().screen().contents_formatted(), before);
            assert_eq!(fixture.quiet.app.pane_sizes[&id], (10, width - 2));
        }
    }

    #[test]
    fn routing_fallback_uses_only_the_existing_bottom_border() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Judging, 10, 78);
        let id = fixture.id();
        let parser = fixture.quiet.app.ptys[&id].parser().clone();
        parser
            .lock()
            .unwrap()
            .process(b"\x1b[Hnative content\x1b[8;1H> native draft");
        let before = parser.lock().unwrap().screen().contents_formatted();
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| {
                render_pane(
                    frame,
                    &mut fixture.quiet.app,
                    &id,
                    0,
                    1,
                    Rect::new(0, 0, 80, 12),
                    true,
                    false,
                    None,
                )
            })
            .unwrap();
        assert!(rendered_row(terminal.backend().buffer(), 11).contains("Jev 판단 중"));
        assert!(!rendered_row(terminal.backend().buffer(), 10).contains("Jev"));
        assert_eq!(parser.lock().unwrap().screen().contents_formatted(), before);
        assert_eq!(fixture.quiet.app.pane_sizes[&id], (10, 78));
    }

    #[test]
    fn routing_channel_polls_idle_updates_and_moves_with_the_pty_on_adoption() {
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Judging, 10, 80);
        let id = fixture.id();
        let pty = &fixture.quiet.app.ptys[&id];
        pty.take_dirty();
        let before_seq = pty.output_seq();
        let mut owned = pty.routing_status().unwrap();
        owned.revision += 1;
        owned.stage = RoutingStage::Requested;
        std::fs::write(&fixture.path, serde_json::to_vec(&owned).unwrap()).unwrap();
        assert!(
            pty.take_dirty(),
            "metadata alone must trigger redraw on an idle pane"
        );
        assert_eq!(
            pty.output_seq(),
            before_seq,
            "metadata must not become terminal output or lifecycle activity"
        );
        let mut cloned = pty.routing_status().unwrap();
        cloned.model = Some("mutated-copy".into());
        assert_ne!(
            pty.routing_status().unwrap(),
            cloned,
            "accessors return owned copies"
        );
        let moved = fixture.quiet.app.ptys.remove(&id).unwrap();
        fixture
            .quiet
            .app
            .ptys
            .insert("adopted-session".into(), moved);
        let adopted = fixture.quiet.app.ptys.remove("adopted-session").unwrap();
        assert_eq!(adopted.routing_status(), Some(owned));
        let owned_dir = fixture.path.parent().unwrap().to_path_buf();
        assert!(fixture.path.exists());
        drop(adopted);
        assert!(
            !owned_dir.exists(),
            "drop must terminate before cleaning just this channel"
        );
    }

    #[test]
    fn routing_status_keeps_the_actual_four_plus_three_pty_geometry_and_full_width_footer() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut fixture = RoutingPtyFixture::new(RoutingStage::Judging, 8, 20);
        for index in 2..7 {
            let id = format!("ui-routing-grid-{index}");
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
            fixture.quiet.app.panes.push(id.clone());
            fixture.quiet.app.ptys.insert(id, pty);
        }
        fixture.quiet.app.layout = PaneLayout::Horizontal;
        let mut terminal = Terminal::new(TestBackend::new(196, 55)).unwrap();
        terminal
            .draw(|frame| render(frame, &mut fixture.quiet.app))
            .unwrap();
        let bounds = fixture.quiet.app.pane_bounds.clone();
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
            let pty = fixture.quiet.app.ptys.get_mut(id).unwrap();
            assert_eq!((pty.rows, pty.cols), (rows, cols));
            assert!(x + cols < 196);
            paint_native_footer(
                &mut pty.parser().lock().unwrap(),
                "gpt-5.1 medium · 98% left",
            );
        }
        terminal
            .draw(|frame| render(frame, &mut fixture.quiet.app))
            .unwrap();
        assert_eq!(fixture.quiet.app.pane_bounds, bounds);
        assert_eq!(
            rendered_row(terminal.backend().buffer(), 54),
            full_width_footer
        );
        for (id, &(_, _, rows, cols)) in &bounds {
            let pty = &fixture.quiet.app.ptys[id];
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
        let lines = help_lines(true);
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
        let keys: Vec<String> = help_lines(true)
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

#[cfg(test)]
mod short_wait_tests {
    use super::short_wait;
    use std::time::Duration;

    /// Just under an hour rounds up to the hour, never to "60m".
    #[test]
    fn a_wait_is_named_in_the_largest_unit_that_fits() {
        assert_eq!(short_wait(Duration::from_secs(45)), "45s");
        assert_eq!(short_wait(Duration::from_secs(239)), "4m");
        assert_eq!(short_wait(Duration::from_secs(3541)), "1h");
        assert_eq!(short_wait(Duration::from_secs(5400)), "2h");
    }
}
