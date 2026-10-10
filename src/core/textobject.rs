use super::document::Document;
use super::pairing::{self, PAIR_KINDS};
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

/// Half-width, in characters, of the window [`matching_partner_within`] searches.
///
/// This is the render highlight's give-up distance, and it is deliberately
/// short: the highlight is decoration, its whole cost is proportional to the
/// window, and a few microseconds per frame is worth more than exactness across
/// a scaled document. A 64 KiB window still left an unmatched delimiter costing
/// tenths of a millisecond per frame; 4 KiB makes the frame cost independent of
/// the corpus.
///
/// `%` and the pair text objects keep using the exact unbounded search: there a
/// missing answer would be a behavioural bug rather than a missing decoration,
/// and neither runs on a per-frame path.
pub const PARTNER_SCAN_LIMIT: usize = 1 << 12;

pub fn matching_pair(document: &Document, cursor: CharPos) -> Option<CharRange> {
    matching_pair_within(document, cursor, usize::MAX)
}

/// [`matching_pair`], but scanning at most `limit` characters in either direction
/// from `cursor`. Returns `None` when no pair lies inside the window, which for a
/// bounded `limit` is indistinguishable from there being no pair at all.
pub fn matching_pair_within(
    document: &Document,
    cursor: CharPos,
    limit: usize,
) -> Option<CharRange> {
    if document.is_empty() {
        return None;
    }
    let position = cursor.0.min(document.len_chars() - 1);
    if let Some(kind) = pair_kind_at(document.char_at(CharPos(position))) {
        if let Some((open, close)) =
            pairing::enclosing_pair(document, CharPos(position), kind, limit)
        {
            return Some(CharRange::new(CharPos(open), CharPos(close + 1)));
        }
    }
    for kind in PAIR_KINDS {
        if let Some((open, close)) =
            pairing::next_opening_pair(document, CharPos(position.saturating_add(1)), kind, limit)
        {
            return Some(CharRange::new(CharPos(open), CharPos(close + 1)));
        }
    }
    None
}

/// The exact partner position for the bracket highlight, searching the whole
/// document in both directions.
pub fn matching_partner(document: &Document, cursor: CharPos) -> Option<CharPos> {
    matching_partner_within(document, cursor, usize::MAX)
}

/// [`matching_partner`] restricted to `limit` characters either side of `cursor`.
///
/// Callers that run on a per-frame path should prefer this: see
/// [`PARTNER_SCAN_LIMIT`].
pub fn matching_partner_within(
    document: &Document,
    cursor: CharPos,
    limit: usize,
) -> Option<CharPos> {
    if document.is_empty() {
        return None;
    }
    let position = cursor.0.min(document.len_chars() - 1);
    match document.char_at(CharPos(position)) {
        Some('"' | '\'' | '`') => matching_quote_partner(
            document,
            position,
            document.char_at(CharPos(position))?,
            limit,
        ),
        Some('(' | ')' | '[' | ']' | '{' | '}' | '<' | '>') => {
            let range = matching_pair_within(document, CharPos(position), limit)?;
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

/// The bracket highlight partner for the cursor, found with two bounded
/// walks instead of the enclosing scan plus four opening scans. Equivalent
/// to [`matching_partner_within`] at the same `limit`; the exhaustive test
/// `the_outward_walk_agrees_with_the_bounded_scan_on_every_short_input`
/// holds the two together.
pub fn matching_partner_outward(
    document: &Document,
    cursor: CharPos,
    limit: usize,
) -> Option<CharPos> {
    if document.is_empty() {
        return None;
    }
    let position = cursor.0.min(document.len_chars() - 1);
    let character = document.char_at(CharPos(position))?;
    let (open_character, close_character) = match character {
        '(' | ')' => ('(', ')'),
        '[' | ']' => ('[', ']'),
        '{' | '}' => ('{', '}'),
        '<' | '>' => ('<', '>'),
        '"' | '\'' | '`' => return matching_quote_partner(document, position, character, limit),
        _ => return None,
    };
    let is_open = character == open_character;

    // Backward: to the opener at depth zero. The delimiter under the cursor
    // is handled explicitly: an unescaped opener ends the walk, an
    // unescaped closer is skipped, matching `enclosing_pair_scan`. The
    // escape check needs the backslashes *before* each delimiter, which a
    // backward walk has not seen yet, so this leg asks `is_escaped` (a
    // short walk over the run) rather than carrying a running parity; the
    // forward leg below uses a running parity because it visits in order.
    let mut depth = 0usize;
    let mut open_at = None;
    for (at, c) in document.chars_before(CharPos(position + 1)).take(limit) {
        let index = at.0;
        if c != open_character && c != close_character {
            continue;
        }
        if is_escaped(document, index) {
            continue;
        }
        if index == position {
            if is_open {
                open_at = Some(index);
                break;
            }
            // The closer under the cursor is not counted, matching the scan.
            continue;
        }
        if c == close_character {
            depth += 1;
        } else if depth == 0 {
            open_at = Some(index);
            break;
        } else {
            depth -= 1;
        }
    }
    let open_at = open_at?;

    // Forward: from the opener to its closer.
    let mut depth = 0usize;
    let mut backslashes = 0usize;
    for (at, c) in document.chars_from(CharPos(open_at)).take(limit) {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        let escaped = backslashes % 2 == 1;
        backslashes = 0;
        if escaped {
            continue;
        }
        if c == open_character {
            depth += 1;
        } else if c == close_character {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                // The pair belongs to the cursor only when the cursor sits
                // on one of its ends; a cursor on an escaped delimiter
                // inside a real pair gets no highlight, matching the
                // bounded scan whose fallback result the filter discards.
                if open_at == position {
                    return Some(at);
                } else if at.0 == position {
                    return Some(CharPos(open_at));
                } else {
                    return None;
                }
            }
        }
    }
    None
}

fn matching_quote_partner(
    document: &Document,
    position: usize,
    quote: char,
    limit: usize,
) -> Option<CharPos> {
    let line = document.pos_to_line_col(CharPos(position)).line;
    let start = document.line_start(line).0;
    let end = document.line_end(line).0;
    let mut positions = Vec::new();
    for (at, character) in document.chars_from(CharPos(start)).take(limit) {
        if at.0 >= end {
            break;
        }
        if character == quote && !is_escaped(document, at.0) {
            positions.push(at.0);
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
    for (at, character) in document.chars_from(CharPos(start)) {
        if at.0 >= end {
            break;
        }
        if character == quote && !is_escaped(document, at.0) {
            quote_positions.push(at.0);
        }
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
    // A pair text object has to be exact: a missing or truncated answer would delete
    // or yank the wrong range, so this path searches the whole document.
    const EXACT: usize = usize::MAX;
    let mut pair = pairing::enclosing_pair(document, cursor, kind, EXACT);
    if pair.is_none() {
        let next = pairing::next_opening_pair(document, cursor, kind, EXACT)?;
        pair = Some(next);
    }
    let (mut open, mut close) = pair?;
    for _ in 1..count {
        let previous =
            pairing::enclosing_pair(document, CharPos(open.saturating_sub(1)), kind, EXACT);
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

/// Longest tag body the tag search reads before treating the `<` as literal
/// text. A real tag header is a handful of characters; past this the `<` is
/// ordinary text (a maths comparison, say) and reading further only costs time.
const TAG_BODY_LIMIT: usize = 1024;

fn tag_object(document: &Document, cursor: CharPos, around: bool) -> Option<CharRange> {
    // Lexes and matches in one pass: the old code lexed the whole document
    // into a `Vec<Tag>` first, but matching only ever needs the first pair
    // closed in document order that contains the cursor, so tags after that
    // point are never read.
    let mut stack: Vec<(String, usize)> = Vec::new();
    // Reused across tags: a document is full of them and each is short, so a
    // buffer per tag would mean an allocation per tag.
    let mut body = String::new();
    let mut chars = document.chars_from(CharPos::ZERO);
    while let Some((index, character)) = chars.next() {
        if character != '<' {
            continue;
        }
        // One pass to the closing '>', so a document full of '<' does not
        // rescan the remainder of the file for every one of them.
        body.clear();
        let mut closed = None;
        for (position, inner) in chars.by_ref().take(TAG_BODY_LIMIT) {
            if inner == '>' {
                closed = Some(position);
                break;
            }
            body.push(inner);
        }
        let Some(end) = closed else {
            // A `<` that stays open past the limit is literal text, and the
            // outer loop resumes just after the region already consumed, so the
            // next TAG_BODY_LIMIT characters are not treated as tag starts.
            // Aborting here instead would let one stray `<` hide every tag that
            // follows it.
            continue;
        };
        let trimmed = body.trim();
        if trimmed.is_empty() {
            continue;
        }
        let closing = trimmed.starts_with('/');
        let self_closing = trimmed.ends_with('/');
        let name = trimmed
            .trim_start_matches('/')
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        if closing {
            if let Some(stack_index) = stack.iter().rposition(|(open_name, _)| *open_name == name) {
                let (_, open) = stack.remove(stack_index);
                if cursor.0 >= open && cursor.0 <= end.0 {
                    return Some(if around {
                        CharRange::new(CharPos(open), CharPos(end.0 + 1))
                    } else {
                        CharRange::new(CharPos(open + 1), CharPos(end.0))
                    });
                }
            }
        } else if !self_closing {
            stack.push((name, index.0));
        }
    }
    None
}

fn unit_bounds(document: &Document, position: usize, big: bool) -> (usize, usize) {
    let kind = word_kind(document.char_at(CharPos(position)), big);
    let mut start = position;
    for (at, character) in document.chars_before(CharPos(position + 1)) {
        if word_kind(Some(character), big) != kind {
            break;
        }
        start = at.0;
    }
    let mut end = position + 1;
    for (at, character) in document.chars_from(CharPos(position + 1)) {
        if word_kind(Some(character), big) != kind {
            break;
        }
        end = at.0 + 1;
    }
    (start, end)
}

fn next_unit_start(document: &Document, start: usize, big: bool) -> usize {
    let len = document.len_chars();
    let mut index = start;
    for (at, character) in document.chars_from(CharPos(start)) {
        if word_kind(Some(character), big) == WordKind::Whitespace {
            index = at.0;
            break;
        }
        index = at.0 + 1;
    }
    if index >= len {
        return len;
    }
    for (at, character) in document.chars_from(CharPos(index)) {
        if word_kind(Some(character), big) != WordKind::Whitespace {
            return at.0;
        }
    }
    len
}

fn include_adjacent_whitespace(document: &Document, start: usize, end: usize) -> CharRange {
    let mut new_end = end;
    for (at, character) in document.chars_from(CharPos(end)) {
        if !matches!(character, ' ' | '\t') {
            break;
        }
        new_end = at.0 + 1;
    }
    let mut new_start = start;
    if new_end == end {
        for (at, character) in document.chars_before(CharPos(start)) {
            if !matches!(character, ' ' | '\t') {
                break;
            }
            new_start = at.0;
        }
    }
    CharRange::new(CharPos(new_start), CharPos(new_end))
}

fn sentence_start_containing(document: &Document, position: usize) -> usize {
    // Nearest terminator first: at most one boundary run can straddle
    // `position`, and it must start at the nearest terminator before it,
    // so the first valid boundary met walking backwards is also the last
    // one a forward scan would record.
    for (at, character) in document.chars_before(CharPos(position)) {
        let index = at.0;
        let Some(end) = sentence_boundary_at(document, index, character) else {
            continue;
        };
        if end > position {
            return index;
        }
        return end;
    }
    0
}

fn next_sentence_start(document: &Document, start: usize) -> usize {
    for (at, character) in document.chars_from(CharPos(start)) {
        if sentence_boundary_at(document, at.0, character).is_some() {
            return at.0;
        }
    }
    document.len_chars()
}

fn sentence_end_from(document: &Document, start: usize) -> usize {
    for (at, character) in document.chars_from(CharPos(start)) {
        if let Some(end) = sentence_boundary_at(document, at.0, character) {
            return end;
        }
    }
    document.len_chars()
}

/// True when a sentence terminator at `index` ends a sentence, returning the
/// index just past the terminator and any closing quotes or brackets.
fn sentence_boundary_at(document: &Document, index: usize, character: char) -> Option<usize> {
    if !matches!(character, '.' | '!' | '?') {
        return None;
    }
    let mut end = index + 1;
    for (at, closing) in document.chars_from(CharPos(end)) {
        if !matches!(closing, ')' | ']' | '"' | '\'') {
            break;
        }
        end = at.0 + 1;
    }
    let follows_whitespace = document
        .chars_from(CharPos(end))
        .next()
        .is_some_and(|(_, next)| next.is_whitespace());
    if end == document.len_chars() || follows_whitespace {
        Some(end)
    } else {
        None
    }
}

pub(crate) fn enclosing_pair_scan(
    document: &Document,
    cursor: CharPos,
    kind: PairKind,
    limit: usize,
) -> Option<(usize, usize)> {
    let (open_character, close_character) = delimiters_for(kind);
    let position = cursor.0.min(document.len_chars().saturating_sub(1));
    let mut depth = 0usize;
    let mut open = None;
    for (at, character) in document.chars_before(CharPos(position + 1)).take(limit) {
        let index = at.0;
        if character == close_character && index < position && !is_escaped(document, index) {
            depth += 1;
        } else if character == open_character && !is_escaped(document, index) {
            if depth == 0 {
                open = Some(index);
                break;
            }
            depth -= 1;
        }
    }
    let open = open?;
    let mut depth = 0usize;
    let mut backslashes = 0usize;
    for (at, character) in document.chars_from(CharPos(open)).take(limit) {
        let index = at.0;
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        let escaped = backslashes % 2 == 1;
        backslashes = 0;
        if character == open_character && !escaped {
            depth += 1;
        } else if character == close_character && !escaped {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some((open, index));
            }
        }
    }
    None
}

/// The earliest opening delimiter at or after `cursor` that starts a balanced
/// run.
///
/// One forward pass with a stack. Each opener is pushed; the opener a closer
/// pops is the one it balances, which is the same answer the old two-phase scan
/// gave -- per candidate, depth counted from the candidate's own position. The
/// reported pair is the completed pair with the smallest opener, so an
/// unbalanced delimiter earlier in the window does not hide a balanced pair
/// after it: `"(("` still reports the inner pair. A single depth-tracking pass
/// that answered on the first pop would report the inner pair of `"(())"` too,
/// so the answer is held until the scan ends and the smallest opener wins.
///
/// The scan reaches `2 x limit` because the old form let a pair's closer sit up
/// to `limit` characters past its own opener, and named openers up to `limit`
/// characters past `cursor`.
#[cfg(test)]
fn next_opening_pair_scan_reference(
    document: &Document,
    cursor: CharPos,
    kind: PairKind,
    limit: usize,
) -> Option<(usize, usize)> {
    let (open_character, close_character) = delimiters_for(kind);
    let start = cursor.0.min(document.len_chars());
    for (at, character) in document.chars_from(CharPos(start)).take(limit) {
        let index = at.0;
        if character == open_character && !is_escaped(document, index) {
            if let Some(close) =
                matching_close(document, index, open_character, close_character, limit)
            {
                return Some((index, close));
            }
        }
    }
    None
}

#[cfg(test)]
fn matching_close(
    document: &Document,
    open: usize,
    open_character: char,
    close_character: char,
    limit: usize,
) -> Option<usize> {
    let mut depth = 0usize;
    let mut backslashes = 0usize;
    for (at, character) in document.chars_from(CharPos(open)).take(limit) {
        let index = at.0;
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        let escaped = backslashes % 2 == 1;
        backslashes = 0;
        if character == open_character && !escaped {
            depth += 1;
        } else if character == close_character && !escaped {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

pub(crate) fn next_opening_pair_scan(
    document: &Document,
    cursor: CharPos,
    kind: PairKind,
    limit: usize,
) -> Option<(usize, usize)> {
    let (open_character, close_character) = delimiters_for(kind);
    let start = cursor.0.min(document.len_chars());
    // An opener's pair is reported only if the pair completes inside the window,
    // and the answer is the completed pair with the smallest opener position --
    // exactly what the old two-phase scan returned. The scan reaches 2 x limit
    // because the old form let a pair's closer sit up to `limit` past its own
    // opener.
    let opener_window_end = start.saturating_add(limit);
    let scan_end = start.saturating_add(limit.saturating_mul(2));
    let mut stack: Vec<usize> = Vec::new();
    let mut best: Option<(usize, usize)> = None;
    // Seed the escape parity from the backslash run just before the window: the
    // old scan judged every opener with `is_escaped`, which looks before `start`,
    // and a window that begins mid-run must give its first characters the same
    // status.
    let mut backslashes = 0usize;
    let mut probe = start;
    while probe > 0 {
        probe -= 1;
        if document.char_at(CharPos(probe)) == Some('\\') {
            backslashes += 1;
        } else {
            break;
        }
    }
    for (at, character) in document.chars_from(CharPos(start)).take(scan_end - start) {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        let escaped = backslashes % 2 == 1;
        backslashes = 0;
        if escaped {
            continue;
        }
        if character == open_character {
            stack.push(at.0);
        } else if character == close_character {
            if let Some(open) = stack.pop() {
                if open < opener_window_end
                    && at.0 < open.saturating_add(limit)
                    && best.is_none_or(|(best_open, _)| open < best_open)
                {
                    best = Some((open, at.0));
                }
            }
        }
    }
    best
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

pub(crate) fn delimiters_for(kind: PairKind) -> (char, char) {
    match kind {
        PairKind::Parenthesis => ('(', ')'),
        PairKind::Bracket => ('[', ']'),
        PairKind::Brace => ('{', '}'),
        PairKind::Angle => ('<', '>'),
    }
}

pub(crate) fn is_escaped(document: &Document, index: usize) -> bool {
    // Runs of backslashes are short in real code, so a backward walk one
    // character at a time costs what it costs to read the run, no more.
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
    let start = document.line_start(line);
    let end = document.line_end(line);
    document
        .chars_from(start)
        .take_while(|(position, _)| position.0 < end.0)
        .all(|(_, character)| character.is_whitespace())
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
    use super::{
        matching_pair, matching_partner, matching_partner_outward, matching_partner_within,
        next_opening_pair_scan, next_opening_pair_scan_reference, parse, resolve, PairKind,
        TextObject, PARTNER_SCAN_LIMIT,
    };
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

    #[test]
    fn an_unbalanced_delimiter_does_not_hide_a_balanced_pair_after_it() {
        // The pair is measured from each candidate's own position, so the inner
        // `()` is still found behind the stray `(`. A single depth-tracking walk
        // from the start would report nothing here.
        let document = Document::from_text("(()");
        assert_eq!(
            matching_pair(&document, CharPos(0)).map(|r| r.start.0),
            Some(1)
        );

        let document = Document::from_text("((x)");
        assert_eq!(
            matching_pair(&document, CharPos(0)).map(|r| r.start.0),
            Some(1)
        );

        let document = Document::from_text(")(");
        // No balanced pair at all: the stray `)` opens nothing.
        assert_eq!(matching_pair(&document, CharPos(0)), None);
    }

    #[test]
    fn a_bounded_partner_search_gives_up_past_the_window() {
        let document = Document::from_text("()");
        assert_eq!(
            matching_partner_within(&document, CharPos(0), 64),
            Some(CharPos(1)),
            "a partner inside the window is still found"
        );
        assert_eq!(
            matching_partner_within(&document, CharPos(0), 0),
            None,
            "a zero-width window finds nothing"
        );

        // An opening delimiter separated from its partner by more than the
        // render window: the exact search reports it, a bounded one does not.
        let gap = PARTNER_SCAN_LIMIT + 4096;
        let far = format!("({})", "x".repeat(gap));
        let document = Document::from_text(&far);
        let open = CharPos(0);
        assert_eq!(matching_partner(&document, open), Some(CharPos(gap + 1)));
        assert_eq!(
            matching_partner_within(&document, open, PARTNER_SCAN_LIMIT),
            None
        );
        // A window just wide enough still finds it.
        assert_eq!(
            matching_partner_within(&document, open, gap + 2),
            Some(CharPos(gap + 1))
        );
    }

    #[test]
    fn the_unbounded_partner_search_is_the_default() {
        // `matching_partner` must not have picked up the render path's bound, so
        // it still crosses a gap wider than PARTNER_SCAN_LIMIT.
        let gap = PARTNER_SCAN_LIMIT + 4096;
        let far = format!("({})", "x".repeat(gap));
        let document = Document::from_text(&far);
        assert_eq!(
            matching_partner(&document, CharPos(0)),
            Some(CharPos(gap + 1))
        );
    }

    /// Every document over the given alphabet up to `max_len` characters.
    fn for_each_short_document(
        alphabet: &[char],
        max_len: usize,
        check: &mut impl FnMut(&Document),
    ) {
        fn recurse(
            alphabet: &[char],
            max_len: usize,
            doc: &mut String,
            check: &mut impl FnMut(&Document),
        ) {
            check(&Document::from_text(doc));
            if doc.chars().count() >= max_len {
                return;
            }
            for &c in alphabet {
                doc.push(c);
                recurse(alphabet, max_len, doc, check);
                doc.pop();
            }
        }
        let mut doc = String::new();
        recurse(alphabet, max_len, &mut doc, check);
    }

    #[test]
    fn the_stack_scan_agrees_with_the_reference_at_every_limit() {
        for (alphabet, max_len) in [
            (&['(', ')', '\\', 'a'][..], 6),
            (&['[', ']', '\\', 'a'][..], 6),
            (&['(', ')', '[', ']', '\\'][..], 5),
        ] {
            for_each_short_document(alphabet, max_len, &mut |document| {
                let len = document.len_chars();
                for from in 0..=len {
                    for limit in [1usize, 2, 3, 7, 1000, usize::MAX] {
                        for kind in [
                            PairKind::Parenthesis,
                            PairKind::Bracket,
                            PairKind::Brace,
                            PairKind::Angle,
                        ] {
                            assert_eq!(
                                next_opening_pair_scan(document, CharPos(from), kind, limit),
                                next_opening_pair_scan_reference(
                                    document,
                                    CharPos(from),
                                    kind,
                                    limit
                                ),
                                "from {from}, limit {limit}, kind {kind:?}, text {:?}",
                                document.to_string(),
                            );
                        }
                    }
                }
            });
        }
    }

    #[test]
    fn the_outward_walk_agrees_with_the_bounded_scan_on_every_short_input() {
        for (alphabet, max_len) in [
            (&['(', ')', '\\', 'a'][..], 6),
            (&['[', ']', '\\', 'a'][..], 6),
            (&['{', '}', '(', ')'][..], 6),
            (&['"', 'a', '\\'][..], 6),
            (&['(', ')', '[', ']', '\\'][..], 5),
        ] {
            for_each_short_document(alphabet, max_len, &mut |document| {
                let len = document.len_chars();
                for cursor in 0..=len {
                    for limit in [1usize, 2, 3, 5, 64] {
                        assert_eq!(
                            matching_partner_outward(document, CharPos(cursor), limit),
                            matching_partner_within(document, CharPos(cursor), limit),
                            "cursor {cursor}, limit {limit}, text {:?}",
                            document.to_string(),
                        );
                    }
                }
            });
        }
    }

    #[test]
    fn the_outward_walk_pins_the_closer_cases() {
        // Cursor on a matched closer: the partner is its opener.
        let document = Document::from_text("()");
        assert_eq!(
            matching_partner_outward(&document, CharPos(1), PARTNER_SCAN_LIMIT),
            Some(CharPos(0))
        );
        // Nested: the cursor's own closer belongs to the inner pair.
        let document = Document::from_text("(())");
        assert_eq!(
            matching_partner_outward(&document, CharPos(2), PARTNER_SCAN_LIMIT),
            Some(CharPos(1))
        );
        // An unmatched opener with a balanced pair later: no highlight, same as
        // the bounded scan (its fallback result was always discarded).
        let document = Document::from_text("( a (b)");
        assert_eq!(
            matching_partner_outward(&document, CharPos(0), PARTNER_SCAN_LIMIT),
            None
        );
        // An escaped opener is literal text.
        let document = Document::from_text(r"\(");
        assert_eq!(
            matching_partner_outward(&document, CharPos(1), PARTNER_SCAN_LIMIT),
            None
        );
        // A cursor on an escaped delimiter inside a real pair: the pair does
        // not belong to the cursor, so there is no highlight.
        let document = Document::from_text("(((\\()");
        assert_eq!(
            matching_partner_outward(&document, CharPos(4), PARTNER_SCAN_LIMIT),
            None
        );
    }

    #[test]
    fn the_quote_partner_respects_the_window() {
        let document = Document::from_text("\"a\"xxxx\"b\"");
        // Cursor on the first quote; its partner is inside a small window.
        assert_eq!(
            matching_partner_within(&document, CharPos(0), 16),
            Some(CharPos(2))
        );
        // A zero-width window gives up.
        assert_eq!(matching_partner_within(&document, CharPos(0), 0), None);
    }

    #[test]
    fn a_stray_angle_bracket_does_not_hide_later_tags() {
        let gap = "a".repeat(1024 + 64);
        let document = Document::from_text(&format!("<{gap}<b>x</b>"));
        let cursor = CharPos(document.to_string().find('x').unwrap());
        let range = super::resolve(&document, cursor, TextObject::Tag { around: false }, 1);
        assert!(
            range.is_some(),
            "a stray `<` must not hide the pair after it"
        );
    }
}
