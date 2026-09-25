use super::document::Document;
use super::position::{CharPos, CharRange};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairKind {
    Parenthesis,
    Bracket,
    Brace,
    Angle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextObject {
    Word { around: bool, big: bool },
    Sentence { around: bool },
    Paragraph { around: bool },
    Quote { around: bool, quote: char },
    Pair { around: bool, kind: PairKind },
    Tag { around: bool },
}

pub fn parse(prefix: char, key: char) -> Option<TextObject> {
    let around = match prefix {
        'a' => true,
        'i' => false,
        _ => return None,
    };
    match key {
        'w' => Some(TextObject::Word { around, big: false }),
        'W' => Some(TextObject::Word { around, big: true }),
        's' => Some(TextObject::Sentence { around }),
        'p' => Some(TextObject::Paragraph { around }),
        '"' | '\'' | '`' => Some(TextObject::Quote { around, quote: key }),
        'b' | '(' | ')' => Some(TextObject::Pair {
            around,
            kind: PairKind::Parenthesis,
        }),
        '[' | ']' => Some(TextObject::Pair {
            around,
            kind: PairKind::Bracket,
        }),
        'B' | '{' | '}' => Some(TextObject::Pair {
            around,
            kind: PairKind::Brace,
        }),
        '<' | '>' => Some(TextObject::Pair {
            around,
            kind: PairKind::Angle,
        }),
        't' => Some(TextObject::Tag { around }),
        _ => None,
    }
}

pub fn resolve(
    document: &Document,
    cursor: CharPos,
    object: TextObject,
    count: usize,
) -> Option<CharRange> {
    let count = count.max(1);
    match object {
        TextObject::Word { around, big } => word_object(document, cursor, around, big, count),
        TextObject::Sentence { around } => sentence_object(document, cursor, around, count),
        TextObject::Paragraph { around } => paragraph_object(document, cursor, around),
        TextObject::Quote { around, quote } => quote_object(document, cursor, around, quote, count),
        TextObject::Pair { around, kind } => pair_object(document, cursor, around, kind, count),
        TextObject::Tag { around } => tag_object(document, cursor, around),
    }
}

pub fn matching_pair(document: &Document, cursor: CharPos) -> Option<CharRange> {
    if document.is_empty() {
        return None;
    }
    let position = cursor.0.min(document.len_chars() - 1);
    if let Some(kind) = pair_kind_at(document.char_at(CharPos(position))) {
        if let Some((open, close)) = enclosing_pair(document, CharPos(position), kind) {
            return Some(CharRange::new(CharPos(open), CharPos(close + 1)));
        }
    }
    for kind in [
        PairKind::Parenthesis,
        PairKind::Bracket,
        PairKind::Brace,
        PairKind::Angle,
    ] {
        if let Some((open, close)) =
            next_opening_pair(document, CharPos(position.saturating_add(1)), kind)
        {
            return Some(CharRange::new(CharPos(open), CharPos(close + 1)));
        }
    }
    None
}

pub fn matching_partner(document: &Document, cursor: CharPos) -> Option<CharPos> {
    if document.is_empty() {
        return None;
    }
    let position = cursor.0.min(document.len_chars() - 1);
    match document.char_at(CharPos(position)) {
        Some('"' | '\'' | '`') => {
            matching_quote_partner(document, position, document.char_at(CharPos(position))?)
        }
        Some('(' | ')' | '[' | ']' | '{' | '}' | '<' | '>') => {
            let range = matching_pair(document, CharPos(position))?;
            if position == range.start.0 {
                Some(CharPos(range.end.0.saturating_sub(1)))
            } else if position + 1 == range.end.0 {
                Some(range.start)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn matching_quote_partner(document: &Document, position: usize, quote: char) -> Option<CharPos> {
    let line = document.pos_to_line_col(CharPos(position)).line;
    let start = document.line_start(line).0;
    let end = document.line_end(line).0;
    let mut positions = Vec::new();
    for index in start..end {
        if document.char_at(CharPos(index)) == Some(quote) && !is_escaped(document, index) {
            positions.push(index);
        }
    }
    for pair in positions.chunks(2) {
        if pair[0] == position {
            return pair.get(1).copied().map(CharPos);
        }
        if pair.get(1).copied() == Some(position) {
            return Some(CharPos(pair[0]));
        }
    }
    None
}

fn word_object(
    document: &Document,
    cursor: CharPos,
    around: bool,
    big: bool,
    count: usize,
) -> Option<CharRange> {
    if document.is_empty() {
        return Some(CharRange::empty(CharPos::ZERO));
    }
    let position = cursor.0.min(document.len_chars().saturating_sub(1));
    let (mut start, mut end) = unit_bounds(document, position, big);
    for _ in 1..count {
        let next = next_unit_start(document, end, big);
        if next >= document.len_chars() || next <= start {
            break;
        }
        let (_, next_end) = unit_bounds(document, next, big);
        end = next_end;
    }
    if around {
        let extended = include_adjacent_whitespace(document, start, end);
        start = extended.start.0;
        end = extended.end.0;
    }
    Some(CharRange::new(CharPos(start), CharPos(end)))
}

fn sentence_object(
    document: &Document,
    cursor: CharPos,
    around: bool,
    count: usize,
) -> Option<CharRange> {
    if document.is_empty() {
        return Some(CharRange::empty(CharPos::ZERO));
    }
    let position = cursor.0.min(document.len_chars().saturating_sub(1));
    let mut start = sentence_start_containing(document, position);
    let mut end = sentence_end_from(document, start);
    for _ in 1..count {
        if end >= document.len_chars() {
            break;
        }
        start = next_sentence_start(document, end);
        end = sentence_end_from(document, start);
    }
    if around {
        let extended = include_adjacent_whitespace(document, start, end);
        start = extended.start.0;
        end = extended.end.0;
    }
    Some(CharRange::new(CharPos(start), CharPos(end)))
}

fn paragraph_object(document: &Document, cursor: CharPos, around: bool) -> Option<CharRange> {
    let line = document.pos_to_line_col(cursor).line;
    if line >= document.line_count() {
        return Some(CharRange::empty(cursor));
    }
    let mut first = line;
    let mut last = line;
    while first > 0 && !is_blank_line(document, first - 1) {
        first -= 1;
    }
    while last + 1 < document.line_count() && !is_blank_line(document, last + 1) {
        last += 1;
    }
    let mut range = CharRange::new(
        document.line_start(first),
        document.line_end_with_newline(last),
    );
    if around && last + 1 < document.line_count() && is_blank_line(document, last + 1) {
        range.end = document.line_end_with_newline(last + 1);
    }
    Some(range)
}

fn quote_object(
    document: &Document,
    cursor: CharPos,
    around: bool,
    quote: char,
    count: usize,
) -> Option<CharRange> {
    let line = document.pos_to_line_col(cursor).line;
    let start = document.line_start(line).0;
    let end = document.line_end(line).0;
    let mut quote_positions = Vec::new();
    let mut index = start;
    while index < end {
        if document.char_at(CharPos(index)) == Some(quote) && !is_escaped(document, index) {
            quote_positions.push(index);
        }
        index += 1;
    }
    if quote_positions.len() < 2 {
        return None;
    }
    let mut pair_index = None;
    for index in (0..quote_positions.len() - 1).step_by(2) {
        let open = quote_positions[index];
        let close = quote_positions[index + 1];
        if cursor.0 >= open && cursor.0 <= close + 1 {
            pair_index = Some(index);
            break;
        }
    }
    if pair_index.is_none() {
        pair_index = quote_positions
            .iter()
            .step_by(2)
            .position(|open| cursor.0 <= *open);
    }
    let pair_index = pair_index?;
    let open = quote_positions[pair_index];
    let close = quote_positions
        .get(pair_index + 1)
        .copied()
        .unwrap_or(open + 1);
    let mut selected = if around {
        include_adjacent_whitespace(document, open, close + 1)
    } else {
        CharRange::new(CharPos(open + 1), CharPos(close))
    };
    for _ in 1..count {
        let next_close = quote_positions.get(pair_index + 3).copied();
        if let Some(next_close) = next_close {
            selected = CharRange::new(selected.start, CharPos(next_close + 1));
        } else {
            break;
        }
    }
    Some(selected)
}

fn pair_object(
    document: &Document,
    cursor: CharPos,
    around: bool,
    kind: PairKind,
    count: usize,
) -> Option<CharRange> {
    let mut pair = enclosing_pair(document, cursor, kind);
    if pair.is_none() {
        let next = next_opening_pair(document, cursor, kind)?;
        pair = Some(next);
    }
    let (mut open, mut close) = pair?;
    for _ in 1..count {
        let previous = enclosing_pair(document, CharPos(open.saturating_sub(1)), kind);
        let Some((previous_open, previous_close)) = previous else {
            break;
        };
        open = previous_open;
        close = previous_close;
    }
    Some(if around {
        CharRange::new(CharPos(open), CharPos(close + 1))
    } else {
        CharRange::new(CharPos(open + 1), CharPos(close))
    })
}

fn tag_object(document: &Document, cursor: CharPos, around: bool) -> Option<CharRange> {
    let tags = all_tags(document);
    let mut stack: Vec<(String, usize)> = Vec::new();
    let mut pair = None;
    for tag in &tags {
        if tag.closing {
            if let Some(index) = stack.iter().rposition(|(name, _)| *name == tag.name) {
                let (_, open) = stack.remove(index);
                if cursor.0 >= open && cursor.0 <= tag.end {
                    pair = Some((open, tag.end));
                    break;
                }
            }
        } else if !tag.self_closing {
            stack.push((tag.name.clone(), tag.start));
        }
    }
    let (open, close) = pair?;
    Some(if around {
        CharRange::new(CharPos(open), CharPos(close + 1))
    } else {
        CharRange::new(CharPos(open + 1), CharPos(close))
    })
}

fn unit_bounds(document: &Document, position: usize, big: bool) -> (usize, usize) {
    let kind = word_kind(document.char_at(CharPos(position)), big);
    let mut start = position;
    let mut end = position + 1;
    while start > 0 && word_kind(document.char_at(CharPos(start - 1)), big) == kind {
        start -= 1;
    }
    while end < document.len_chars() && word_kind(document.char_at(CharPos(end)), big) == kind {
        end += 1;
    }
    (start, end)
}

fn next_unit_start(document: &Document, start: usize, big: bool) -> usize {
    let mut index = start;
    while index < document.len_chars()
        && word_kind(document.char_at(CharPos(index)), big) != WordKind::Whitespace
    {
        index += 1;
    }
    while index < document.len_chars()
        && word_kind(document.char_at(CharPos(index)), big) == WordKind::Whitespace
    {
        index += 1;
    }
    index
}

fn include_adjacent_whitespace(document: &Document, start: usize, end: usize) -> CharRange {
    let mut new_start = start;
    let mut new_end = end;
    while new_end < document.len_chars()
        && document
            .char_at(CharPos(new_end))
            .is_some_and(|character| matches!(character, ' ' | '\t'))
    {
        new_end += 1;
    }
    if new_end == end {
        while new_start > 0
            && document
                .char_at(CharPos(new_start - 1))
                .is_some_and(|character| matches!(character, ' ' | '\t'))
        {
            new_start -= 1;
        }
    }
    CharRange::new(CharPos(new_start), CharPos(new_end))
}

fn sentence_start_containing(document: &Document, position: usize) -> usize {
    let mut start = 0;
    let mut index = 0;
    while index < position {
        let Some(end) = sentence_boundary_at(document, index) else {
            index += 1;
            continue;
        };
        if end > position {
            return index;
        }
        start = end;
        index = end;
    }
    start
}

fn next_sentence_start(document: &Document, start: usize) -> usize {
    let len = document.len_chars();
    let mut index = start;
    while index < len {
        if sentence_boundary_at(document, index).is_some() {
            return index;
        }
        index += 1;
    }
    len
}

fn sentence_end_from(document: &Document, start: usize) -> usize {
    let len = document.len_chars();
    let mut index = start;
    while index < len {
        if let Some(end) = sentence_boundary_at(document, index) {
            return end;
        }
        index += 1;
    }
    len
}

fn sentence_boundary_at(document: &Document, index: usize) -> Option<usize> {
    let character = document.char_at(CharPos(index))?;
    if !matches!(character, '.' | '!' | '?') {
        return None;
    }
    let mut end = index + 1;
    while end < document.len_chars()
        && matches!(document.char_at(CharPos(end)), Some(')' | ']' | '"' | '\''))
    {
        end += 1;
    }
    if end == document.len_chars()
        || document
            .char_at(CharPos(end))
            .is_some_and(char::is_whitespace)
    {
        Some(end)
    } else {
        None
    }
}

fn enclosing_pair(document: &Document, cursor: CharPos, kind: PairKind) -> Option<(usize, usize)> {
    let (open_character, close_character) = delimiters(kind);
    let position = cursor.0.min(document.len_chars().saturating_sub(1));
    let mut depth = 0usize;
    let mut open = None;
    for index in (0..=position).rev() {
        let character = document.char_at(CharPos(index));
        if character == Some(close_character) && index < position && !is_escaped(document, index) {
            depth += 1;
        } else if character == Some(open_character) && !is_escaped(document, index) {
            if depth == 0 {
                open = Some(index);
                break;
            }
            depth -= 1;
        }
    }
    let open = open?;
    let mut depth = 0usize;
    for index in open..document.len_chars() {
        let character = document.char_at(CharPos(index));
        if character == Some(open_character) && !is_escaped(document, index) {
            depth += 1;
        } else if character == Some(close_character) && !is_escaped(document, index) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some((open, index));
            }
        }
    }
    None
}

fn next_opening_pair(
    document: &Document,
    cursor: CharPos,
    kind: PairKind,
) -> Option<(usize, usize)> {
    let (open_character, close_character) = delimiters(kind);
    let start = cursor.0.min(document.len_chars());
    for index in start..document.len_chars() {
        if document.char_at(CharPos(index)) == Some(open_character) && !is_escaped(document, index)
        {
            if let Some(close) = matching_close(document, index, open_character, close_character) {
                return Some((index, close));
            }
        }
    }
    None
}

fn matching_close(
    document: &Document,
    open: usize,
    open_character: char,
    close_character: char,
) -> Option<usize> {
    let mut depth = 0usize;
    for index in open..document.len_chars() {
        let character = document.char_at(CharPos(index));
        if character == Some(open_character) && !is_escaped(document, index) {
            depth += 1;
        } else if character == Some(close_character) && !is_escaped(document, index) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn pair_kind_at(character: Option<char>) -> Option<PairKind> {
    match character {
        Some('(' | ')') => Some(PairKind::Parenthesis),
        Some('[' | ']') => Some(PairKind::Bracket),
        Some('{' | '}') => Some(PairKind::Brace),
        Some('<' | '>') => Some(PairKind::Angle),
        _ => None,
    }
}

fn delimiters(kind: PairKind) -> (char, char) {
    match kind {
        PairKind::Parenthesis => ('(', ')'),
        PairKind::Bracket => ('[', ']'),
        PairKind::Brace => ('{', '}'),
        PairKind::Angle => ('<', '>'),
    }
}

fn is_escaped(document: &Document, index: usize) -> bool {
    let mut backslashes = 0usize;
    let mut current = index;
    while current > 0 {
        current -= 1;
        if document.char_at(CharPos(current)) != Some('\\') {
            break;
        }
        backslashes += 1;
    }
    backslashes % 2 == 1
}

fn is_blank_line(document: &Document, line: usize) -> bool {
    document.line_text(line).chars().all(char::is_whitespace)
}

#[derive(Clone)]
struct Tag {
    start: usize,
    end: usize,
    name: String,
    closing: bool,
    self_closing: bool,
}

fn all_tags(document: &Document) -> Vec<Tag> {
    let mut tags = Vec::new();
    let mut index = 0;
    while index < document.len_chars() {
        if document.char_at(CharPos(index)) != Some('<') {
            index += 1;
            continue;
        }
        let Some(end) = (index + 1..document.len_chars())
            .find(|candidate| document.char_at(CharPos(*candidate)) == Some('>'))
        else {
            break;
        };
        let raw: String = (index + 1..end)
            .filter_map(|position| document.char_at(CharPos(position)))
            .collect();
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let closing = trimmed.starts_with('/');
            let self_closing = trimmed.ends_with('/');
            let name = trimmed
                .trim_start_matches('/')
                .trim_end_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if !name.is_empty() {
                tags.push(Tag {
                    start: index,
                    end,
                    name,
                    closing,
                    self_closing,
                });
            }
        }
        index = end + 1;
    }
    tags
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
    use super::{matching_partner, parse, resolve, PairKind, TextObject};
    use crate::core::document::Document;
    use crate::core::position::CharPos;

    fn range(object: TextObject, text: &str, cursor: usize) -> String {
        let document = Document::from_text(text);
        let range = resolve(&document, CharPos(cursor), object, 1).unwrap();
        document.slice(range)
    }

    #[test]
    fn parses_standard_objects_and_aliases() {
        assert_eq!(
            parse('a', 'w'),
            Some(TextObject::Word {
                around: true,
                big: false
            })
        );
        assert_eq!(
            parse('i', 'W'),
            Some(TextObject::Word {
                around: false,
                big: true
            })
        );
        assert_eq!(
            parse('a', 'b'),
            Some(TextObject::Pair {
                around: true,
                kind: PairKind::Parenthesis
            })
        );
        assert_eq!(
            parse('i', 'B'),
            Some(TextObject::Pair {
                around: false,
                kind: PairKind::Brace
            })
        );
    }

    #[test]
    fn selects_words_with_inner_and_around_ranges() {
        assert_eq!(
            range(
                TextObject::Word {
                    around: false,
                    big: false
                },
                "one two",
                1
            ),
            "one"
        );
        assert_eq!(
            range(
                TextObject::Word {
                    around: true,
                    big: false
                },
                "one two",
                1
            ),
            "one "
        );
    }

    #[test]
    fn selects_nested_pairs() {
        assert_eq!(
            range(
                TextObject::Pair {
                    around: false,
                    kind: PairKind::Parenthesis
                },
                "a(b(c)d)e",
                2
            ),
            "b(c)d"
        );
        assert_eq!(
            range(
                TextObject::Pair {
                    around: true,
                    kind: PairKind::Parenthesis
                },
                "a(b(c)d)e",
                2
            ),
            "(b(c)d)"
        );
    }

    #[test]
    fn selects_quotes_and_tags() {
        assert_eq!(
            range(
                TextObject::Quote {
                    around: false,
                    quote: '"'
                },
                "say \"hello\" now",
                6
            ),
            "hello"
        );
        assert_eq!(
            range(TextObject::Tag { around: true }, "<div><b>x</b></div>", 8),
            "<b>x</b>"
        );
    }

    #[test]
    fn selects_sentence_and_paragraph() {
        assert_eq!(
            range(TextObject::Sentence { around: false }, "One. Two!", 1),
            "One."
        );
        assert_eq!(
            range(
                TextObject::Paragraph { around: false },
                "one\ntwo\n\nthree",
                1
            ),
            "one\ntwo\n"
        );
    }

    #[test]
    fn selects_bracket_angle_and_brace_aliases() {
        let bracket = TextObject::Pair {
            around: true,
            kind: PairKind::Bracket,
        };
        let brace = TextObject::Pair {
            around: false,
            kind: PairKind::Brace,
        };
        let angle = TextObject::Pair {
            around: true,
            kind: PairKind::Angle,
        };
        assert_eq!(range(bracket, "x [one] y", 4), "[one]");
        assert_eq!(range(brace, "x {one} y", 4), "one");
        assert_eq!(range(angle, "x <one> y", 4), "<one>");
    }

    #[test]
    fn finds_matching_brackets_and_quotes() {
        let document = Document::from_text("(a) [b] \"c\" 'd'");
        assert_eq!(matching_partner(&document, CharPos(0)), Some(CharPos(2)));
        assert_eq!(matching_partner(&document, CharPos(2)), Some(CharPos(0)));
        assert_eq!(matching_partner(&document, CharPos(4)), Some(CharPos(6)));
        assert_eq!(matching_partner(&document, CharPos(8)), Some(CharPos(10)));
        assert_eq!(matching_partner(&document, CharPos(12)), Some(CharPos(14)));
    }
}
