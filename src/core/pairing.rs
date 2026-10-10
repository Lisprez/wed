//! Delimiter pairing, precomputed.
//!
//! `#` is O(distance), where `d` is the distance to the partner. On a large
//! document that is the whole remaining file: on a 50 MiB document with no
//! delimiter of the searched kind, `%` measured 220 ms, and a pair text object
//! such as `i(` measured 86 ms. Both are single keypresses.
//!
//! One pass over the text, matching each bracket kind with its own stack, answers
//! all of it in constant time afterwards. The pass itself is linear and costs
//! roughly what loading the file does, so the index pays for itself after the
//! first query in any document where the pairing is asked for more than once --
//! hopping through a file's pairs with `%`, or reaching for several pair text
//! objects in a row.
//!
//! # Staleness
//!
//! The index is cached against [`Document::mutation`] and rebuilt when that
//! changes. It is never updated in place: any edit can re-pair an unbounded
//! region, so there is no cheap incremental update, and a stale answer is worse
//! than a slow one.
//!
//! # Escapes
//!
//! A delimiter preceded by an odd number of backslashes is literal text. The pass
//! tracks that with the same running parity the scan paths use, so the two agree
//! on `\"` and `\\(`.
//!
//! # Storage
//!
//! Per kind, `pairs` holds every *matched* pair as `(open, close)` sorted by
//! `open`, and `parents` holds for each entry the index of the innermost pair
//! containing it. Openers that never found a partner are not stored at all,
//! which is what lets the queries skip them in O(log n) instead of walking a
//! run of dead openers. Only matched pairs take space: 12 bytes a pair (two
//! u32 positions plus a u32 parent index).
//!
//! Measured on the bracket-dense 50 MiB corpus, which holds 6,765,008 matched
//! pairs: 77 MiB, against 103 MiB for the two `Vec<usize>` this replaces. An
//! earlier plan called for 51 MiB from two u32 arrays with no parent index; the
//! extra 26 MiB buys the parent chain, which is what turns `enclosing_at` from a
//! walk over a run of dead openers into O(log + nesting depth).
//!
//! # Size limit
//!
//! Positions are u32, so the index covers documents under 4 Gi characters.
//! Beyond that the build panics; callers guard on document length first and
//! fall back to the scans, which answer the same questions without the index.

use super::document::Document;
use super::position::{CharPos, CharRange};
use super::textobject::{delimiters_for, next_opening_pair_scan, PairKind};

/// The bracket kinds, in the order `matching_pair` prefers them.
///
/// A pair of the higher-priority kind wins even when a lower-priority pair starts
/// earlier in the text, so this order is part of the behaviour and not just an
/// implementation detail.
pub const PAIR_KINDS: [PairKind; 4] = [
    PairKind::Parenthesis,
    PairKind::Bracket,
    PairKind::Brace,
    PairKind::Angle,
];

/// Stands in for "this pair has no containing pair".
const NO_PARENT: u32 = u32::MAX;

/// Delimiter pairing for one revision of a document.
///
/// Per kind, `pairs` holds every *matched* pair as `(open, close)` sorted by
/// `open`, and `parents` holds for each entry the index of the innermost pair
/// containing it. Openers that never found a partner are not stored at
/// all, which is what makes the queries below skip them in O(log n).
/// Positions are u32: the index supports documents under 4 Gi characters
/// (callers fall back to the scans beyond that) and halves the memory
/// against usize. The parent link costs 4 bytes per pair and buys an
/// O(log + nesting depth) `enclosing_at`.
pub struct PairIndex {
    pairs: [Vec<(u32, u32)>; PAIR_KINDS.len()],
    parents: [Vec<u32>; PAIR_KINDS.len()],
    open_counts: [usize; PAIR_KINDS.len()],
}

fn slot(kind: PairKind) -> usize {
    PAIR_KINDS
        .iter()
        .position(|k| *k == kind)
        .expect("every kind is listed")
}

impl PairIndex {
    /// Matches every delimiter in `document` in one pass.
    ///
    /// Each kind keeps its own stack, so a parenthesis cannot affect a brace and a
    /// document that mixes them -- or is not balanced at all -- still pairs up
    /// correctly for the parts that are.
    pub fn build(document: &Document) -> Self {
        assert!(
            document.len_chars() < u32::MAX as usize,
            "the pair index supports documents under 4 Gi characters; \
             callers fall back to the scans beyond that"
        );
        let mut pairs: [Vec<(u32, u32)>; PAIR_KINDS.len()] = Default::default();
        // Openers seen but not yet closed, as indices into `pairs`.
        let mut stacks: [Vec<u32>; PAIR_KINDS.len()] = Default::default();
        let mut open_counts = [0usize; PAIR_KINDS.len()];

        let mut backslashes = 0usize;
        let mut chunk_byte_start = 0usize;
        let mut chunk_char_start = 0usize;
        for chunk in document.rope().chunks() {
            if chunk.is_ascii() {
                // ASCII bytes map one-to-one onto chars, and no multi-byte
                // UTF-8 sequence can contain a delimiter byte, so a byte scan
                // sees exactly the delimiter chars a char scan would.
                for (offset, byte) in chunk.as_bytes().iter().enumerate() {
                    let position = chunk_char_start + offset;
                    match *byte {
                        b'\\' => backslashes += 1,
                        byte => {
                            let escaped = backslashes % 2 == 1;
                            backslashes = 0;
                            if escaped {
                                continue;
                            }
                            for (index, kind) in PAIR_KINDS.iter().enumerate() {
                                let (open_character, close_character) = delimiters_for(*kind);
                                if byte == open_character as u8 {
                                    // A close of u32::MAX marks "no partner
                                    // yet"; the entry is dropped below.
                                    pairs[index].push((position as u32, u32::MAX));
                                    stacks[index].push((pairs[index].len() - 1) as u32);
                                    open_counts[index] += 1;
                                } else if byte == close_character as u8 {
                                    if let Some(pair) = stacks[index].pop() {
                                        pairs[index][pair as usize].1 = position as u32;
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                for (offset, character) in chunk.chars().enumerate() {
                    let position = chunk_char_start + offset;
                    if character == '\\' {
                        backslashes += 1;
                        continue;
                    }
                    let escaped = backslashes % 2 == 1;
                    backslashes = 0;
                    if escaped {
                        continue;
                    }
                    for (index, kind) in PAIR_KINDS.iter().enumerate() {
                        let (open_character, close_character) = delimiters_for(*kind);
                        if character == open_character {
                            pairs[index].push((position as u32, u32::MAX));
                            stacks[index].push((pairs[index].len() - 1) as u32);
                            open_counts[index] += 1;
                        } else if character == close_character {
                            if let Some(pair) = stacks[index].pop() {
                                pairs[index][pair as usize].1 = position as u32;
                            }
                        }
                    }
                }
            }
            chunk_byte_start += chunk.len();
            chunk_char_start = document.rope().byte_to_char(chunk_byte_start);
        }

        let mut parents: [Vec<u32>; PAIR_KINDS.len()] = Default::default();
        for kind in 0..PAIR_KINDS.len() {
            // Drop openers that never found a partner; their absence is what
            // makes the queries below skip them.
            pairs[kind].retain(|(_, close)| *close != u32::MAX);
            let mut stack: Vec<u32> = Vec::new();
            parents[kind] = Vec::with_capacity(pairs[kind].len());
            for i in 0..pairs[kind].len() {
                let open = pairs[kind][i].0;
                while let Some(&top) = stack.last() {
                    if pairs[kind][top as usize].1 < open {
                        stack.pop();
                    } else {
                        break;
                    }
                }
                parents[kind].push(stack.last().copied().unwrap_or(NO_PARENT));
                stack.push(i as u32);
            }
            // Growing by doubling leaves up to half the capacity unused, and at
            // four kinds on a delimiter-dense document that slack is measured
            // in megabytes. The vectors are never appended to again, so handing
            // the slack back is strictly worth it.
            pairs[kind].shrink_to_fit();
            parents[kind].shrink_to_fit();
        }

        Self {
            pairs,
            parents,
            open_counts,
        }
    }

    /// The innermost pair of `kind` that `position` sits in.
    ///
    /// `close >= position` rather than `>`, so a position resting on a closing
    /// delimiter counts as inside the pair that delimiter ends. The scan agrees:
    /// it counts nesting backwards from the cursor while ignoring the delimiter
    /// under it, which for a closer lands on that closer's own opener.
    pub fn enclosing_at(&self, kind: PairKind, position: usize) -> Option<(usize, usize)> {
        let index = slot(kind);
        let pairs = &self.pairs[index];
        let mut candidate = pairs.partition_point(|(open, _)| (*open as usize) < position);
        while candidate > 0 {
            candidate -= 1;
            let (open, close) = pairs[candidate];
            if close as usize >= position {
                return Some((open as usize, close as usize));
            }
            let parent = self.parents[index][candidate];
            if parent == NO_PARENT {
                return None;
            }
            // `parent + 1`, because the loop decrements before it reads.
            candidate = parent as usize + 1;
        }
        None
    }

    /// The pair an opening delimiter at `position` belongs to.
    ///
    /// `None` when the opener has no partner, which is the answer the scan gives
    /// for an unclosed delimiter and the case that made a scan run to end of file.
    pub fn opener_pair_at(&self, kind: PairKind, position: usize) -> Option<(usize, usize)> {
        let index = slot(kind);
        let key = u32::try_from(position).ok()?;
        let found = self.pairs[index]
            .binary_search_by_key(&key, |(open, _)| *open)
            .ok()?;
        let (open, close) = self.pairs[index][found];
        Some((open as usize, close as usize))
    }

    /// The first pair of `kind` starting at or after `from`.
    ///
    /// Unmatched openers are not in the table at all, so this is a single
    /// partition point rather than a walk over a run of dead openers.
    pub fn first_balanced_pair(&self, kind: PairKind, from: usize) -> Option<(usize, usize)> {
        let index = slot(kind);
        let candidate = self.pairs[index].partition_point(|(open, _)| (*open as usize) < from);
        let (open, close) = *self.pairs[index].get(candidate)?;
        Some((open as usize, close as usize))
    }

    /// The pairs of every kind, as ranges, for callers that want all of them.
    pub fn pairs(&self, kind: PairKind) -> Vec<CharRange> {
        let index = slot(kind);
        self.pairs[index]
            .iter()
            .map(|(open, close)| {
                CharRange::new(CharPos(*open as usize), CharPos(*close as usize + 1))
            })
            .collect()
    }

    /// Number of opening delimiters seen, matched or not.
    pub fn open_count(&self, kind: PairKind) -> usize {
        self.open_counts[slot(kind)]
    }
}

/// True when `document` has a live `kind` opening delimiter at `position`.
///
/// Escape-aware: a backslashed delimiter is literal text, and routing one here
/// would ask the index for a pair that by definition does not exist instead of
/// letting the enclosing-pair query answer.
pub fn is_opener_at(document: &Document, position: CharPos, kind: PairKind) -> bool {
    let (open_character, _) = delimiters_for(kind);
    document.char_at(position) == Some(open_character)
        && !super::textobject::is_escaped(document, position.0)
}

/// The innermost pair of `kind` enclosing `cursor`, using the index when the
/// query is unbounded.
///
/// A bounded query keeps the scan: it costs at most `limit` characters and needs
/// no index, which is what keeps the render path cheap. That path runs after every
/// keystroke, and an index rebuilt that often would cost far more than the bound
/// saves.
pub fn enclosing_pair(
    document: &Document,
    cursor: CharPos,
    kind: PairKind,
    limit: usize,
) -> Option<(usize, usize)> {
    if limit != usize::MAX || document.len_chars() >= u32::MAX as usize {
        return super::textobject::enclosing_pair_scan(document, cursor, kind, limit);
    }
    let index = document.pair_index();
    // A cursor resting on an opening delimiter belongs to that delimiter's own
    // pair. Asking for it directly is what keeps an unclosed opener from costing a
    // scan to end of file, which was the worst case the index was built for.
    if is_opener_at(document, cursor, kind) {
        return index.opener_pair_at(kind, cursor.0);
    }
    index.enclosing_at(kind, cursor.0)
}

/// The first balanced pair of `kind` at or after `cursor`, using the index when
/// the query is unbounded. See [`enclosing_pair`] for why a bounded query does not.
pub fn next_opening_pair(
    document: &Document,
    cursor: CharPos,
    kind: PairKind,
    limit: usize,
) -> Option<(usize, usize)> {
    if limit != usize::MAX || document.len_chars() >= u32::MAX as usize {
        return next_opening_pair_scan(document, cursor, kind, limit);
    }
    let index = document.pair_index();
    index.first_balanced_pair(kind, cursor.0)
}

#[cfg(test)]
mod tests {
    use super::{PairIndex, PAIR_KINDS};
    use crate::core::document::Document;
    use crate::core::position::CharPos;
    use crate::core::textobject::{enclosing_pair_scan, next_opening_pair_scan, PairKind};

    /// A deterministic generator, so a failure can be reproduced exactly.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut value = self.0;
            value ^= value >> 12;
            value ^= value << 25;
            value ^= value >> 27;
            self.0 = value;
            value.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }
    }

    /// Text drawn from the delimiters, escapes and noise that make pairing hard.
    fn random_document(seed: u64, length: usize) -> String {
        const PIECES: [char; 15] = [
            '(', ')', '[', ']', '{', '}', '<', '>', 'a', ' ', '\\', '"', 'q', '\n', ',',
        ];
        let mut random = Rng(seed.max(1) | 1);
        let mut text = String::with_capacity(length);
        while text.len() < length {
            text.push(PIECES[random.below(PIECES.len())]);
        }
        text
    }

    /// The index-backed queries must be indistinguishable from the scans.
    ///
    /// This is the safety net that matters: the index exists only to answer the
    /// same question faster, so any disagreement is a behaviour change. It
    /// compares the *dispatch* -- `pairing::enclosing_pair` and
    /// `pairing::next_opening_pair`, which pick between the two -- against the
    /// scans directly, because that is what callers actually get.
    ///
    /// Unbalanced input is exactly where pairing code disagrees with itself, so
    /// the corpus leans hard on openers that never close, closers that never
    /// open, and backslashes in front of delimiters.
    #[test]
    fn the_index_agrees_with_the_scan_on_random_input() {
        // Kept short deliberately: every position is probed against a scan that is
        // itself linear in the document, so this is quadratic per round and was
        // five seconds of the test suite at triple the size.
        for round in 0..150 {
            let text = random_document(0x9e37_79b9_7f4a_7c15 ^ (round as u64 * 0x1000_0001), 80);
            let document = Document::from_text(&text);
            let length = document.len_chars();
            for kind in PAIR_KINDS {
                for from in 0..=length {
                    let expected =
                        next_opening_pair_scan(&document, CharPos(from), kind, usize::MAX);
                    let actual =
                        super::next_opening_pair(&document, CharPos(from), kind, usize::MAX);
                    assert_eq!(
                        actual, expected,
                        "next_opening_pair disagreed for {kind:?} from {from} in {text:?}"
                    );
                }
                for position in 0..length {
                    let expected =
                        enclosing_pair_scan(&document, CharPos(position), kind, usize::MAX);
                    let actual =
                        super::enclosing_pair(&document, CharPos(position), kind, usize::MAX);
                    assert_eq!(
                        actual, expected,
                        "enclosing_pair disagreed for {kind:?} at {position} in {text:?}"
                    );
                }
            }
        }
    }

    /// An edit must invalidate the cached pairing.
    #[test]
    fn the_cache_is_rebuilt_after_an_edit() {
        use crate::core::position::CharRange;
        let mut document = Document::from_text("(a)");
        assert_eq!(
            super::next_opening_pair(&document, CharPos(0), PairKind::Parenthesis, usize::MAX),
            Some((0, 2))
        );

        // Delete the closing delimiter, so the opener no longer has a partner.
        document.overwrite_range(CharRange::new(CharPos(2), CharPos(3)), "");
        assert_eq!(
            super::next_opening_pair(&document, CharPos(0), PairKind::Parenthesis, usize::MAX),
            None,
            "the cached index must not survive an edit"
        );

        // Put one back.
        document.overwrite_range(CharRange::new(CharPos(2), CharPos(2)), ")");
        assert_eq!(
            super::next_opening_pair(&document, CharPos(0), PairKind::Parenthesis, usize::MAX),
            Some((0, 2))
        );
    }

    #[test]
    fn escaped_delimiters_are_literal() {
        let document = Document::from_text(r#"\(x)"#);
        let index = PairIndex::build(&document);
        // The `(` is escaped, so the only pair present is `(`...`)` at 2 and 4.
        // `\(x)` is four characters: backslash, parenthesis, x, closing. The
        // parenthesis is literal, so the closing one is unmatched and there is no
        // pair at all -- not a pair that merely starts later.
        assert_eq!(index.first_balanced_pair(PairKind::Parenthesis, 0), None);

        // Two backslashes make the parenthesis live again.
        let document = Document::from_text(r#"\\(x)"#);
        let index = PairIndex::build(&document);
        assert_eq!(
            index.first_balanced_pair(PairKind::Parenthesis, 0),
            Some((2, 4))
        );
    }

    #[test]
    fn unmatched_openers_are_recorded_as_having_no_partner() {
        let document = Document::from_text("(()");
        let index = PairIndex::build(&document);
        assert_eq!(index.open_count(PairKind::Parenthesis), 2);
        assert_eq!(
            index.first_balanced_pair(PairKind::Parenthesis, 0),
            Some((1, 2))
        );
        assert_eq!(index.first_balanced_pair(PairKind::Parenthesis, 2), None);
        assert_eq!(index.enclosing_at(PairKind::Parenthesis, 0), None);
        assert_eq!(index.enclosing_at(PairKind::Parenthesis, 1), None);
        assert_eq!(index.enclosing_at(PairKind::Parenthesis, 2), Some((1, 2)));
    }

    #[test]
    fn nested_pairs_are_nested() {
        let document = Document::from_text("(a(b)c)");
        let index = PairIndex::build(&document);
        assert_eq!(index.enclosing_at(PairKind::Parenthesis, 3), Some((2, 4)));
        assert_eq!(index.enclosing_at(PairKind::Parenthesis, 0), None);
        assert_eq!(index.enclosing_at(PairKind::Parenthesis, 5), Some((0, 6)));
        // A position on a closing delimiter is inside the pair it ends.
        assert_eq!(index.enclosing_at(PairKind::Parenthesis, 4), Some((2, 4)));
        assert_eq!(index.enclosing_at(PairKind::Parenthesis, 6), Some((0, 6)));
    }

    #[test]
    fn kinds_do_not_interfere() {
        let document = Document::from_text("([x])");
        let index = PairIndex::build(&document);
        assert_eq!(
            index.first_balanced_pair(PairKind::Parenthesis, 0),
            Some((0, 4))
        );
        assert_eq!(
            index.first_balanced_pair(PairKind::Bracket, 0),
            Some((1, 3))
        );
        assert_eq!(index.first_balanced_pair(PairKind::Brace, 0), None);
    }

    #[test]
    fn an_empty_document_has_no_pairs() {
        let document = Document::from_text("");
        let index = PairIndex::build(&document);
        for kind in PAIR_KINDS {
            assert_eq!(index.first_balanced_pair(kind, 0), None);
        }
    }

    #[test]
    fn every_matched_opener_has_a_partner_after_it() {
        let text = random_document(0x1234_5678, 400);
        let document = Document::from_text(&text);
        let index = PairIndex::build(&document);
        for kind in PAIR_KINDS {
            for pair in index.pairs(kind) {
                assert!(
                    pair.start.0 < pair.end.0,
                    "{kind:?} pair {pair:?} is not ordered"
                );
            }
        }
    }
}
