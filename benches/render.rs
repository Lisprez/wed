//! Viewport painting benchmarks.
//!
//! `src/render.rs` and `src/app.rs` belong to the binary crate rather than the
//! library, so this target pulls them in directly with `#[path]`. That keeps the
//! benchmark measuring the real painting code instead of a copy of it, and costs
//! only the handful of `pub(crate)` markers already on `paint_line`, `Row`,
//! `Scratch`, `display_width`, and `cursor_display_column`.
//!
//! Output is collected into a `Vec<u8>` rather than a terminal, for the same
//! reason `src/render.rs`'s own tests do: it isolates the cost of producing the
//! escape sequences from the cost of the terminal consuming them.
//!
//! Run with `cargo bench --bench render`.

// The `wed` library is referenced as `wed::...` from inside both modules, so the
// include order below does not matter; only `crate::app` has to resolve, which
// the `#[path]` on `app` takes care of.
//
// Cargo builds bench targets with `cfg(test)` enabled, so each included module
// also brings in its own test helpers. Nothing here calls the terminal-facing
// half of these modules, which is exactly why they are unreachable from this
// target, so the resulting dead-code and unused-import warnings are expected.
// Scoped to the two includes rather than the crate, so the benchmark's own code
// is still fully linted.
#[allow(dead_code, unused_imports)]
#[path = "../src/app.rs"]
mod app;
#[allow(dead_code, unused_imports)]
#[path = "../src/render.rs"]
mod render;
// `render.rs` reaches for `crate::width`, so it has to come along too.
#[allow(dead_code, unused_imports)]
#[path = "../src/width.rs"]
mod width;

#[path = "harness/mod.rs"]
mod harness;

use harness::{measure, mib, report, section, Corpus, Scale};
use wed::core::{CharPos, CharRange, Document, LineColumn};

/// A conventional terminal: 80x24 leaves 22 text rows under the two status rows.
const TEXT_WIDTH: usize = 80;
const TEXT_HEIGHT: usize = 22;
/// A deep horizontal scroll offset, chosen to expose the cost of seeking from
/// the start of a line to the left edge of the viewport.
const DEEP_COL_OFFSET: usize = 40_000;

fn main() {
    let scale = Scale::from_env();
    println!("wed viewport benchmarks");
    println!("scale: {scale:?} (set WED_BENCH_SCALE=full for the 50 MiB corpora)");
    println!("viewport: {TEXT_WIDTH}x{TEXT_HEIGHT}, sink = Vec<u8>");

    let mut sizes = vec![mib(1)];
    if scale == Scale::Full {
        sizes.push(mib(50));
    }

    for target_bytes in sizes {
        for corpus in Corpus::ALL {
            let text = corpus.build(target_bytes, 0x5eed_1234_abcd_0002);
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
    let document = Document::from_text(text);
    if document.line_count() < TEXT_HEIGHT * 4 {
        println!("  (too few lines to fill the viewport; skipping)");
        return;
    }
    let row_offset = document.line_count() / 2;

    bench_frame(corpus, &document, row_offset, 0);
    bench_frame_with_selection(corpus, &document, row_offset);
    bench_deep_scroll(corpus, &document, row_offset);
    bench_cursor_display_column(corpus, &document);
    bench_matching_partner(text);
    bench_repaint_volume(corpus, text, row_offset);
}

/// Bytes a frame emits, against the bytes a frame would emit if only the rows
/// that actually changed were repainted.
///
/// A cursor move within a line changes no row content at all, so a repaint-aware
/// renderer would emit nothing but the cursor positioning. This measures the gap
/// between the two, which is the ceiling on what such a renderer could save.
fn bench_repaint_volume(corpus: Corpus, text: &str, row_offset: usize) {
    let document = Document::from_text(text);
    let lines = document.line_count();
    if lines < TEXT_HEIGHT * 4 {
        return;
    }
    let mut scratch = render::Scratch::default();
    let mut sink: Vec<u8> = Vec::with_capacity(TEXT_WIDTH * TEXT_HEIGHT * 8);

    let full = paint_viewport_bytes(&document, row_offset, 0, &[], None, &mut sink, &mut scratch);

    // Repaint with the cursor on the next line: the rows are byte-identical,
    // because the cursor is drawn separately from the row content.
    let next = paint_viewport_bytes(
        &document,
        row_offset + 1,
        0,
        &[],
        None,
        &mut sink,
        &mut scratch,
    );

    report(&measure(
        format!(
            "repaint: {} rows, bytes emitted moving the cursor one line ({} corpus)",
            TEXT_HEIGHT,
            corpus.label()
        ),
        || {
            let bytes = paint_viewport_bytes(
                &document,
                row_offset + 1,
                0,
                &[],
                None,
                &mut sink,
                &mut scratch,
            );
            assert_eq!(bytes, next, "the frame must not depend on the cursor row");
            full
        },
    ));
    println!(
        "  {} corpus: {TEXT_HEIGHT} rows emit {full} bytes; a change-aware renderer would emit 0",
        corpus.label()
    );
}

/// One viewport's worth of rows, which is what a frame actually pays for.
fn bench_frame(corpus: Corpus, document: &Document, row_offset: usize, col_offset: usize) {
    let mut scratch = render::Scratch::default();
    let mut sink: Vec<u8> = Vec::with_capacity(TEXT_WIDTH * TEXT_HEIGHT * 8);
    report(&measure(
        format!(
            "frame: {} rows painted in {} corpus (col_offset={col_offset})",
            TEXT_HEIGHT,
            corpus.label()
        ),
        || {
            paint_viewport(
                document,
                row_offset,
                col_offset,
                &[],
                None,
                &mut sink,
                &mut scratch,
            );
        },
    ));
}

/// The same rows, but with a blockwise selection covering the whole viewport, so
/// the selection range-cursor walk is on the measured path.
fn bench_frame_with_selection(corpus: Corpus, document: &Document, row_offset: usize) {
    let mut ranges: Vec<CharRange> = Vec::with_capacity(TEXT_HEIGHT);
    for screen_row in 0..TEXT_HEIGHT {
        let line = row_offset + screen_row;
        let start = document.line_start(line);
        let width = document.line_end(line).0.saturating_sub(start.0).min(20);
        ranges.push(CharRange::new(start, start.advance(width)));
    }
    let mut scratch = render::Scratch::default();
    let mut sink: Vec<u8> = Vec::with_capacity(TEXT_WIDTH * TEXT_HEIGHT * 8);
    report(&measure(
        format!(
            "frame: {} rows painted with selection in {} corpus",
            TEXT_HEIGHT,
            corpus.label()
        ),
        || {
            paint_viewport(
                document,
                row_offset,
                0,
                &ranges,
                None,
                &mut sink,
                &mut scratch,
            );
        },
    ));
}

/// The horizontally-scrolled case: every painted row has to be walked from its
/// start up to `col_offset` before anything visible is emitted.
fn bench_deep_scroll(corpus: Corpus, document: &Document, row_offset: usize) {
    let mut scratch = render::Scratch::default();
    let mut sink: Vec<u8> = Vec::with_capacity(TEXT_WIDTH * TEXT_HEIGHT * 8);
    report(&measure(
        format!(
            "frame: {} rows painted at col_offset={} in {} corpus",
            TEXT_HEIGHT,
            DEEP_COL_OFFSET,
            corpus.label()
        ),
        || {
            paint_viewport(
                document,
                row_offset,
                DEEP_COL_OFFSET,
                &[],
                None,
                &mut sink,
                &mut scratch,
            );
        },
    ));
}

/// Resolving the cursor's display column, which the renderer does on every frame
/// whose cursor or revision moved.
fn bench_cursor_display_column(corpus: Corpus, document: &Document) {
    let line = document.line_count() / 2;
    let start = document.line_start(line);
    let column = document.line_end(line).0.saturating_sub(start.0);
    report(&measure(
        format!(
            "cursor: display column at end of a line in {} corpus",
            corpus.label()
        ),
        || render::cursor_display_column(document, LineColumn { line, column }),
    ));
}

/// The full `App`-level bracket highlight, which the renderer recomputes on
/// every frame whose cursor or revision moved.
///
/// Two shapes, because the well-formed one is far cheaper than the interesting
/// one: a delimiter whose partner is a short walk away resolves immediately,
/// whereas an unclosed delimiter near the start of a large document makes the
/// forward scan run to end of file. The second is what a user hits while
/// mid-keystroke, and it sits on the per-frame path.
fn bench_matching_partner(text: &str) {
    let mut app = app::App::new(None).unwrap();
    app.editor.replace_text(text);
    match find_delimiter(&app.editor.document, app.editor.document.len_chars() / 2) {
        Some(position) => {
            app.editor.cursor = CharPos(position);
            report(&measure(
                "matching: matching_partner_for_render, matched",
                || render::matching_partner_for_render(&app),
            ));
        }
        None => println!("  (no delimiter at or after the midpoint; skipping matched)"),
    }

    let fragment = "fn wed_unclosed_case() { if flag { sink(\n";
    app.editor.replace_text(&format!("{fragment}{text}"));
    app.editor.cursor = CharPos(fragment.len() - 2);
    report(&measure(
        "matching: matching_partner_for_render, unclosed",
        || render::matching_partner_for_render(&app),
    ));
}

fn paint_viewport(
    document: &Document,
    row_offset: usize,
    col_offset: usize,
    selection_ranges: &[CharRange],
    matching_partner: Option<CharPos>,
    sink: &mut Vec<u8>,
    scratch: &mut render::Scratch,
) {
    std::hint::black_box(paint_viewport_bytes(
        document,
        row_offset,
        col_offset,
        selection_ranges,
        matching_partner,
        sink,
        scratch,
    ));
}

/// Paints a viewport and reports how many bytes it emitted.
fn paint_viewport_bytes(
    document: &Document,
    row_offset: usize,
    col_offset: usize,
    selection_ranges: &[CharRange],
    matching_partner: Option<CharPos>,
    sink: &mut Vec<u8>,
    scratch: &mut render::Scratch,
) -> usize {
    sink.clear();
    for screen_row in 0..TEXT_HEIGHT {
        let row = render::Row {
            document,
            file_row: row_offset + screen_row,
            col_offset,
            text_width: TEXT_WIDTH,
            selection_ranges,
            matching_partner,
        };
        render::paint_line(sink, row, scratch).unwrap();
    }
    sink.len()
}

fn find_delimiter(document: &Document, from: usize) -> Option<usize> {
    document
        .chars_from(CharPos::ZERO)
        .skip(from)
        .find(|(_, character)| matches!(character, '(' | '[' | '{' | '<'))
        .map(|(position, _)| position.0)
}
