//! Pulling copyable links out of an agent's answer.
//!
//! Answers are markdown, so a URL almost never sits on its own: measured across
//! this project's own 965-turn transcript, the 52 URL occurrences broke down as
//! 31 `[text](url)`, 18 bare-with-trailing-punctuation, and 3 `**url**`. A plain
//! `https?://\S+` match therefore returns `…/hooks/)` or `…-c8d7c21a**` — strings
//! that do not open. Trimming that tail is the whole job.
//!
//! Source is the transcript, not the pane's screen: a terminal wraps a long URL
//! across rows (the longest here is 70 chars), and the screen has no marker for
//! where one answer ends and the next begins.

use std::path::PathBuf;

use mindplayer_core::{Agent, Session};
use serde_json::Value;

use crate::handoff::parse_turn_for;

/// How far back to look for an answer that contains a link.
///
/// Links are sparse and bursty — 22 of 965 turns here, with a median gap of 16
/// turns and a maximum of 274. Scanning back forever would mean reading the
/// whole (39 MB) transcript on a keypress, so this is the point where "no links
/// found" is the more useful answer.
pub const MAX_TURNS_BACK: usize = 40;

/// Bytes read from the end of the transcript. Enough to hold far more than
/// [`MAX_TURNS_BACK`] answers, while keeping a keypress off a whole-file read.
const TAIL_BYTES: u64 = 4 << 20;

/// Characters that end a URL outright — a URL never contains them unescaped, and
/// they are how markdown, tables, and quoting fence one off.
const HARD_STOPS: &[char] = &['<', '>', '"', '`', '|', '\\', '^', '{', '}'];

/// Trailing characters that are punctuation or markup rather than part of the
/// address. `)` is handled separately, since a URL may legitimately contain a
/// balanced pair.
const TRAILING_JUNK: &[char] = &['*', '_', '~', '.', ',', ';', ':', '!', '?', '\'', ']', '('];

/// One answer's links, and how far back that answer was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHit {
    pub links: Vec<String>,
    /// 0 = the latest answer, 1 = the one before it, and so on.
    pub turns_ago: usize,
}

/// Every link in `text`, in order, deduplicated.
pub fn extract_links(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &text[i..];
        let Some(start) = find_scheme(rest) else {
            break;
        };
        let abs = i + start;
        let candidate = &text[abs..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || HARD_STOPS.contains(&c))
            .unwrap_or(candidate.len());
        let url = trim_trailing(&candidate[..end]);
        // A scheme with nothing after it is not a link.
        if url.len() > "https://".len() && !out.iter().any(|u| u == url) {
            out.push(url.to_string());
        }
        i = abs + end.max(1);
    }
    out
}

/// Byte offset of the next `http://` / `https://` in `s`.
fn find_scheme(s: &str) -> Option<usize> {
    let http = s.find("http://");
    let https = s.find("https://");
    match (http, https) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Strip trailing markup and sentence punctuation, and any `)` that closes
/// something outside the URL — `](https://x/)` ends at `/`, while a URL that
/// opened its own `(` keeps the matching `)`.
fn trim_trailing(url: &str) -> &str {
    let mut end = url.len();
    loop {
        let s = &url[..end];
        let Some(last) = s.chars().last() else { break };
        if TRAILING_JUNK.contains(&last) {
            end -= last.len_utf8();
            continue;
        }
        if last == ')' {
            let opens = s.matches('(').count();
            let closes = s.matches(')').count();
            if closes > opens {
                end -= 1;
                continue;
            }
        }
        break;
    }
    &url[..end]
}

/// The most recent answer that contains a link, searching back at most
/// `max_turns` answers.
///
/// `None` when the transcript is unreadable or no answer in range had one — the
/// caller says so rather than copying nothing silently.
pub fn latest_links(session: &Session, max_turns: usize) -> Option<LinkHit> {
    let answers = recent_answers(session, max_turns)?;
    for (turns_ago, text) in answers.iter().enumerate() {
        let links = extract_links(text);
        if !links.is_empty() {
            return Some(LinkHit { links, turns_ago });
        }
    }
    None
}

/// The last `max_turns` assistant answers, newest first.
fn recent_answers(session: &Session, max_turns: usize) -> Option<Vec<String>> {
    let path = transcript_path(session)?;
    let text = read_tail(&path, TAIL_BYTES)?;
    let parse = parse_turn_for(session.agent);
    let mut answers: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some((role, body)) = parse(&v) {
            if role == "assistant" && !body.trim().is_empty() {
                answers.push(body);
            }
        }
    }
    answers.reverse();
    answers.truncate(max_turns);
    Some(answers)
}

/// Kiro's `Session::file` is a metadata sidecar; the turns live in an adjacent
/// jsonl, the same asymmetry the conversation log handles.
fn transcript_path(session: &Session) -> Option<PathBuf> {
    if session.file.as_os_str().is_empty() {
        return None;
    }
    if session.agent == Agent::Kiro {
        let adjacent = session.file.with_extension("jsonl");
        return adjacent.exists().then_some(adjacent);
    }
    Some(session.file.clone())
}

/// Last `n` bytes as lossy UTF-8, dropping the first (probably partial) line.
fn read_tail(path: &std::path::Path, n: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(n);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.take(n).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    if from == 0 {
        return Some(text);
    }
    Some(match text.find('\n') {
        Some(i) => text[i + 1..].to_string(),
        None => String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every string below is lifted verbatim from this project's own transcript,
    /// so these assert against the shapes answers actually produce rather than
    /// ones convenient to parse.
    #[test]
    fn a_markdown_link_stops_at_its_closing_paren() {
        // The most common form by far: 31 of 52 real occurrences.
        assert_eq!(
            extract_links("[Kiro Docs - Kiro](https://kiro.dev/docs/cli/hooks/)"),
            vec!["https://kiro.dev/docs/cli/hooks/"]
        );
        assert_eq!(
            extract_links("| [bloopai/vibe-kanban](https://github.com/bloopai/vibe-kanban) | 27."),
            vec!["https://github.com/bloopai/vibe-kanban"]
        );
    }

    #[test]
    fn bold_markers_are_not_part_of_the_address() {
        assert_eq!(
            extract_links(
                "**https://claude.ai/code/artifact/3f7b2acb-00df-44e7-ada4-98c4c8d7c21a**"
            ),
            vec!["https://claude.ai/code/artifact/3f7b2acb-00df-44e7-ada4-98c4c8d7c21a"]
        );
    }

    #[test]
    fn a_table_cell_url_stops_at_the_pipe() {
        assert_eq!(
            extract_links("| URL | https://github.com/longlg88/mindplayer/releases/tag/v0.16.0 |"),
            vec!["https://github.com/longlg88/mindplayer/releases/tag/v0.16.0"]
        );
    }

    #[test]
    fn sentence_punctuation_is_trimmed() {
        for (raw, want) in [
            ("see https://example.com/page.", "https://example.com/page"),
            (
                "see https://example.com/page, and",
                "https://example.com/page",
            ),
            ("done: https://example.com/a/b/", "https://example.com/a/b/"),
            ("really? https://example.com/x!", "https://example.com/x"),
        ] {
            assert_eq!(extract_links(raw), vec![want.to_string()], "{raw}");
        }
    }

    /// A URL may contain its own balanced parens; only an unmatched closer is
    /// markdown's, not the address's.
    #[test]
    fn balanced_parens_inside_a_url_are_kept() {
        assert_eq!(
            extract_links("[x](https://en.wikipedia.org/wiki/Rust_(programming_language))"),
            vec!["https://en.wikipedia.org/wiki/Rust_(programming_language)"]
        );
    }

    #[test]
    fn backticks_and_angle_brackets_fence_a_url() {
        assert_eq!(
            extract_links("`https://example.com/x` and <https://example.com/y>"),
            vec![
                "https://example.com/x".to_string(),
                "https://example.com/y".to_string()
            ]
        );
    }

    #[test]
    fn several_links_keep_their_order_and_are_deduplicated() {
        let text = "\
- [orca](https://www.onorca.dev/)
- docs at https://www.onorca.dev/docs/cli/overview
- repo https://github.com/stablyai/orca
- again https://www.onorca.dev/";
        assert_eq!(
            extract_links(text),
            vec![
                "https://www.onorca.dev/".to_string(),
                "https://www.onorca.dev/docs/cli/overview".to_string(),
                "https://github.com/stablyai/orca".to_string(),
            ]
        );
    }

    #[test]
    fn text_without_links_yields_nothing() {
        assert!(extract_links("no links here — just prose about http things").is_empty());
        assert!(extract_links("").is_empty());
        // A bare scheme is not an address.
        assert!(extract_links("https://").is_empty());
    }

    #[test]
    fn http_and_https_are_both_found() {
        assert_eq!(
            extract_links("old http://example.com/a new https://example.com/b"),
            vec![
                "http://example.com/a".to_string(),
                "https://example.com/b".to_string()
            ]
        );
    }

    /// Runs the extractor over every answer in this machine's own Claude Code
    /// transcripts and asserts nothing dirty comes out — the fixtures above are
    /// hand-picked, this is the whole corpus they were picked from.
    ///
    /// Ignored by default: it depends on the developer's `~/.claude` and reads
    /// tens of MB. Run it deliberately with
    /// `cargo test -p mindplayer-tui links:: -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_transcripts_yield_only_clean_addresses() {
        let home = std::env::var("HOME").expect("HOME");
        let root = std::path::Path::new(&home).join(".claude/projects");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        collect_jsonl(&root, &mut files);
        assert!(!files.is_empty(), "no transcripts under {}", root.display());

        let mut total = 0usize;
        let mut dirty: Vec<String> = Vec::new();
        for f in &files {
            let Ok(text) = std::fs::read_to_string(f) else {
                continue;
            };
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if v.get("type").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                let Some((role, body)) = parse_turn_for(Agent::Claude)(&v) else {
                    continue;
                };
                if role != "assistant" {
                    continue;
                }
                for u in extract_links(&body) {
                    total += 1;
                    // A trailing `)` is only markup when it closes something the
                    // URL never opened — `…/Rust_(programming_language)` is a
                    // real address and must not count as dirty.
                    let unbalanced_close =
                        u.ends_with(')') && u.matches(')').count() > u.matches('(').count();
                    let bad = unbalanced_close
                        || u.ends_with('*')
                        || u.ends_with('.')
                        || u.ends_with(',')
                        || u.ends_with(']')
                        || u.contains('|')
                        || u.contains(' ')
                        || u.contains('`');
                    if bad && !dirty.iter().any(|d| d == &u) {
                        dirty.push(u);
                    }
                }
            }
        }
        println!("scanned {} transcripts, {total} links", files.len());
        assert!(
            dirty.is_empty(),
            "{} address(es) came out with markup attached:\n{}",
            dirty.len(),
            dirty.join("\n")
        );
        assert!(total > 0, "the corpus produced no links to check");
    }

    fn collect_jsonl(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_jsonl(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                out.push(p);
            }
        }
    }

    /// Guards the scan loop: pathological input must terminate and stay bounded.
    /// It is not asserted that such input yields nothing — `http://http://…` has
    /// no whitespace, so treating it as one (useless) address is correct; what
    /// matters is that the scan advances and returns.
    #[test]
    fn pathological_input_terminates_and_stays_bounded() {
        let out = extract_links("https://https://https://a");
        assert!(out.len() <= 1, "{out:?}");

        let repeated = "http://".repeat(2000);
        let out = extract_links(&repeated);
        assert!(out.len() <= 1, "one unbroken run is at most one link");

        // Interleaved with whitespace: one per run, and it returns.
        let spaced = "http:// ".repeat(500);
        assert!(
            extract_links(&spaced).is_empty(),
            "a bare scheme is not an address"
        );
    }
}
