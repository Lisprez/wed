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

/// Stands in for "this opening delimiter has no partner".
const UNMATCHED: usize = usize::MAX;

/// Delimiter pairing for one revision of a document.
///
/// Two parallel arrays per kind: the opening delimiters in ascending order, and
/// for each of them the position of its partner or [`UNMATCHED`]. Parallel
/// arrays rather than a map because the positions are already sorted, so a lookup
/// is a binary search rather than a hash.
pub struct PairIndex {
    opens: [Vec<usize>; PAIR_KINDS.len()],
    close_of: [Vec<usize>; PAIR_KINDS.len()],
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
        let mut opens: [Vec<usize>; PAIR_KINDS.len()] = Default::default();
        let mut close_of: [Vec<usize>; PAIR_KINDS.len()] = Default::default();
        // Openers seen but not yet closed, as indices into `opens`.
        let mut stacks: [Vec<usize>; PAIR_KINDS.len()] = Default::default();

        let mut backslashes = 0usize;
        for (position, character) in document.chars_from(CharPos::ZERO) {
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
                    opens[index].push(position.0);
                    close_of[index].push(UNMATCHED);
                    stacks[index].push(opens[index].len() - 1);
                } else if character == close_character {
                    if let Some(open_index) = stacks[index].pop() {
                        close_of[index][open_index] = position.0;
                    }
                }
            }
        }

        // Growing by doubling leaves up to half the capacity unused, and at four
        // kinds on a delimiter-dense document that slack is measured in megabytes.
        // The vectors are never appended to again, so handing the slack back is
        // strictly worth it.
        for kind in 0..PAIR_KINDS.len() {
            opens[kind].shrink_to_fit();
            close_of[kind].shrink_to_fit();
        }

        Self { opens, close_of }
    }

    /// The innermost pair of `kind` that `position` sits in.
    ///
    /// `close >= position` rather than `>`, so a position resting on a closing
    /// delimiter counts as inside the pair that delimiter ends. The scan agrees:
    /// it counts nesting backwards from the cursor while ignoring the delimiter
    /// under it, which for a closer lands on that closer's own opener.
    pub fn enclosing_at(&self, kind: PairKind, position: usize) -> Option<(usize, usize)> {
        let index = slot(kind);
        let below = self.opens[index].partition_point(|open| *open < position);
        // The pairs containing `position` nest, so the first opener below it whose
        // partner reaches at least this far is the innermost one.
        let mut opener = below;
        while opener > 0 {
            opener -= 1;
            let open = self.opens[index][opener];
            let close = self.close_of[index][opener];
            if close != UNMATCHED && close >= position {
                return Some((open, close));
            }
        }
        None
    }

    /// The pair an opening delimiter at `position` belongs to.
    ///
    /// `None` when the opener has no partner, which is the answer the scan gives
    /// for an unclosed delimiter and the case that made a scan run to end of file.
    pub fn opener_pair_at(&self, kind: PairKind, position: usize) -> Option<(usize, usize)> {
        let index = slot(kind);
        let found = self.opens[index].binary_search(&position).ok()?;
        let close = self.close_of[index][found];
        (close != UNMATCHED).then_some((position, close))
    }

    /// The first pair of `kind` starting at or after `from` whose opener has a
    /// partner.
    ///
    /// Matches the scan it replaces, which also skipped openers that never close
    /// rather than reporting the first opener and giving up.
    pub fn first_balanced_pair(&self, kind: PairKind, from: usize) -> Option<(usize, usize)> {
        let index = slot(kind);
        let mut candidate = self.opens[index].partition_point(|open| *open < from);
        while candidate < self.opens[index].len() {
            let open = self.opens[index][candidate];
            let close = self.close_of[index][candidate];
            if close != UNMATCHED {
                return Some((open, close));
            }
            candidate += 1;
        }
        None
    }

    /// The pairs of every kind, as ranges, for callers that want all of them.
    pub fn pairs(&self, kind: PairKind) -> Vec<CharRange> {
        let index = slot(kind);
        self.opens[index]
            .iter()
            .zip(&self.close_of[index])
            .filter(|(_, close)| **close != UNMATCHED)
            .map(|(open, close)| CharRange::new(CharPos(*open), CharPos(*close + 1)))
            .collect()
    }

    /// Number of opening delimiters recorded for `kind`.
    pub fn open_count(&self, kind: PairKind) -> usize {
        self.opens[slot(kind)].len()
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
    if limit != usize::MAX {
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
    if limit != usize::MAX {
        return next_opening_pair_scan(document, cursor, kind, limit);
    }
    let index = document.pair_index();
    index.first_balanced_pair(kind, cursor.0)
}

#[cfg(test)]
mod tests {
    use super::{PairIndex, PAIR_KINDS, UNMATCHED};
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
        let _ = UNMATCHED;
    }
}
