//! Retained-memory benchmarks.
//!
//! `docs/tiny-neovim-development-plan.md` section 8 sets a target of "memory
//! close to one to two times the file size", and nothing verifies it. Timing
//! benchmarks cannot see it: `:s///g` over a 50 MiB document finishes in tens of
//! milliseconds and then holds two full copies of the text in the undo record
//! indefinitely.
//!
//! A counting global allocator is the only way to see this without a dependency,
//! so this target tracks live bytes across an operation.

#[path = "harness/mod.rs"]
mod harness;

use harness::{mib, section, Corpus, Scale};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = System.alloc(layout);
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
            PEAK.fetch_max(live, Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        System.dealloc(pointer, layout)
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let grown = System.realloc(pointer, layout, new_size);
        if !grown.is_null() {
            // The old block is released and the new one taken, so the live delta
            // is the difference rather than either size alone.
            let live = LIVE.fetch_add(new_size, Relaxed) + new_size;
            let live = live.saturating_sub(layout.size());
            LIVE.store(live, Relaxed);
            PEAK.fetch_max(live, Relaxed);
        }
        grown
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Peak memory while loading.
///
/// `App::new` is not reachable from the library, so this measures the two halves
/// it is made of: reading the whole file into a string, then handing that string
/// to the rope. The editor itself keeps both alive at that moment, which is the
/// peak this exists to bound.
fn bench_load_peak(_corpus: Corpus, text: &str) {
    let whole_string = {
        let before = live();
        let owned = text.to_string();
        let string_only = live() - before;

        let before = live();
        let document = wed::core::Editor::from_text(&owned);
        let rope = live() - before;
        println!(
            "{:<52} {:>9} KiB for the file as a String",
            "load: whole-file read",
            string_only / 1024
        );
        println!(
            "{:<52} {:>9} KiB for the rope built from it",
            "load: rope construction",
            rope / 1024
        );
        println!(
            "{:<52} {:>9} KiB held at once ({:.2}x the file)",
            "load: peak with the old whole-file path",
            (string_only + rope) / 1024,
            (string_only + rope) as f64 / text.len() as f64
        );
        drop(document);
        owned
    };

    // The streaming path: a small read buffer plus the rope, never the whole file
    // as one contiguous string.
    let before = live();
    let mut document = wed::core::Document::empty();
    for piece in whole_string.as_bytes().chunks(1 << 16) {
        document.append_str(std::str::from_utf8(piece).unwrap_or(""));
    }
    let streamed = live() - before;
    println!(
        "{:<52} {:>9} KiB held at once ({:.2}x the file)",
        "load: peak with the streaming path",
        streamed / 1024,
        streamed as f64 / text.len() as f64
    );
}

/// What the bracket index costs to hold.
///
/// It is built only when a command asks about pairing and dropped by the next
/// edit, so this is a transient cost rather than something that accumulates.
fn bench_pair_index_memory(_corpus: Corpus, text: &str) {
    let document = wed::core::Document::from_text(text);
    settle();
    let before = live();
    std::hint::black_box(
        document
            .pair_index()
            .pairs(wed::core::PairKind::Parenthesis)
            .len(),
    );
    let retained = live() - before;
    let openers = document
        .pair_index()
        .open_count(wed::core::PairKind::Parenthesis);
    println!(
        "{:<52} {:>9} KiB ({:.3}x the file) for {openers} parenthesis openers",
        "pairing: index resident after build",
        retained / 1024,
        retained as f64 / text.len() as f64
    );
    // And it must go away again on the next edit.
    let baseline = live();
    document.invalidate_pair_index();
    println!(
        "{:<52} {:>9} KiB released on the next edit",
        "pairing: index released by invalidate",
        (baseline - live()) / 1024
    );
}

fn main() {
    let scale = Scale::from_env();
    println!("wed retained-memory benchmarks");
    println!("scale: {scale:?}");

    let mut sizes = vec![mib(1)];
    if scale == Scale::Full {
        sizes.push(mib(50));
    }
    for target_bytes in sizes {
        for corpus in [Corpus::Code, Corpus::Prose, Corpus::Wide] {
            let text = corpus.build(target_bytes, 0x5eed_1234_abcd_0003);
            run(corpus, target_bytes, &text);
        }
    }
}

fn run(corpus: Corpus, target_bytes: usize, text: &str) {
    section(&format!(
        "{} / {} MiB",
        corpus.label(),
        target_bytes / (1024 * 1024)
    ));
    bench_load(corpus, text);
    bench_load_peak(corpus, text);
    bench_pair_index_memory(corpus, text);
    bench_insert(corpus, text);
    bench_substitute(corpus, text);
    bench_regex_substitute_peak(corpus, text);
    bench_session_growth(corpus, text);
    bench_undo(corpus, text);
}

/// Bytes the rope itself costs, which is the baseline every other figure is a
/// multiple of.
fn bench_load(_corpus: Corpus, text: &str) {
    let baseline = live();
    let editor = wed::core::Editor::from_text(text);
    let retained = live() - baseline;
    report(
        "load: editor resident after loading the document",
        retained,
        text.len(),
        &editor,
    );
}

/// An insert session should retain only what was typed.
fn bench_insert(_corpus: Corpus, text: &str) {
    let mut editor = wed::core::Editor::from_text(text);
    settle();
    let baseline = live();
    editor.insert_text_for_test("hello");
    let retained = live() - baseline;
    report(
        "insert: retained after i<text><Esc>",
        retained,
        text.len(),
        &editor,
    );
}

/// The one to watch: the old implementation stores the whole document twice more
/// in the undo record, which for a 50 MiB file is 100 MiB held indefinitely.
fn bench_substitute(_corpus: Corpus, text: &str) {
    let Some(needle) = common_word(text) else {
        println!("  (no common word; skipping substitute)");
        return;
    };
    let mut editor = wed::core::Editor::from_text(text);
    settle();
    let baseline = live();
    let replaced = editor.replace_literal(&needle, "zz", true);
    let retained = live() - baseline;
    report(
        &format!("substitute: retained after s/{needle}/zz/g ({replaced} hits)"),
        retained,
        text.len(),
        &editor,
    );
}

/// Retained text must not grow with the length of the session.
///
/// This is what the undo budget exists for. Each session here types half a
/// megabyte, which undo has to remember; without a budget twelve of them would
/// retain six megabytes and a real session runs for hours.
///
/// Reported as the history's own byte count rather than total live bytes,
/// because the document also grows here by the amount typed, and that growth is
/// the correct result rather than overhead.
fn bench_session_growth(_corpus: Corpus, text: &str) {
    let mut editor = wed::core::Editor::from_text(text);
    let budget = mib(4);
    editor.set_undo_budget(budget);
    settle();
    let chunk = "y".repeat(mib(1) / 2);
    let before = live();
    for _ in 0..12 {
        editor.insert_text_for_test(&chunk);
    }
    let live_delta = live() - before;
    println!(
        "{:<52} {:>9} KiB undo text, budget {} KiB, {:.2}x the file, {} KiB live growth",
        "session: 12 x 0.5 MiB inserts",
        editor.history.held() / 1024,
        editor.history.budget() / 1024,
        editor.history.held() as f64 / text.len() as f64,
        live_delta / 1024,
    );
}
/// The regex form reads the document whole, so it costs one extra copy while it
/// runs. Measured the same way as the load peak.
fn bench_regex_substitute_peak(_corpus: Corpus, text: &str) {
    let Some(pattern) = common_word(text) else {
        return;
    };
    let substitution = match wed::core::Substitution::parse(&format!("s/{pattern}/zz/g")) {
        Ok(substitution) => substitution,
        Err(_) => return,
    };
    let mut editor = wed::core::Editor::from_text(text);
    settle();
    let before = live();
    let replaced = editor.substitute(&substitution);
    let peak_growth = PEAK.load(std::sync::atomic::Ordering::Relaxed) - before;
    println!(
        "{:<52} {:>9} KiB retained ({:.2}x the file), {:>9} KiB peak, {replaced} matches",
        "substitute: regex form",
        (live() - before) / 1024,
        (live() - before) as f64 / text.len() as f64,
        peak_growth / 1024
    );
}

fn bench_undo(_corpus: Corpus, text: &str) {
    let Some(needle) = common_word(text) else {
        return;
    };
    let mut editor = wed::core::Editor::from_text(text);
    editor.replace_literal(&needle, "zz", true);
    settle();
    let baseline = live();
    editor.handle_key(wed::core::Key::Char('u'));
    let retained = live() - baseline;
    report(
        "undo: retained after undoing it",
        retained,
        text.len(),
        &editor,
    );
}

fn report(what: &str, retained: usize, file_bytes: usize, _editor: &wed::core::Editor) {
    println!(
        "{what:<52} {:>9} KiB retained, {:.2}x the file",
        retained / 1024,
        retained as f64 / file_bytes as f64,
    );
}

fn live() -> usize {
    LIVE.load(Relaxed)
}

/// Runs a no-op allocation cycle so that any buffer growth triggered by the last
/// real operation is already accounted for before the baseline is read.
fn settle() {
    let mut sink: Vec<u8> = Vec::new();
    for index in 0..64u8 {
        sink.push(index);
    }
    std::hint::black_box(&sink);
    sink.clear();
    sink.shrink_to_fit();
}

/// A word that occurs often, drawn from the corpus's fixed vocabulary.
fn common_word(text: &str) -> Option<String> {
    let end = text.find('\n')?;
    let start = text[..end].find(|c: char| c.is_ascii_alphabetic())?;
    let word: String = text[start..]
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    (word.len() >= 2).then_some(word)
}
