//! Terminal cell width for a character.
//!
//! `render.rs` needs to know how many terminal cells a character covers so it
//! can advance columns, place the cursor, and clip at the viewport edge. Most
//! characters are one cell; the exceptions are East Asian ideographs and
//! emoji, which take two, and combining marks and zero-width formatting
//! characters, which take none.
//!
//! The tables below are a deliberate subset rather than a full Unicode
//! implementation, which would mean a dependency and a data file. They follow
//! the East Asian Width property for the blocks that actually appear in source
//! code and prose, plus the emoji blocks terminals render double-width by
//! default. A character in none of the ranges is one cell, which is the correct
//! answer for the overwhelming majority of text.
//!
//! Both tables must stay sorted by start and non-overlapping, since lookup is a
//! binary search. [`tests::tables_are_sorted`] enforces that.

/// Terminal cells occupied by a tab, given the column it starts at.
pub const TAB_STOP: usize = 8;

/// Characters that occupy no cell.
///
/// Combining marks overlay the glyph before them and the zero-width formatting
/// characters occupy nothing at all. Counting them as one cell each is what
/// makes a line of them shift every following column out of alignment.
const ZERO_WIDTH: [(u32, u32); 26] = [
    (0x0300, 0x036F), // combining diacritical marks
    (0x0483, 0x0489), // Cyrillic combining marks
    (0x0591, 0x05BD), // Hebrew accents
    (0x05BF, 0x05BF),
    (0x05C1, 0x05C2),
    (0x0610, 0x061A), // Arabic marks
    (0x064B, 0x065F),
    (0x0670, 0x0670),
    (0x06D6, 0x06DC), // Arabic small high marks
    (0x0730, 0x074A), // Syriac marks
    (0x07A6, 0x07B0),
    (0x0900, 0x0903), // Devanagari signs
    (0x093A, 0x094F),
    (0x0951, 0x0957),
    (0x0E31, 0x0E31), // Thai vowel signs
    (0x0E34, 0x0E3A),
    (0x0E47, 0x0E4E),
    (0x1AB0, 0x1AFF),   // combining diacritical marks extended
    (0x1DC0, 0x1DFF),   // combining diacritical marks supplement
    (0x200B, 0x200F),   // zero-width space, joiners, marks, bidi controls
    (0x20D0, 0x20FF),   // combining marks for symbols
    (0xFE00, 0xFE0F),   // variation selectors
    (0xFEFF, 0xFEFF),   // zero-width no-break space
    (0x1D167, 0x1D169), // musical symbols
    (0x1D17B, 0x1D182),
    (0xE0100, 0xE01EF), // variation selectors supplement
];

/// Characters that occupy two cells.
///
/// CJK ideographs, kana, Hangul, fullwidth forms, and the emoji blocks that
/// terminals render double-width by default. A two-cell glyph is why the
/// viewport has to clip by cell rather than by character: half of one glyph
/// has no meaning, and drawing it would leave every later column misaligned.
const WIDE: [(u32, u32); 75] = [
    (0x1100, 0x115F), // Hangul Jamo initial consonants
    (0x2329, 0x232A), // angle brackets, rendered wide by convention
    (0x2E80, 0x2E99), // CJK radicals supplement
    (0x2E9B, 0x2EF3),
    (0x2F00, 0x2FD5), // Kangxi radicals
    (0x2FF0, 0x2FFB), // ideographic description characters
    (0x3000, 0x303E), // CJK symbols and punctuation
    (0x3041, 0x3096), // Hiragana
    (0x3099, 0x30FF), // combining marks, Katakana
    (0x3105, 0x312F), // Bopomofo
    (0x3131, 0x318E), // Hangul compatibility Jamo
    (0x3190, 0x31E3), // kanbun, CJK strokes, Katakana extensions
    (0x31F0, 0x321E), // Katakana phonetic extensions, enclosed CJK
    (0x3220, 0x3247), // enclosed CJK letters and months
    (0x3250, 0x4DBF), // enclosed CJK, CJK extension A
    (0x4E00, 0x9FFF), // CJK unified ideographs
    (0xA000, 0xA48C), // Yi syllables
    (0xA490, 0xA4C6),
    (0xA960, 0xA97C), // Hangul Jamo extended-A
    (0xAC00, 0xD7A3), // Hangul syllables
    (0xF900, 0xFAFF), // CJK compatibility ideographs
    (0xFE10, 0xFE19), // vertical forms
    (0xFE30, 0xFE52), // CJK compatibility forms
    (0xFE54, 0xFE66),
    (0xFE68, 0xFE6B),
    (0xFF01, 0xFF60),   // fullwidth forms
    (0xFFE0, 0xFFE6),   // fullwidth signs
    (0x16FE0, 0x16FE4), // ideographic symbols and punctuation
    (0x17000, 0x187F7), // Tangut
    (0x18800, 0x18CD5), // Tangut components
    (0x1B000, 0x1B001), // Kana supplement
    (0x1B150, 0x1B152), // small kana extension
    (0x1B164, 0x1B167),
    (0x1B170, 0x1B2FB), // Nushu, Kana extended-B
    (0x1F004, 0x1F004), // mahjong red dragon
    (0x1F0CF, 0x1F0CF), // playing card black joker
    (0x1F18E, 0x1F18E), // negative squared AB
    (0x1F191, 0x1F19A), // squared letters and symbols
    (0x1F200, 0x1F320), // enclosed ideographic supplement, emoji
    (0x1F32D, 0x1F335),
    (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393),
    (0x1F3A0, 0x1F3CA),
    (0x1F3CF, 0x1F3D3),
    (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4),
    (0x1F3F8, 0x1F43E),
    (0x1F440, 0x1F440),
    (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D),
    (0x1F54B, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A4),
    (0x1F5FB, 0x1F64F), // emoji
    (0x1F680, 0x1F6C5),
    (0x1F6CC, 0x1F6CC),
    (0x1F6D0, 0x1F6D2),
    (0x1F6D5, 0x1F6D7),
    (0x1F6EB, 0x1F6EC),
    (0x1F6F4, 0x1F6FC),
    (0x1F7E0, 0x1F7EB),
    (0x1F90C, 0x1F93A),
    (0x1F93C, 0x1F945),
    (0x1F947, 0x1F9FF),
    (0x1FA70, 0x1FA7C),
    (0x1FA80, 0x1FA88),
    (0x1FA90, 0x1FABD),
    (0x1FABF, 0x1FAC5),
    (0x1FACE, 0x1FADB),
    (0x1FAE0, 0x1FAE8),
    (0x1FAF0, 0x1FAF8),
    (0x20000, 0x2FFFD), // CJK unified ideographs extension B and onward
    (0x30000, 0x3FFFD),
];

/// Cells occupied by `character` when it starts at `column`.
///
/// A tab advances to the next tab stop. Everything else is zero, one, or two
/// cells according to the tables above.
pub fn width(character: char, column: usize) -> usize {
    if character == '\t' {
        return TAB_STOP - (column % TAB_STOP);
    }
    let code = character as u32;
    if in_ranges(&ZERO_WIDTH, code) {
        0
    } else if in_ranges(&WIDE, code) {
        2
    } else {
        1
    }
}

/// Binary search over a table of ascending, non-overlapping inclusive ranges.
fn in_ranges(ranges: &[(u32, u32)], code: u32) -> bool {
    ranges
        .binary_search_by(|(start, end)| {
            if code < *start {
                std::cmp::Ordering::Greater
            } else if code > *end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::{in_ranges, width, TAB_STOP, WIDE, ZERO_WIDTH};

    #[test]
    fn tables_are_sorted_and_disjoint() {
        for table in [ZERO_WIDTH.as_slice(), WIDE.as_slice()] {
            for pair in table.windows(2) {
                assert!(
                    pair[0].1 < pair[1].0,
                    "ranges must be ascending and non-overlapping: {:?} then {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    #[test]
    fn ordinary_characters_are_one_cell() {
        for character in ['a', 'Z', '0', ' ', '.', ',', '(', ')', '{', '}', '\'', '"'] {
            assert_eq!(width(character, 0), 1, "{character:?} should be one cell");
        }
        // Control characters are drawn as a placeholder, so they take one cell
        // like the glyph that replaces them.
        assert_eq!(width('\u{1}', 0), 1);
    }

    #[test]
    fn cjk_and_fullwidth_forms_are_two_cells() {
        for character in [
            '\u{4e16}',  // 世 CJK unified ideograph
            '\u{754c}',  // 界
            '\u{7f16}',  // 編
            '\u{3042}',  // あ Hiragana
            '\u{30a2}',  // ア Katakana
            '\u{ac00}',  // 가 Hangul syllable
            '\u{ff21}',  // Ａ fullwidth A
            '\u{3000}',  // ideographic space
            '\u{1f600}', // emoji
        ] {
            assert_eq!(
                width(character, 0),
                2,
                "U+{:04X} should be two cells",
                character as u32
            );
        }
    }

    #[test]
    fn combining_marks_are_zero_cells() {
        for character in ['\u{0301}', '\u{20d0}', '\u{200b}', '\u{fe0f}', '\u{feff}'] {
            assert_eq!(
                width(character, 0),
                0,
                "U+{:04X} should be zero cells",
                character as u32
            );
        }
    }

    #[test]
    fn a_tab_advances_to_the_next_tab_stop() {
        assert_eq!(width('\t', 0), TAB_STOP);
        assert_eq!(width('\t', 3), 5);
        assert_eq!(width('\t', 7), 1);
        assert_eq!(width('\t', 8), TAB_STOP);
    }

    #[test]
    fn range_edges_are_inclusive() {
        assert!(in_ranges(&WIDE, 0x4e00));
        assert!(in_ranges(&WIDE, 0x9fff));
        // 0x2FFF falls in the gap between the ideographic description
        // characters and the CJK symbols block.
        assert!(!in_ranges(&WIDE, 0x2fff));
        // 0xFF00 falls in the gap before the fullwidth forms.
        assert!(!in_ranges(&WIDE, 0xff00));
        assert!(in_ranges(&WIDE, 0xff01));
    }
}
