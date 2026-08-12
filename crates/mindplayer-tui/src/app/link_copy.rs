//! `Ctrl-y` — copy a link out of the focused pane's most recent answer.
//!
//! The number of links decides the UI, because the distribution is lopsided:
//! across this project's own 965-turn history, 17 of the 22 link-bearing answers
//! had exactly one link, and only 5 had more. So one link is copied outright
//! with no popup — a zoomed pane exists to be looked at, and covering it to
//! confirm a single obvious choice buys nothing — while two or more open the
//! picker.

use super::{App, LinkPicker};
use crate::links::{self, MAX_TURNS_BACK};

impl App {
    /// Find links in the focused pane's most recent answer that had any, and
    /// either copy the one or offer the several.
    pub fn copy_links_from_focused(&mut self) {
        let Some(sid) = self.focused_pane().map(str::to_string) else {
            self.status = "no live pane to copy from".to_string();
            return;
        };
        let Some(session) = self.all_sessions.iter().find(|s| s.id == sid).cloned() else {
            self.status = "that pane's session is no longer in the list".to_string();
            return;
        };
        match links::latest_links(&session, MAX_TURNS_BACK) {
            None => {
                // Logged even though nothing was copied: the usage question is
                // how often the key is reached for, and a search that came back
                // empty is still a use of it.
                self.log_link_copy(0, 0, false);
                self.status = format!("no links found · searched back {MAX_TURNS_BACK} answers");
            }
            Some(hit) if hit.links.len() == 1 => {
                self.log_link_copy(1, hit.turns_ago, false);
                let url = hit.links[0].clone();
                self.status = format!("link copied · {} · {}", url, ago(hit.turns_ago));
                self.pending_clipboard = Some(url);
            }
            Some(hit) => {
                self.log_link_copy(hit.links.len(), hit.turns_ago, true);
                self.status = format!("{} links · {}", hit.links.len(), ago(hit.turns_ago));
                self.link_picker = Some(LinkPicker {
                    links: hit.links,
                    selected: 0,
                    turns_ago: hit.turns_ago,
                });
            }
        }
    }

    fn log_link_copy(&self, links: usize, turns_ago: usize, picked: bool) {
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::LinkCopy {
                links,
                turns_ago,
                picked,
            },
        );
    }

    pub fn move_link_pick(&mut self, delta: isize) {
        let Some(p) = self.link_picker.as_mut() else {
            return;
        };
        let last = p.links.len().saturating_sub(1);
        p.selected = match delta {
            d if d < 0 => p.selected.saturating_sub(1),
            _ => (p.selected + 1).min(last),
        };
    }

    /// Copy the highlighted link and close.
    pub fn confirm_link_pick(&mut self) {
        let Some(p) = self.link_picker.take() else {
            return;
        };
        let Some(url) = p.links.get(p.selected).cloned() else {
            return;
        };
        self.status = format!("link copied · {} · {}", url, ago(p.turns_ago));
        self.pending_clipboard = Some(url);
    }

    /// Copy every link in the answer, one per line, and close.
    pub fn copy_all_links(&mut self) {
        let Some(p) = self.link_picker.take() else {
            return;
        };
        self.status = format!("{} links copied · {}", p.links.len(), ago(p.turns_ago));
        self.pending_clipboard = Some(p.links.join("\n"));
    }

    pub fn cancel_link_pick(&mut self) {
        self.link_picker = None;
    }
}

/// How far back the answer was, in the words the status line uses.
fn ago(turns_ago: usize) -> String {
    match turns_ago {
        0 => "from the latest answer".to_string(),
        1 => "from the answer before".to_string(),
        n => format!("from {n} answers back"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::isolated_app;

    #[test]
    fn ago_reads_as_a_sentence() {
        assert_eq!(ago(0), "from the latest answer");
        assert_eq!(ago(1), "from the answer before");
        assert_eq!(ago(9), "from 9 answers back");
    }

    fn picker(links: &[&str], turns_ago: usize) -> LinkPicker {
        LinkPicker {
            links: links.iter().map(|s| s.to_string()).collect(),
            selected: 0,
            turns_ago,
        }
    }

    #[test]
    fn confirming_copies_only_the_highlighted_link() {
        let mut app = isolated_app();
        app.link_picker = Some(picker(&["https://a.example", "https://b.example"], 3));
        app.move_link_pick(1);
        app.confirm_link_pick();
        assert_eq!(app.take_clipboard().as_deref(), Some("https://b.example"));
        assert!(app.link_picker.is_none(), "picker closes on confirm");
        assert!(app.status.contains("3 answers back"), "{}", app.status);
    }

    #[test]
    fn copy_all_joins_with_newlines() {
        let mut app = isolated_app();
        app.link_picker = Some(picker(&["https://a.example", "https://b.example"], 0));
        app.copy_all_links();
        assert_eq!(
            app.take_clipboard().as_deref(),
            Some("https://a.example\nhttps://b.example")
        );
        assert!(app.link_picker.is_none());
    }

    #[test]
    fn the_cursor_clamps_at_both_ends() {
        let mut app = isolated_app();
        app.link_picker = Some(picker(&["https://a.example", "https://b.example"], 0));
        app.move_link_pick(-1);
        assert_eq!(
            app.link_picker.as_ref().unwrap().selected,
            0,
            "clamped at top"
        );
        app.move_link_pick(1);
        app.move_link_pick(1);
        assert_eq!(
            app.link_picker.as_ref().unwrap().selected,
            1,
            "clamped at bottom"
        );
    }

    #[test]
    fn cancelling_copies_nothing() {
        let mut app = isolated_app();
        app.link_picker = Some(picker(&["https://a.example"], 0));
        app.cancel_link_pick();
        assert!(app.link_picker.is_none());
        assert!(app.take_clipboard().is_none(), "esc must not copy");
    }

    #[test]
    fn with_no_live_pane_it_says_so_instead_of_copying() {
        let mut app = isolated_app();
        app.panes.clear();
        app.copy_links_from_focused();
        assert!(app.take_clipboard().is_none());
        assert!(app.status.contains("no live pane"), "{}", app.status);
    }

    /// A pane whose session has no readable transcript must report that rather
    /// than silently doing nothing.
    #[test]
    fn a_pane_with_no_transcript_reports_no_links() {
        let mut app = isolated_app();
        let mut s = crate::app::tests::session("s-1", mindplayer_core::Agent::Claude, false);
        s.file = std::path::PathBuf::from("/nonexistent/never.jsonl");
        app.all_sessions = vec![s];
        app.focus_or_add_pane("s-1");
        app.copy_links_from_focused();
        assert!(app.take_clipboard().is_none());
        assert!(app.status.contains("no links found"), "{}", app.status);
    }
}
