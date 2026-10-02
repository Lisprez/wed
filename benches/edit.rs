//! Core editing benchmarks against the `wed` library.
//!
//! Covers the cases listed in the performance targets of
//! `docs/tiny-neovim-development-plan.md` section 8 that live below the
//! terminal layer: load, save, insert, delete, cursor motion, line jump,
//! text objects, and search.
//!
//! Run with `cargo bench --bench edit`. Set `WED_BENCH_SCALE=full` to add the
//! 50 MiB corpora (the default keeps 1 MiB so CI stays fast).
//! Viewport painting is measured separately, in `benches/render.rs`.

#[path = "harness/mod.rs"]
mod harness;

use harness::{measure, mib, report, section, Corpus, Rng, Scale, TempFile};
use wed::core::{
    matching_partner, resolve_text_object, CharPos, Document, Editor, Key, Mode, PairKind,
    Selection, SelectionKind, TextObject,
};

/// The bracket index, and whether building it pays for itself.
///
/// Building the index is one pass over the document, so it can only be a win if
/// the pairing is asked for more than once between edits. This measures both
/// halves: the build on its own, and a run of queries once it is built.
fn bench_pair_index(corpus: Corpus, text: &str) {
    let document = wed::core::Document::from_text(text);

    // The build alone: the cache is dropped first so each sample pays for it.
    report(&measure(
        format!("pairing: index build over a {} corpus", corpus.label()),
        || {
            document.invalidate_pair_index();
            std::hint::black_box(
                document
                    .pair_index()
                    .open_count(wed::core::PairKind::Parenthesis),
            );
        },
    ));

    // A run of queries against an already-built index. The cursor is moved by
    // hand each time so the queries are not all identical, which is what hopping
    // through a file's pairs with `%` looks like.
    let mut editor = Editor::from_text(text);
    let mut positions: Vec<usize> = Vec::new();
    for offset in 0..20 {
        positions.push((offset * 977) % editor.document.len_chars().max(1));
    }
    let mut next = 0usize;
    // Warm the index so the measured run does not include the build.
    editor.handle_key(Key::Char('%'));
    report(&measure(
        format!("pairing: 20x % on a warm index ({})", corpus.label()),
        || {
            for _ in 0..20 {
                editor.cursor = CharPos(positions[next % positions.len()]);
                next += 1;
                editor.handle_key(Key::Char('%'));
            }
        },
    ));
}

/// `:s` with a pattern, which unlike the literal form reads the document whole.
///
/// The text is released once the match positions are known and the result is
/// built by editing rather than by concatenating, so the peak is the rope plus one
/// copy of the text.
fn bench_regex_substitute(corpus: Corpus, text: &str) {
    let Some(pattern) = common_word(text) else {
        println!("  (no common word; skipping)");
        return;
    };
    let command = format!("s/{pattern}/zz/g");
    let substitution = match wed::core::Substitution::parse(&command) {
        Ok(substitution) => substitution,
        Err(error) => {
            println!("  (could not build the substitution: {error})");
            return;
        }
    };
    let mut editor = Editor::from_text(text);
    report(&measure(
        format!(
            "substitute: regex s/{pattern}/zz/g over a {} corpus",
            corpus.label()
        ),
        || editor.substitute(&substitution),
    ));
}

fn main() {
    let scale = Scale::from_env();
    println!("wed core benchmarks");
    println!("scale: {scale:?} (set WED_BENCH_SCALE=full for the 50 MiB corpora)");

    let mut sizes = vec![mib(1)];
    if scale == Scale::Full {
        sizes.push(mib(50));
    }

    for target_bytes in sizes {
        for corpus in Corpus::ALL {
            // Generated outside every timed region: building the input must
            // never land inside a reported sample.
            let text = corpus.build(target_bytes, 0x5eed_1234_abcd_0001);
            run_corpus(corpus, target_bytes, &text);
        }
    }
}

fn run_corpus(corpus: Corpus, target_bytes: usize, text: &str) {
    section(&format!(
        "{} / {} MiB",
        corpus.label(),
        target_bytes / (1024 * 1024)
    ));
    bench_load(corpus, text);
    bench_save(text);
    bench_insert(corpus, text);
    bench_delete(corpus, text);
    bench_motion(corpus, text);
    bench_goto(corpus, text);
    bench_text_objects(corpus, text);
    bench_selection_ranges(text);
    bench_matching_partner(corpus, text);
    bench_matching_pair_motion(corpus, text);
    bench_pair_index(corpus, text);
    bench_search(text);
    bench_search_from_deep(text);
    bench_replace_literal(corpus, text);
    bench_regex_substitute(corpus, text);
}

/// `Editor::from_text`, i.e. reading a file and indexing it into the rope.
fn bench_load(corpus: Corpus, text: &str) {
    report(&measure(
        format!("load: build editor from {} corpus", corpus.label()),
        || Editor::from_text(text),
    ));
}

/// The write half of `App::save`: create the file and stream the document out.
///
/// `fsync` and the atomic rename are deliberately excluded; they measure the
/// filesystem rather than this code, and their variance swamps everything else
/// in this file.
fn bench_save(text: &str) {
    let document = Document::from_text(text);
    let file = TempFile::new("save");
    report(&measure("save: write document to file", || {
        let mut handle = std::fs::File::create(file.path()).unwrap();
        document.write_to(&mut handle).unwrap();
    }));
}

/// One Normal-mode `i<char><Esc>` round trip. `Escape` steps the cursor back
/// over the character just inserted, so the position is stable across samples
/// and the document only grows by one character each time.
fn bench_insert(corpus: Corpus, text: &str) {
    let mut editor = Editor::from_text(text);
    editor.cursor = middle_line(&editor.document);
    report(&measure(
        format!("insert: i<char><Esc> in {} corpus", corpus.label()),
        || {
            editor.handle_key(Key::Char('i'));
            editor.handle_key(Key::Char('x'));
            editor.handle_key(Key::Escape);
        },
    ));
}

/// One Normal-mode `x`.
fn bench_delete(corpus: Corpus, text: &str) {
    let mut editor = Editor::from_text(text);
    editor.cursor = middle_line(&editor.document);
    report(&measure(
        format!("delete: x in {} corpus", corpus.label()),
        || {
            editor.handle_key(Key::Char('x'));
        },
    ));
}

/// A `j` + `k` pair: the cursor returns to the same line each time, so every
/// sample performs two real vertical motions rather than clamping at an edge.
/// The `w` + `b` pair is the same idea for word motion.
fn bench_motion(corpus: Corpus, text: &str) {
    let mut editor = Editor::from_text(text);
    editor.cursor = middle_line(&editor.document);
    report(&measure(
        format!("motion: j+k pair in {} corpus", corpus.label()),
        || {
            editor.handle_key(Key::Char('j'));
            editor.handle_key(Key::Char('k'));
        },
    ));
    report(&measure(
        format!("motion: w+b pair in {} corpus", corpus.label()),
        || {
            editor.handle_key(Key::Char('w'));
            editor.handle_key(Key::Char('b'));
        },
    ));
}

/// `goto_line` only moves the cursor, so no reset is needed between samples.
/// Targets are drawn from a fixed seeded list so the run stays reproducible.
fn bench_goto(corpus: Corpus, text: &str) {
    let lines = Document::from_text(text).line_count();
    if lines < 2 {
        return;
    }
    let mut random = Rng::new(0x1234_5678_9abc_def0);
    let targets: Vec<usize> = (0..64)
        .map(|_| 1 + random.next_u64() as usize % lines)
        .collect();
    let mut editor = Editor::from_text(text);
    let mut next = 0usize;
    report(&measure(
        format!("goto: goto_line in {} corpus", corpus.label()),
        || {
            editor.goto_line(targets[next % targets.len()]);
            next += 1;
        },
    ));
}

/// Resolver-only, because the operator that would consume the range mutates the
/// document. These are the scans the text-object path spends its time in.
fn bench_text_objects(corpus: Corpus, text: &str) {
    let document = Document::from_text(text);
    let cursor = CharPos(document.len_chars() / 2);
    let objects = [
        (
            "iw",
            TextObject::Word {
                around: false,
                big: false,
            },
        ),
        (
            "aw",
            TextObject::Word {
                around: true,
                big: false,
            },
        ),
        (
            "i(",
            TextObject::Pair {
                around: false,
                kind: PairKind::Parenthesis,
            },
        ),
        (
            "a\"",
            TextObject::Quote {
                around: true,
                quote: '"',
            },
        ),
    ];
    for (label, object) in objects {
        report(&measure(
            format!("textobject: {label} in {} corpus", corpus.label()),
            || resolve_text_object(&document, cursor, object, 1),
        ));
    }
}

/// A blockwise selection spanning 100 lines, the shape that makes
/// `Selection::ranges` query the rope once per selected line.
fn bench_selection_ranges(text: &str) {
    let document = Document::from_text(text);
    let lines = document.line_count();
    if lines < 200 {
        return;
    }
    let first = lines / 4;
    let selection = Selection {
        anchor: document.line_start(first),
        active: document.line_start(first + 100),
        kind: SelectionKind::Blockwise { left: 0, right: 40 },
    };
    report(&measure(
        "selection: ranges() over a 100-line block",
        || selection.ranges(&document),
    ));
}

/// The bracket-pair highlight the renderer recomputes on every cursor move.
///
/// Two shapes, because they cost very different amounts:
///
/// * `matched` parks the cursor on a delimiter that has a partner a short walk
///   away, which is the well-formed case.
/// * `unclosed` truncates the document so the nearest delimiter has no partner.
///   This is the ordinary situation while the user is mid-keystroke, and it is
///   the shape that degenerates into scanning to end of file.
fn bench_matching_partner(corpus: Corpus, text: &str) {
    let full = Document::from_text(text);
    match find_delimiter(&full, full.len_chars() / 2) {
        Some(position) => report(&measure(
            format!(
                "matching: matching_partner, matched delimiter in {} corpus",
                corpus.label()
            ),
            || matching_partner(&full, CharPos(position)),
        )),
        None => println!("  (no delimiter at or after the midpoint; skipping matched)"),
    }

    // An unclosed delimiter placed at the very front of an otherwise ordinary
    // document. Two placements matter and the obvious one is wrong: truncating a
    // balanced corpus does not produce an unmatched delimiter, and appending an
    // unclosed bracket at the end leaves nothing after it for the forward scan
    // to cross. Prepending puts the delimiter near offset zero with the whole
    // document behind it, which is what makes the scan degenerate.
    let fragment = "fn wed_unclosed_case() { if flag { sink(\n";
    let unclosed_text = format!("{fragment}{text}");
    let unclosed = Document::from_text(&unclosed_text);
    let position = fragment.len() - 2;
    report(&measure(
        format!(
            "matching: matching_partner, unclosed delimiter in {} corpus",
            corpus.label()
        ),
        || matching_partner(&unclosed, CharPos(position)),
    ));
}

/// `%`, which is the path that tries all four bracket kinds in turn. On a
/// document without delimiters each of those runs a full scan to end of file,
/// so this is the worst case for the pair-matching code.
fn bench_matching_pair_motion(corpus: Corpus, text: &str) {
    let mut editor = Editor::from_text(text);
    editor.cursor = CharPos(editor.document.len_chars() / 2);
    report(&measure(
        format!("matching: % motion in {} corpus", corpus.label()),
        || {
            editor.handle_key(Key::Char('%'));
        },
    ));
}

/// A full incremental search: `/` or `?`, the pattern, then Enter.
///
/// The pattern occurs exactly once and the cursor starts at the top, so the
/// forward search has to scan nearly the whole document. Direction alternates
/// each sample, which keeps the cursor anchored: the forward search relocates
/// it onto the single match, and the backward search from there finds nothing
/// and leaves it in place.
fn bench_search(text: &str) {
    let Some(pattern) = rare_pattern(text) else {
        println!("  (no unique pattern found; skipping)");
        return;
    };
    let mut editor = Editor::from_text(text);
    editor.cursor = CharPos::ZERO;
    let mut backwards = false;
    report(&measure("search: / and ? with a unique pattern", || {
        backwards = !backwards;
        editor.handle_key(Key::Char(if backwards { '?' } else { '/' }));
        for character in pattern.chars() {
            editor.handle_key(Key::Char(character));
        }
        editor.handle_key(Key::Enter);
        assert_eq!(
            editor.mode,
            Mode::Normal,
            "search must return to Normal mode so the next sample can start over"
        );
    }));
}

/// The `:s/old/new/g` path.
///
/// Worth measuring on its own because it is the one command that rewrites the
/// whole document, and the old implementation built the entire text, then the
/// entire replacement, and kept both in the undo record.
fn bench_replace_literal(corpus: Corpus, text: &str) {
    let Some(needle) = common_word(text) else {
        println!("  (no common word to substitute; skipping)");
        return;
    };
    let mut editor = Editor::from_text(text);
    editor.mark_saved();
    report(&measure(
        format!(
            "substitute: s/{needle}/zz/g over a {} corpus",
            corpus.label()
        ),
        || editor.replace_literal(&needle, "zz", true),
    ));
}

/// A word that occurs often, so the substitution does real work rather than
/// failing on the first character.
fn common_word(text: &str) -> Option<String> {
    // The generated corpora draw from a fixed vocabulary, so the first token of
    // the first line is guaranteed to recur.
    let end = text.find('\n')?;
    let line = &text[..end];
    let start = line.find(|c: char| c.is_ascii_alphabetic())?;
    let word: String = line[start..]
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    (word.len() >= 2).then_some(word)
}
/// A forward search that starts deep inside the document.
///
/// The existing search case starts at position zero, where the chunk loop has
/// nothing to skip. Starting halfway in is the case that makes it skip, and it
/// is also what a user gets after opening a large file and pressing `n` until the
/// cursor is well past the beginning.
fn bench_search_from_deep(text: &str) {
    let Some(pattern) = absent_pattern(text) else {
        println!("  (could not build an absent pattern; skipping)");
        return;
    };
    let mut editor = Editor::from_text(text);
    editor.cursor = CharPos(editor.document.len_chars() / 2);
    report(&measure(
        "search: / from the midpoint with no match",
        || {
            editor.handle_key(Key::Char('/'));
            for character in pattern.chars() {
                editor.handle_key(Key::Char(character));
            }
            editor.handle_key(Key::Enter);
            // The pattern does not occur, so the cursor never moves and the next
            // sample starts from the same place.
            assert_eq!(editor.cursor, CharPos(editor.document.len_chars() / 2));
        },
    ));
}

/// A pattern that occurs zero times, so the scan runs to end of file.
fn absent_pattern(text: &str) -> Option<String> {
    let mut random = Rng::new(0x0bad_c0de_0000_0001);
    for _ in 0..256 {
        // `~` appears in no generated corpus, so this cannot occur either.
        let candidate = format!("~q{}z", random.next_u64() % 1_000_000_000);
        if text.find(candidate.as_str()).is_none() {
            return Some(candidate);
        }
    }
    None
}

/// Picks a pattern that occurs exactly once, so a search cannot stop early.
///
/// Derived from the document rather than invented: an invented token would not
/// occur at all, and a search that misses immediately measures nothing.
///
/// Takes the final line and lengthens a suffix of it until the suffix is unique.
/// Growing from the line end matters twice over: the generated corpora draw
/// from a vocabulary of only sixteen words, so no single word is ever unique and
/// the pattern has to span a whole line; and a pattern taken from the tail sits
/// far from a cursor at position zero, which is what makes the scan deep.
/// far from a cursor at position zero, which is what makes the scan deep.
///
/// Capped at [`MAX_PATTERN_BYTES`]. Without a cap the long-line corpus yields a
/// 64 KiB pattern, and what gets measured becomes the cost of simulating sixty
/// thousand keystrokes rather than the cost of the search.
const MAX_PATTERN_BYTES: usize = 256;

fn rare_pattern(text: &str) -> Option<String> {
    let end = text.trim_end().len();
    let line_start = text[..end].rfind('\n').map_or(0, |index| index + 1);
    let line = &text[line_start..end];
    let longest = line.len().min(MAX_PATTERN_BYTES);
    if longest < 16 {
        return None;
    }
    for take in (16..=longest).rev() {
        if !line.is_char_boundary(line.len() - take) {
            continue;
        }
        let candidate = &line[line.len() - take..];
        let mut occurrences = 0usize;
        for _ in text.match_indices(candidate) {
            occurrences += 1;
            if occurrences > 1 {
                break;
            }
        }
        if occurrences == 1 {
            return Some(candidate.to_string());
        }
    }
    None
}

/// First opening delimiter at or after `from`.
fn find_delimiter(document: &Document, from: usize) -> Option<usize> {
    document
        .chars_from(CharPos::ZERO)
        .skip(from)
        .find(|(_, character)| matches!(character, '(' | '[' | '{' | '<'))
        .map(|(position, _)| position.0)
}

/// Start of the line halfway down the document.
fn middle_line(document: &Document) -> CharPos {
    document.line_start(document.line_count() / 2)
}
