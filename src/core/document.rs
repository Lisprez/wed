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
        let removed = self.slice(range);
        self.overwrite_range(range, text);
        removed
    }

    /// Replaces `range` with `text` without reading the old text out, which
    /// matters when `range` covers a whole document.
    pub fn overwrite_range(&mut self, range: CharRange, text: &str) {
        let range = range.clamp(self.len_chars());
        if !range.is_empty() {
            self.rope.remove(range.start.0..range.end.0);
        }
        if !text.is_empty() {
            self.rope.insert(range.start.0, text);
        }
    }

    /// Writes the whole document to `out`, chunk by chunk, so a large file never
    /// has to be held in one contiguous `String` first.
    pub fn write_to<W: std::io::Write>(&self, out: &mut W) -> std::io::Result<()> {
        for chunk in self.rope.chunks() {
            out.write_all(chunk.as_bytes())?;
        }
        Ok(())
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
        self.slice(self.line_content_range(line))
    }

    pub fn line_range(&self, line: usize) -> CharRange {
        CharRange::new(self.line_start(line), self.line_end_with_newline(line))
    }

    /// The range of `line` excluding its line break.
    pub fn line_content_range(&self, line: usize) -> CharRange {
        let line = line.min(self.line_count().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        CharRange::new(CharPos(start), CharPos(self.content_end_from(line, start)))
    }

    /// Appends the text of `range` to `out` without allocating an intermediate
    /// `String`. Equivalent to `out.push_str(&self.slice(range))`.
    pub fn write_slice(&self, range: CharRange, out: &mut String) {
        let range = range.clamp(self.len_chars());
        for chunk in self.rope.slice(range.start.0..range.end.0).chunks() {
            out.push_str(chunk);
        }
    }

    /// Every character from `pos` to the end of the document, paired with its
    /// position.
    ///
    /// Walking the rope in order moves chunk by chunk, which is far cheaper than
    /// asking for one character at a time, so scans should use this rather than
    /// repeated [`Self::char_at`].
    pub fn chars_from(&self, pos: CharPos) -> impl Iterator<Item = (CharPos, char)> + '_ {
        let start = pos.clamp(self.len_chars()).0;
        self.rope
            .chars_at(start)
            .enumerate()
            .map(move |(offset, character)| (CharPos(start + offset), character))
    }

    /// Every character before `pos`, paired with its position, nearest first.
    ///
    /// The rope is read a block at a time so a backwards scan costs the same per
    /// character as a forwards one. Each iterator carries its own block, so a
    /// scan may be nested inside another.
    pub fn chars_before(&self, pos: CharPos) -> CharsBefore<'_> {
        CharsBefore {
            document: self,
            block: String::new(),
            next: pos.clamp(self.len_chars()).0,
            offset: 0,
        }
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
        self.content_end_from(line, self.rope.line_to_char(line))
    }

    /// Char index where `line`'s text ends, before any line break. `start` must
    /// be `line_to_char(line)`, passed in so callers that already know it do not
    /// pay for a second lookup into the rope.
    fn content_end_from(&self, line: usize, start: usize) -> usize {
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

/// Characters read backwards from the document, nearest first.
pub struct CharsBefore<'a> {
    document: &'a Document,
    /// Holds the characters currently being walked.
    block: String,
    /// Char index of the next character to yield.
    next: usize,
    /// Byte offset within `block` one past the next character to yield.
    offset: usize,
}

impl Iterator for CharsBefore<'_> {
    type Item = (CharPos, char);

    fn next(&mut self) -> Option<(CharPos, char)> {
        if self.offset == 0 {
            if self.next == 0 {
                return None;
            }
            let end = self.next;
            let start = end.saturating_sub(BACKWARD_BLOCK);
            self.block.clear();
            self.document.write_slice(
                CharRange::new(CharPos(start), CharPos(end)),
                &mut self.block,
            );
            self.offset = self.block.len();
            // The block's last character is the one to yield next.
            self.next = end - 1;
        }
        let character = self.block[..self.offset].chars().next_back()?;
        self.offset -= character.len_utf8();
        let position = self.next;
        if self.offset > 0 {
            self.next = position.saturating_sub(1);
        }
        // When the block runs out the next refill reads up to `position`, which
        // is the character just yielded.
        Some((CharPos(position), character))
    }
}

/// Characters read per block by [`Document::chars_before`].
const BACKWARD_BLOCK: usize = 1024;

impl fmt::Display for Document {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.rope.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{Document, BACKWARD_BLOCK};
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

    #[test]
    fn scans_characters_forwards_and_backwards() {
        let document = Document::from_text("héllo\n世界");
        let forwards: Vec<(usize, char)> = document
            .chars_from(CharPos(2))
            .map(|(position, character)| (position.0, character))
            .collect();
        assert_eq!(forwards.first(), Some(&(2, 'l')));
        assert_eq!(forwards.last(), Some(&(7, '界')));

        let backwards: Vec<(usize, char)> = document
            .chars_before(CharPos(7))
            .map(|(position, character)| (position.0, character))
            .collect();
        // Nearest first, excluding the character at `pos` itself.
        assert_eq!(backwards.first(), Some(&(6, '世')));
        assert_eq!(backwards.last(), Some(&(0, 'h')));

        // Empty and past-the-end bounds yield nothing.
        assert_eq!(document.chars_before(CharPos::ZERO).count(), 0);
        assert_eq!(document.chars_from(CharPos(99)).count(), 0);
    }

    #[test]
    fn backward_scan_works_across_block_boundaries() {
        // Longer than one backward block, with multi-byte characters at the seam.
        let text: String = std::iter::repeat_n('a', BACKWARD_BLOCK * 3)
            .chain(std::iter::once('é'))
            .chain(std::iter::repeat_n('b', 50))
            .collect();
        let document = Document::from_text(&text);
        let backwards: Vec<char> = document
            .chars_before(CharPos(document.len_chars()))
            .map(|(_, character)| character)
            .collect();
        let expected: Vec<char> = text.chars().rev().collect();
        assert_eq!(backwards, expected);
    }
}
