//! Shared multi-line text+cursor buffer, used by every free-text prompt
//! (transition-report, catch-up) that needs cursor motion and editing.

#[derive(Debug, Clone, Default)]
pub struct BroadcastDraft {
    pub instruction: String,
    pub cursor: usize,
}

impl BroadcastDraft {
    pub fn push_text(&mut self, text: &str) {
        self.clamp_cursor();
        self.instruction.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn push_char(&mut self, c: char) {
        self.clamp_cursor();
        self.instruction.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        self.clamp_cursor();
        let Some(prev) = previous_boundary(&self.instruction, self.cursor) else {
            return;
        };
        self.instruction.drain(prev..self.cursor);
        self.cursor = prev;
    }

    pub fn delete(&mut self) {
        self.clamp_cursor();
        let Some(next) = next_boundary(&self.instruction, self.cursor) else {
            return;
        };
        self.instruction.drain(self.cursor..next);
    }

    pub fn move_left(&mut self) {
        self.clamp_cursor();
        if let Some(prev) = previous_boundary(&self.instruction, self.cursor) {
            self.cursor = prev;
        }
    }

    pub fn move_right(&mut self) {
        self.clamp_cursor();
        if let Some(next) = next_boundary(&self.instruction, self.cursor) {
            self.cursor = next;
        }
    }

    pub fn move_home(&mut self) {
        self.clamp_cursor();
        self.cursor = line_start(&self.instruction, self.cursor);
    }

    pub fn move_end(&mut self) {
        self.clamp_cursor();
        self.cursor = line_end(&self.instruction, self.cursor);
    }

    pub fn move_up(&mut self) {
        self.move_vertical(-1);
    }

    pub fn move_down(&mut self) {
        self.move_vertical(1);
    }

    fn move_vertical(&mut self, delta: isize) {
        self.clamp_cursor();
        let current_start = line_start(&self.instruction, self.cursor);
        let current_end = line_end(&self.instruction, self.cursor);
        let current_col = self.instruction[current_start..self.cursor].chars().count();
        let target = if delta < 0 {
            if current_start == 0 {
                return;
            }
            let prev_end = current_start.saturating_sub(1);
            let prev_start = line_start(&self.instruction, prev_end);
            Some((prev_start, prev_end))
        } else {
            if current_end >= self.instruction.len() {
                return;
            }
            let next_start = current_end + 1;
            let next_end = line_end(&self.instruction, next_start);
            Some((next_start, next_end))
        };
        let Some((target_start, target_end)) = target else {
            return;
        };
        self.cursor =
            byte_index_at_char_col(&self.instruction, target_start, target_end, current_col);
    }

    fn clamp_cursor(&mut self) {
        if self.cursor > self.instruction.len() {
            self.cursor = self.instruction.len();
        }
        while self.cursor > 0 && !self.instruction.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }
}

fn previous_boundary(text: &str, cursor: usize) -> Option<usize> {
    text[..cursor].char_indices().last().map(|(index, _)| index)
}

fn next_boundary(text: &str, cursor: usize) -> Option<usize> {
    text[cursor..]
        .char_indices()
        .nth(1)
        .map(|(index, _)| cursor + index)
        .or_else(|| (cursor < text.len()).then_some(text.len()))
}

fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor].rfind('\n').map_or(0, |index| index + 1)
}

fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map_or(text.len(), |index| cursor + index)
}

fn byte_index_at_char_col(text: &str, start: usize, end: usize, col: usize) -> usize {
    text[start..end]
        .char_indices()
        .nth(col)
        .map_or(end, |(index, _)| start + index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_draft_supports_cursor_editing() {
        let mut draft = BroadcastDraft::default();
        draft.push_text("abc\n한글");
        assert_eq!(draft.cursor, "abc\n한글".len());
        draft.move_left();
        draft.move_left();
        draft.push_char('!');
        assert_eq!(draft.instruction, "abc\n!한글");
        draft.move_home();
        draft.push_text("리뷰 ");
        assert_eq!(draft.instruction, "abc\n리뷰 !한글");
        draft.move_end();
        draft.backspace();
        assert_eq!(draft.instruction, "abc\n리뷰 !한");
        draft.move_up();
        draft.delete();
        assert_eq!(draft.instruction, "abc리뷰 !한");
    }
}
