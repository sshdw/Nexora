//! Draft editing with minimal wrap-aware navigation (brief §5.4).
//!
//! The draft is plain text held in one `String` with a byte cursor kept on a
//! UTF-8 boundary. Visual (wrapped) lines are derived on demand and reused for
//! both rendering the caret and for `Up`/`Down` cursor movement, so editing a
//! long, wrapped draft stays predictable. There is no external text-area crate:
//! the operation set is intentionally small and fully unit-tested.

/// One rendered (wrapped) line: its text and the byte offset in the draft where
/// it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisualLine {
    pub(crate) text: String,
    pub(crate) start: usize,
}

/// Wrap one line of text to at most `width` characters, returning each chunk
/// with its starting byte offset in `text`. Empty lines still produce one
/// (empty) chunk so the caret can rest there.
fn wrap_line(text: &str, width: usize) -> Vec<(String, usize)> {
    let width = width.max(1);
    if text.is_empty() {
        return vec![(String::new(), 0)];
    }
    let mut out = Vec::new();
    let mut chunk = String::new();
    let mut chunk_start = 0usize;
    let mut chunk_chars = 0usize;
    let mut byte = 0usize;
    for ch in text.chars() {
        if chunk_chars == width {
            out.push((std::mem::take(&mut chunk), chunk_start));
            chunk_start = byte;
            chunk_chars = 0;
        }
        chunk.push(ch);
        chunk_chars += 1;
        byte += ch.len_utf8();
    }
    out.push((chunk, chunk_start));
    out
}

/// The composer's entire editable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Composer {
    draft: String,
    cursor: usize,
}

impl Default for Composer {
    fn default() -> Self {
        Self::new()
    }
}

impl Composer {
    /// An empty composer with the cursor at offset zero.
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            draft: String::new(),
            cursor: 0,
        }
    }

    /// The current draft bytes.
    #[must_use]
    pub(crate) fn draft(&self) -> &str {
        &self.draft
    }

    /// Byte offset of the caret in the draft (always a UTF-8 boundary).
    #[must_use]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether the draft is empty.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.draft.is_empty()
    }

    /// Hand the draft to the caller and reset to empty. Used exactly once, on
    /// host acceptance.
    pub(crate) fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.draft)
    }

    /// Replace the draft wholesale (test helper for seeding deterministic state).
    #[cfg(test)]
    pub(crate) fn set_draft(&mut self, draft: String) {
        self.cursor = draft.len();
        self.draft = draft;
    }

    /// Insert text at the caret (used for ordinary keypresses and pastes).
    pub(crate) fn insert(&mut self, text: &str) {
        let at = self.cursor.min(self.draft.len());
        self.draft.insert_str(at, text);
        self.cursor = at + text.len();
    }

    /// Insert a single character at the caret.
    pub(crate) fn insert_char(&mut self, ch: char) {
        let mut buf = [0u8; 4];
        self.insert(ch.encode_utf8(&mut buf));
    }

    /// Delete the character before the caret.
    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prior = self.draft[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(idx, _)| idx);
        self.draft.replace_range(prior..self.cursor, "");
        self.cursor = prior;
    }

    /// Delete the character under the caret.
    pub(crate) fn delete(&mut self) {
        if self.cursor >= self.draft.len() {
            return;
        }
        let len = self.draft[self.cursor..]
            .chars()
            .next()
            .map_or(1, char::len_utf8);
        self.draft.replace_range(self.cursor..self.cursor + len, "");
    }

    /// Move the caret one character left/right, clamped to the draft.
    pub(crate) fn move_left(&mut self) {
        self.cursor = self.draft[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(idx, _)| idx);
    }

    pub(crate) fn move_right(&mut self) {
        if self.cursor >= self.draft.len() {
            return;
        }
        let len = self.draft[self.cursor..]
            .chars()
            .next()
            .map_or(1, char::len_utf8);
        self.cursor += len;
    }

    /// Move to the start/end of the current *logical* line (Home/End).
    pub(crate) fn move_line_start(&mut self) {
        self.cursor = self.draft[..self.cursor]
            .rfind('\n')
            .map_or(0, |idx| idx + 1);
    }

    pub(crate) fn move_line_end(&mut self) {
        let rest = &self.draft[self.cursor..];
        self.cursor = rest
            .find('\n')
            .map_or(self.draft.len(), |idx| self.cursor + idx);
    }

    /// Compute the wrapped visual lines for a given width.
    #[must_use]
    pub(crate) fn visual_lines(&self, width: u16) -> Vec<VisualLine> {
        let width = usize::from(width).max(1);
        let mut lines = Vec::new();
        let mut base = 0usize;
        for logical in self.draft.split('\n') {
            for (text, start) in wrap_line(logical, width) {
                lines.push(VisualLine {
                    text,
                    start: base + start,
                });
            }
            base += logical.len() + 1; // +1 for the consumed '\n'
        }
        lines
    }

    /// Move the caret one visual row up/down, preserving the column where
    /// possible (wrap-aware `Up`/`Down` in the composer).
    pub(crate) fn move_visual(&mut self, width: u16, direction: Vertical) {
        let lines = self.visual_lines(width);
        let current = lines
            .iter()
            .rposition(|line| self.cursor >= line.start)
            .unwrap_or(0);
        let column = self.draft[lines[current].start..self.cursor]
            .chars()
            .count();
        let target = match direction {
            Vertical::Up => current.saturating_sub(1),
            Vertical::Down => (current + 1).min(lines.len().saturating_sub(1)),
        };
        if target == current {
            return;
        }
        let line = &lines[target];
        let chars = line.text.chars().count();
        let take = column.min(chars);
        let mut byte = line.start;
        for ch in line.text.chars().take(take) {
            byte += ch.len_utf8();
        }
        self.cursor = byte;
    }
}

/// Direction for wrap-aware vertical caret movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vertical {
    Up,
    Down,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_delete_respect_utf8_boundaries() {
        let mut c = Composer::new();
        c.insert("hé");
        c.insert_char('→');
        assert_eq!(c.draft(), "hé→");
        assert_eq!(c.cursor(), "hé→".len());
        c.move_left(); // caret now before '→'
        c.backspace(); // removes 'é' — a whole UTF-8 scalar, not half a byte
        assert_eq!(c.draft(), "h→");
        c.delete();
        assert_eq!(c.draft(), "h");
    }

    #[test]
    fn wrapped_lines_keep_byte_offsets() {
        let mut c = Composer::new();
        c.set_draft("abcdefgh".to_string());
        let lines = c.visual_lines(3);
        assert_eq!(
            lines[0],
            VisualLine {
                text: "abc".into(),
                start: 0
            }
        );
        assert_eq!(
            lines[1],
            VisualLine {
                text: "def".into(),
                start: 3
            }
        );
        assert_eq!(
            lines[2],
            VisualLine {
                text: "gh".into(),
                start: 6
            }
        );
    }

    #[test]
    fn vertical_movement_is_wrap_aware() {
        let mut c = Composer::new();
        c.set_draft("abcdefgh".to_string());
        c.cursor = 4; // start of "def"
        c.move_visual(3, Vertical::Down);
        assert_eq!(c.cursor, 7); // column 1 of "gh"
        c.move_visual(3, Vertical::Up);
        assert_eq!(c.cursor, 4);
    }

    #[test]
    fn take_resets_the_composer() {
        let mut c = Composer::new();
        c.insert("task");
        assert_eq!(c.take(), "task");
        assert!(c.is_empty());
        assert_eq!(c.cursor(), 0);
    }
}
