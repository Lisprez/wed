use super::document::Document;
use super::position::{CharPos, CharRange, LineColumn, RangeKind};
use super::textobject::matching_pair;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    FirstNonBlank,
    LineEnd,
    WordForward,
    WordForwardBig,
    WordBackward,
    WordBackwardBig,
    WordEnd,
    WordEndBig,
    ParagraphForward,
    ParagraphBackward,
    SentenceForward,
    SentenceBackward,
    GotoFirst,
    GotoLast,
    MatchingPair,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionResult {
    pub target: CharPos,
    pub range: CharRange,
    pub kind: RangeKind,
}

pub fn resolve(document: &Document, cursor: CharPos, motion: Motion, count: usize) -> MotionResult {
    let count = count.max(1);
    let cursor = cursor.clamp(document.len_chars());
    match motion {
        Motion::Left => horizontal(document, cursor, count, false),
        Motion::Right => horizontal(document, cursor, count, true),
        Motion::Up => vertical(document, cursor, count, false),
        Motion::Down => vertical(document, cursor, count, true),
        Motion::LineStart => {
            let line = document.pos_to_line_col(cursor).line;
            let target = document.line_start(line);
            result(
                target,
                CharRange::new(target, cursor),
                RangeKind::Characterwise,
                false,
            )
        }
        Motion::FirstNonBlank => {
            let line = document.pos_to_line_col(cursor).line;
            let start = document.line_start(line);
            let end = document.line_end(line);
            let target = (start.0..end.0)
                .find(|index| {
                    document
                        .char_at(CharPos(*index))
                        .is_some_and(|character| !character.is_whitespace())
                })
                .map(CharPos)
                .unwrap_or(start);
            result(
                target,
                CharRange::new(target, cursor),
                RangeKind::Characterwise,
                false,
            )
        }
        Motion::LineEnd => {
            let line = document.pos_to_line_col(cursor).line;
            let target = document.line_end(line);
            let end = target.advance(usize::from(target.0 < document.len_chars()));
            result(
                target,
                CharRange::new(cursor, end),
                RangeKind::Characterwise,
                false,
            )
        }
        Motion::WordForward => word_forward(document, cursor, count, false),
        Motion::WordForwardBig => word_forward(document, cursor, count, true),
        Motion::WordBackward => word_backward(document, cursor, count, false),
        Motion::WordBackwardBig => word_backward(document, cursor, count, true),
        Motion::WordEnd => word_end(document, cursor, count, false),
        Motion::WordEndBig => word_end(document, cursor, count, true),
        Motion::ParagraphForward => paragraph(document, cursor, count, true),
        Motion::ParagraphBackward => paragraph(document, cursor, count, false),
        Motion::SentenceForward => sentence(document, cursor, count, true),
        Motion::SentenceBackward => sentence(document, cursor, count, false),
        Motion::GotoFirst => {
            let target = document.line_start(0);
            result(
                target,
                CharRange::new(target, cursor),
                RangeKind::Linewise,
                false,
            )
        }
        Motion::GotoLast => {
            let line = document.line_count().saturating_sub(1);
            let target = document.line_start(line);
            result(
                target,
                CharRange::new(target, cursor),
                RangeKind::Linewise,
                false,
            )
        }
        Motion::MatchingPair => {
            if let Some(range) = matching_pair(document, cursor) {
                let target = if cursor.0 <= range.start.0 {
                    CharPos(range.end.0.saturating_sub(1))
                } else {
                    range.start
                };
                result(target, range, RangeKind::Characterwise, false)
            } else {
                result(
                    cursor,
                    CharRange::empty(cursor),
                    RangeKind::Characterwise,
                    false,
                )
            }
        }
    }
}

fn horizontal(document: &Document, cursor: CharPos, count: usize, forward: bool) -> MotionResult {
    let target = if forward {
        cursor.advance(count).clamp(document.len_chars())
    } else {
        CharPos(cursor.0.saturating_sub(count))
    };
    result(
        target,
        CharRange::new(cursor, target),
        RangeKind::Characterwise,
        false,
    )
}

fn vertical(document: &Document, cursor: CharPos, count: usize, forward: bool) -> MotionResult {
    let current = document.pos_to_line_col(cursor);
    let target_line = if forward {
        current.line.saturating_add(count)
    } else {
        current.line.saturating_sub(count)
    };
    let target = document.line_col_to_pos(LineColumn {
        line: target_line,
        column: current.column,
    });
    let first_line = current.line.min(document.pos_to_line_col(target).line);
    let last_line = current.line.max(document.pos_to_line_col(target).line);
    let range = CharRange::new(
        document.line_start(first_line),
        document.line_end_with_newline(last_line),
    );
    result(target, range, RangeKind::Linewise, false)
}

fn word_forward(document: &Document, cursor: CharPos, count: usize, big: bool) -> MotionResult {
    let mut target = cursor.0;
    for _ in 0..count {
        target = next_word_start(document, target, big);
    }
    let target = CharPos(target);
    result(
        target,
        CharRange::new(cursor, target),
        RangeKind::Characterwise,
        false,
    )
}

fn word_backward(document: &Document, cursor: CharPos, count: usize, big: bool) -> MotionResult {
    let mut position = cursor.0;
    for _ in 0..count {
        position = previous_word_start(document, position, big);
    }
    let target = CharPos(position);
    result(
        target,
        CharRange::new(target, cursor),
        RangeKind::Characterwise,
        false,
    )
}

fn word_end(document: &Document, cursor: CharPos, count: usize, big: bool) -> MotionResult {
    let mut position = cursor.0;
    for _ in 0..count {
        position = next_word_end(document, position, big);
    }
    let target = CharPos(position);
    let end = target.advance(usize::from(target.0 < document.len_chars()));
    result(
        target,
        CharRange::new(cursor, end),
        RangeKind::Characterwise,
        false,
    )
}

fn paragraph(document: &Document, cursor: CharPos, count: usize, forward: bool) -> MotionResult {
    let current_line = document.pos_to_line_col(cursor).line;
    let mut line = current_line;
    for _ in 0..count {
        if forward {
            line += 1;
            while line < document.line_count() && is_blank_line(document, line) {
                line += 1;
            }
        } else {
            if line == 0 {
                break;
            }
            line -= 1;
            while line > 0 && is_blank_line(document, line) {
                line -= 1;
            }
        }
    }
    let target = document.line_start(line.min(document.line_count().saturating_sub(1)));
    let range = if forward {
        CharRange::new(cursor, document.line_end_with_newline(line))
    } else {
        CharRange::new(document.line_start(line), cursor)
    };
    result(target, range, RangeKind::Linewise, false)
}

fn sentence(document: &Document, cursor: CharPos, count: usize, forward: bool) -> MotionResult {
    let mut position = cursor.0;
    for _ in 0..count {
        if forward {
            position = next_sentence_end(document, position);
        } else {
            position = previous_sentence_start(document, position);
        }
    }
    let target = CharPos(position);
    let range = if forward {
        CharRange::new(cursor, target)
    } else {
        CharRange::new(target, cursor)
    };
    result(target, range, RangeKind::Characterwise, false)
}

fn next_word_start(document: &Document, start: usize, big: bool) -> usize {
    let len = document.len_chars();
    if start >= len {
        return len;
    }
    let mut index = start + 1;
    let current_kind = word_kind(document.char_at(CharPos(start)), big);
    if current_kind != WordKind::Whitespace {
        while index < len && word_kind(document.char_at(CharPos(index)), big) == current_kind {
            index += 1;
        }
    }
    while index < len && word_kind(document.char_at(CharPos(index)), big) == WordKind::Whitespace {
        index += 1;
    }
    index.min(len)
}

fn previous_word_start(document: &Document, start: usize, big: bool) -> usize {
    if start == 0 {
        return 0;
    }
    let mut index = start.saturating_sub(1);
    while index > 0 && word_kind(document.char_at(CharPos(index)), big) == WordKind::Whitespace {
        index -= 1;
    }
    if index == 0 {
        return 0;
    }
    let kind = word_kind(document.char_at(CharPos(index)), big);
    index -= 1;
    while index > 0 && word_kind(document.char_at(CharPos(index)), big) == kind {
        index -= 1;
    }
    index
}

fn next_word_end(document: &Document, start: usize, big: bool) -> usize {
    let len = document.len_chars();
    let mut index = (start + 1).min(len);
    while index < len && word_kind(document.char_at(CharPos(index)), big) == WordKind::Whitespace {
        index += 1;
    }
    if index >= len {
        return len.saturating_sub(1);
    }
    let kind = word_kind(document.char_at(CharPos(index)), big);
    while index + 1 < len && word_kind(document.char_at(CharPos(index + 1)), big) == kind {
        index += 1;
    }
    index
}

fn next_sentence_end(document: &Document, start: usize) -> usize {
    let len = document.len_chars();
    let mut index = start;
    while index < len {
        let character = document.char_at(CharPos(index));
        if matches!(character, Some('.' | '!' | '?')) {
            let mut end = index + 1;
            while end < len
                && matches!(document.char_at(CharPos(end)), Some(')' | ']' | '"' | '\''))
            {
                end += 1;
            }
            if end == len
                || document
                    .char_at(CharPos(end))
                    .is_some_and(char::is_whitespace)
            {
                return end;
            }
        }
        index += 1;
    }
    len
}

fn previous_sentence_start(document: &Document, start: usize) -> usize {
    if start == 0 {
        return 0;
    }
    let mut index = start;
    while index > 0 {
        index -= 1;
        let character = document.char_at(CharPos(index));
        if matches!(character, Some('.' | '!' | '?')) {
            let next = index + 1;
            if next == document.len_chars()
                || document
                    .char_at(CharPos(next))
                    .is_some_and(char::is_whitespace)
            {
                return index + 1;
            }
        }
    }
    0
}

fn is_blank_line(document: &Document, line: usize) -> bool {
    document.line_text(line).chars().all(char::is_whitespace)
}

fn result(target: CharPos, range: CharRange, kind: RangeKind, inclusive: bool) -> MotionResult {
    let range = if inclusive && range.end.0 < usize::MAX {
        CharRange::new(range.start, range.end.advance(1))
    } else {
        range
    };
    MotionResult {
        target,
        range,
        kind,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WordKind {
    Keyword,
    Punctuation,
    Whitespace,
}

fn word_kind(character: Option<char>, big: bool) -> WordKind {
    let Some(character) = character else {
        return WordKind::Whitespace;
    };
    if character.is_whitespace() {
        return WordKind::Whitespace;
    }
    if big || character.is_alphanumeric() || character == '_' {
        WordKind::Keyword
    } else {
        WordKind::Punctuation
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve, Motion};
    use crate::core::document::Document;
    use crate::core::position::CharPos;

    #[test]
    fn resolves_horizontal_motion_and_line_motion() {
        let document = Document::from_text("one\ntwo");
        let result = resolve(&document, CharPos(0), Motion::Down, 1);
        assert_eq!(result.target, CharPos(4));
        assert_eq!(result.kind, crate::core::position::RangeKind::Linewise);

        let result = resolve(&document, CharPos(1), Motion::Right, 2);
        assert_eq!(result.target, CharPos(3));
    }

    #[test]
    fn resolves_word_motion() {
        let document = Document::from_text("one two three");
        let result = resolve(&document, CharPos(0), Motion::WordForward, 1);
        assert_eq!(result.target, CharPos(4));
        let result = resolve(&document, CharPos(4), Motion::WordEnd, 1);
        assert_eq!(result.target, CharPos(6));
    }
}
