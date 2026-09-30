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
    let target_line = document.pos_to_line_col(target).line;
    let first_line = current.line.min(target_line);
    let last_line = current.line.max(target_line);
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
    let current_kind = word_kind(document.char_at(CharPos(start)), big);
    let mut chars = document.chars_from(CharPos(start + 1)).peekable();
    // Leave the run the cursor sits in, leaving the first character of the next
    // run unread so it can be examined again below.
    if current_kind != WordKind::Whitespace {
        while chars
            .peek()
            .is_some_and(|(_, character)| word_kind(Some(*character), big) == current_kind)
        {
            chars.next();
        }
    }
    // Then leave the whitespace that separates them.
    loop {
        match chars.peek() {
            Some((_, character)) if word_kind(Some(*character), big) == WordKind::Whitespace => {
                chars.next();
            }
            Some((position, _)) => return position.0,
            None => return len,
        }
    }
}

fn previous_word_start(document: &Document, start: usize, big: bool) -> usize {
    let mut chars = document.chars_before(CharPos(start));
    // `index` tracks the character the iterator is sitting on, so the walk
    // follows the previous character-at-a-time implementation step for step.
    let mut index = start;
    let mut kind = WordKind::Whitespace;
    while index > 0 {
        let Some((at, character)) = chars.next() else {
            return 0;
        };
        index = at.0;
        kind = word_kind(Some(character), big);
        if kind != WordKind::Whitespace {
            break;
        }
    }
    if kind == WordKind::Whitespace || index == 0 {
        return 0;
    }
    // Walk off the rest of the run. The loop gives up once `index` reaches 0
    // without examining it, so a run reaching the start of the document reports
    // 0 even when the character there belongs to the same run.
    index -= 1;
    while index > 0 {
        let Some((at, character)) = chars.next() else {
            break;
        };
        debug_assert_eq!(at.0, index);
        if word_kind(Some(character), big) != kind {
            break;
        }
        index -= 1;
    }
    index
}

fn next_word_end(document: &Document, start: usize, big: bool) -> usize {
    let len = document.len_chars();
    let mut chars = document
        .chars_from(CharPos((start + 1).min(len)))
        .peekable();
    // Leave any leading whitespace, then walk to the end of the run after it.
    while chars
        .peek()
        .is_some_and(|(_, character)| word_kind(Some(*character), big) == WordKind::Whitespace)
    {
        chars.next();
    }
    let Some((first, kind)) = chars
        .next()
        .map(|(position, character)| (position, word_kind(Some(character), big)))
    else {
        return len.saturating_sub(1);
    };
    let mut end = first;
    for (position, character) in chars.by_ref() {
        if word_kind(Some(character), big) != kind {
            break;
        }
        end = position;
    }
    end.0
}

fn next_sentence_end(document: &Document, start: usize) -> usize {
    let len = document.len_chars();
    for (at, character) in document.chars_from(CharPos(start)) {
        if !matches!(character, '.' | '!' | '?') {
            continue;
        }
        let mut end = at.0 + 1;
        for (next, closing) in document.chars_from(CharPos(end)) {
            if !matches!(closing, ')' | ']' | '"' | '\'') {
                break;
            }
            end = next.0 + 1;
        }
        let followed_by_space = document
            .chars_from(CharPos(end))
            .next()
            .is_some_and(|(_, next)| next.is_whitespace());
        if end == len || followed_by_space {
            return end;
        }
    }
    len
}

fn previous_sentence_start(document: &Document, start: usize) -> usize {
    for (at, character) in document.chars_before(CharPos(start)) {
        if !matches!(character, '.' | '!' | '?') {
            continue;
        }
        let next = at.0 + 1;
        let followed_by_space = document
            .chars_from(CharPos(next))
            .next()
            .is_some_and(|(_, after)| after.is_whitespace());
        if next == document.len_chars() || followed_by_space {
            return next;
        }
    }
    0
}

fn is_blank_line(document: &Document, line: usize) -> bool {
    let start = document.line_start(line);
    let end = document.line_end(line);
    document
        .chars_from(start)
        .take_while(|(position, _)| position.0 < end.0)
        .all(|(_, character)| character.is_whitespace())
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
