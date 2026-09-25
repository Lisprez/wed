use super::position::{CharPos, CharRange, LineColumn};
use ropey::Rope;
use std::fmt;

pub struct Document {
    rope: Rope,
}

impl Document {
    pub fn new() -> Self {
        Self::from_text("")
    }

    pub fn from_text(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
        }
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn is_empty(&self) -> bool {
        self.len_chars() == 0
    }

    pub fn char_at(&self, pos: CharPos) -> Option<char> {
        if pos.0 < self.len_chars() {
            Some(self.rope.char(pos.0))
        } else {
            None
        }
    }

    pub fn slice(&self, range: CharRange) -> String {
        let range = range.clamp(self.len_chars());
        self.rope.slice(range.start.0..range.end.0).to_string()
    }

    pub fn insert_str(&mut self, pos: CharPos, text: &str) {
        if !text.is_empty() {
            self.rope.insert(pos.clamp(self.len_chars()).0, text);
        }
    }

    pub fn delete_range(&mut self, range: CharRange) -> String {
        let range = range.clamp(self.len_chars());
        let removed = self.slice(range);
        if !range.is_empty() {
            self.rope.remove(range.start.0..range.end.0);
        }
        removed
    }

    pub fn replace_range(&mut self, range: CharRange, text: &str) -> String {
        let range = range.clamp(self.len_chars());
        let removed = self.slice(range);
        if !range.is_empty() {
            self.rope.remove(range.start.0..range.end.0);
        }
        if !text.is_empty() {
            self.rope.insert(range.start.0, text);
        }
        removed
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn line_start(&self, line: usize) -> CharPos {
        let line = line.min(self.line_count().saturating_sub(1));
        CharPos(self.rope.line_to_char(line))
    }

    pub fn line_end(&self, line: usize) -> CharPos {
        CharPos(self.line_content_end(line))
    }

    pub fn line_end_with_newline(&self, line: usize) -> CharPos {
        let line = line.min(self.line_count().saturating_sub(1));
        let next = if line + 1 < self.line_count() {
            self.rope.line_to_char(line + 1)
        } else {
            self.len_chars()
        };
        CharPos(next)
    }

    pub fn line_text(&self, line: usize) -> String {
        let line = line.min(self.line_count().saturating_sub(1));
        self.slice(CharRange::new(self.line_start(line), self.line_end(line)))
    }

    pub fn line_range(&self, line: usize) -> CharRange {
        CharRange::new(self.line_start(line), self.line_end_with_newline(line))
    }

    pub fn pos_to_line_col(&self, pos: CharPos) -> LineColumn {
        let pos = pos.clamp(self.len_chars()).0;
        let line = self.rope.char_to_line(pos);
        let line = line.min(self.line_count().saturating_sub(1));
        LineColumn {
            line,
            column: pos.saturating_sub(self.rope.line_to_char(line)),
        }
    }

    pub fn line_col_to_pos(&self, location: LineColumn) -> CharPos {
        let line = location.line.min(self.line_count().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        let end = self.line_content_end(line);
        CharPos(start.saturating_add(location.column).min(end))
    }

    fn line_content_end(&self, line: usize) -> usize {
        let line = line.min(self.line_count().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        let next = if line + 1 < self.line_count() {
            self.rope.line_to_char(line + 1)
        } else {
            self.len_chars()
        };
        let mut end = next;
        if end > start && self.rope.char(end - 1) == '\n' {
            end -= 1;
        }
        if end > start && self.rope.char(end - 1) == '\r' {
            end -= 1;
        }
        end
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Document {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.rope.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::Document;
    use crate::core::position::{CharPos, CharRange};

    #[test]
    fn preserves_unicode_and_round_trips() {
        let mut document = Document::from_text("héllo\n世界");
        assert_eq!(document.to_string(), "héllo\n世界");
        document.insert_str(CharPos(5), "!");
        assert_eq!(document.to_string(), "héllo!\n世界");
        assert_eq!(
            document.delete_range(CharRange::new(CharPos(0), CharPos(6))),
            "héllo!"
        );
        assert_eq!(document.to_string(), "\n世界");
    }

    #[test]
    fn maps_lines_and_columns() {
        let document = Document::from_text("one\ntwo\n");
        assert_eq!(document.line_count(), 3);
        assert_eq!(document.line_text(1), "two");
        assert_eq!(
            document.pos_to_line_col(CharPos(5)),
            crate::core::position::LineColumn { line: 1, column: 1 }
        );
        assert_eq!(
            document.line_col_to_pos(crate::core::position::LineColumn { line: 1, column: 1 }),
            CharPos(5)
        );
    }

    #[test]
    fn inserts_and_deletes_across_newlines() {
        let mut document = Document::new();
        document.insert_str(CharPos::ZERO, "a\nb");
        assert_eq!(document.line_count(), 2);
        document.delete_range(CharRange::new(CharPos(1), CharPos(2)));
        assert_eq!(document.to_string(), "ab");
        assert_eq!(document.line_count(), 1);
    }
}
