//! Shared plumbing for the hand-rolled benchmarks.
//!
//! No external dependencies: corpora come from a seeded xorshift PRNG and the
//! timings from [`std::time::Instant`]. Included by each bench target with
//! `#[path = "harness/mod.rs"] mod harness;`.

// Each bench target is a separate crate that uses a different subset of this
// shared module, so much of it is dead code from any one of their point of view.
#![allow(dead_code)]

use std::time::{Duration, Instant};

/// Which corpus sizes to run, chosen by the `WED_BENCH_SCALE` env var.
///
/// `fast` (the default, and what CI uses) exercises the 1 MiB corpora so a full
/// `cargo bench` finishes in seconds. `full` adds the 50 MiB corpora, which is
/// what the performance targets in `docs/tiny-neovim-development-plan.md` are
/// stated against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Fast,
    Full,
}

impl Scale {
    pub fn from_env() -> Self {
        match std::env::var("WED_BENCH_SCALE").as_deref() {
            Ok("full") => Scale::Full,
            _ => Scale::Fast,
        }
    }

    /// Largest corpus to build, in bytes.
    pub fn max_bytes(self) -> usize {
        match self {
            Scale::Fast => mib(1),
            Scale::Full => mib(50),
        }
    }
}

pub fn mib(n: usize) -> usize {
    n * 1024 * 1024
}

/// The flavours of synthetic text the editor has to stay responsive on.
///
/// Each one stresses a different part of the hot path: `Code` is dense with the
/// brackets and quotes the pair-matching scans look for, `Wide` forces
/// double-width glyph handling in the renderer, `Tabs` forces tab-stop
/// arithmetic, and `Prose` is the long-run-of-plain-characters case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corpus {
    Code,
    Wide,
    Tabs,
    Prose,
    LongLine,
}

impl Corpus {
    pub const ALL: [Corpus; 5] = [
        Corpus::Code,
        Corpus::Wide,
        Corpus::Tabs,
        Corpus::Prose,
        Corpus::LongLine,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Corpus::Code => "code",
            Corpus::Wide => "wide",
            Corpus::Tabs => "tabs",
            Corpus::Prose => "prose",
            Corpus::LongLine => "long-line",
        }
    }

    /// Builds a deterministic corpus of roughly `target_bytes` bytes.
    ///
    /// Deterministic on purpose: a benchmark that re-rolls its input every run
    /// cannot be compared against a recorded baseline.
    pub fn build(self, target_bytes: usize, seed: u64) -> String {
        let mut random = Rng::new(seed);
        let mut text = String::with_capacity(target_bytes + target_bytes / 8);
        while text.len() < target_bytes {
            match self {
                Corpus::Code => push_code_line(&mut random, &mut text),
                Corpus::Wide => push_wide_line(&mut random, &mut text),
                Corpus::Tabs => push_tab_line(&mut random, &mut text),
                Corpus::Prose => push_prose_line(&mut random, &mut text),
                Corpus::LongLine => push_long_line(&mut random, &mut text),
            }
        }
        text
    }
}

/// Bytes per line in the `long-line` corpus.
const LONG_LINE: usize = 64 * 1024;

/// xorshift64*, so corpus generation stays reproducible without a dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // A zero state would be absorbing, so keep the seed away from it.
        Self(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.0 = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }

    fn pick<'a>(&mut self, options: &[&'a str]) -> &'a str {
        options[self.below(options.len())]
    }
}

const IDENTIFIERS: [&str; 12] = [
    "buffer",
    "cursor",
    "window",
    "render",
    "motion",
    "history",
    "rope",
    "chunk",
    "column",
    "selection",
    "revision",
    "indent",
];
const CODE_KEYWORDS: [&str; 8] = [
    "let", "fn", "match", "return", "if", "else", "while", "struct",
];
const CODE_PUNCTUATION: [&str; 8] = ["(", ")", "{", "}", "[", "]", ",", ";"];
const WORDS: [&str; 16] = [
    "editor", "buffer", "document", "cursor", "motion", "range", "line", "column", "render",
    "paint", "chunk", "byte", "char", "region", "viewport", "offset",
];

fn push_code_line(random: &mut Rng, text: &mut String) {
    // Shaped like real code: nesting delimiters, string literals with escaped
    // quotes, and a keyword at the head of the line.
    text.push_str(random.pick(&CODE_KEYWORDS));
    text.push(' ');
    text.push_str(random.pick(&IDENTIFIERS));
    text.push_str(random.pick(&CODE_PUNCTUATION));
    text.push('"');
    let words = 2 + random.below(6);
    for index in 0..words {
        if index > 0 {
            text.push(' ');
        }
        text.push_str(random.pick(&WORDS));
    }
    // An escaped quote every so often, so the pair scanners see real backslash
    // runs rather than only the trivial case.
    if random.below(8) == 0 {
        text.push_str("\\\"");
        text.push_str(random.pick(&WORDS));
    }
    text.push('"');
    text.push_str(random.pick(&CODE_PUNCTUATION));
    text.push_str(random.pick(&IDENTIFIERS));
    text.push_str(random.pick(&CODE_PUNCTUATION));
    text.push('\n');
}

fn push_wide_line(random: &mut Rng, text: &mut String) {
    // CJK plus ASCII, which is what makes the display column diverge from the
    // character index.
    const WIDE: [&str; 6] = ["世", "界", "編輯", "器", "缓冲区", "行"];
    let words = 4 + random.below(10);
    for index in 0..words {
        if index > 0 {
            text.push(' ');
        }
        if random.below(3) == 0 {
            text.push_str(random.pick(&IDENTIFIERS));
        } else {
            text.push_str(random.pick(&WIDE));
        }
    }
    text.push('，');
    text.push_str(random.pick(&WIDE));
    text.push('。');
    text.push('\n');
}

fn push_tab_line(random: &mut Rng, text: &mut String) {
    // Leading indentation of varying depth, so tab-stop expansion has to be
    // recomputed at a column that is not a multiple of eight.
    let depth = 1 + random.below(6);
    for _ in 0..depth {
        text.push('\t');
    }
    let words = 3 + random.below(8);
    for index in 0..words {
        if index > 0 && random.below(4) == 0 {
            text.push('\t');
        } else if index > 0 {
            text.push(' ');
        }
        text.push_str(random.pick(&WORDS));
    }
    text.push('\n');
}

/// One very long line, the shape that makes the horizontal viewport expensive.
///
/// Lines this long are ordinary in the wild (minified assets, CSV exports,
/// unwrapped logs) and they are the only input for which the viewport's
/// horizontal seek and the cursor's display column both have to walk from the
/// start of a line rather than from a nearby chunk boundary.
fn push_long_line(random: &mut Rng, text: &mut String) {
    let start = text.len();
    while text.len() - start < LONG_LINE {
        match random.below(8) {
            0 => text.push_str(random.pick(&CODE_PUNCTUATION)),
            1 => text.push('\t'),
            _ => text.push_str(random.pick(&IDENTIFIERS)),
        }
        if random.below(24) == 0 {
            text.push(' ');
        }
    }
    text.push('\n');
}

fn push_prose_line(random: &mut Rng, text: &mut String) {
    // Long plain runs: the cheapest case, and the one where per-character
    // overheads are most visible relative to the work done.
    let sentences = 1 + random.below(4);
    for sentence in 0..sentences {
        if sentence > 0 {
            text.push(' ');
        }
        let words = 5 + random.below(14);
        for index in 0..words {
            if index > 0 {
                text.push(' ');
            }
            text.push_str(random.pick(&WORDS));
        }
        text.push_str(". ");
    }
    text.push('\n');
}

/// Latency samples for one benchmark case, in the order they were collected.
pub struct Samples {
    label: String,
    durations: Vec<Duration>,
}

impl Samples {
    fn new(label: String) -> Self {
        Self {
            label,
            durations: Vec::new(),
        }
    }

    pub fn push(&mut self, duration: Duration) {
        self.durations.push(duration);
    }

    /// p-th percentile using the nearest-rank method.
    fn percentile(&self, p: usize) -> Duration {
        assert!(!self.durations.is_empty(), "no samples collected");
        let mut sorted = self.durations.clone();
        sorted.sort_unstable();
        let rank = (p * sorted.len()).div_ceil(100).max(1) - 1;
        sorted[rank.min(sorted.len() - 1)]
    }

    pub fn min(&self) -> Duration {
        self.percentile(0)
    }

    pub fn p50(&self) -> Duration {
        self.percentile(50)
    }

    pub fn p95(&self) -> Duration {
        self.percentile(95)
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn count(&self) -> usize {
        self.durations.len()
    }
}

/// Runs `body` enough times to fill a time budget, then reports percentiles.
///
/// Iteration count is derived from a single untimed probe so that a case costing
/// 400ms does not try to collect 200 samples. The floor of
/// [`MIN_ITERATIONS`] keeps even the cheapest cases from reporting percentiles
/// over a handful of noisy samples.
const BUDGET: Duration = Duration::from_millis(400);
const MIN_ITERATIONS: usize = 12;
const MAX_ITERATIONS: usize = 400;

pub fn measure<F, T>(label: impl Into<String>, mut body: F) -> Samples
where
    F: FnMut() -> T,
{
    let label = label.into();
    // Probe untimed, then discard: it pays for cold caches and first-touch page
    // faults, which would otherwise land in the first reported sample.
    let probe = Instant::now();
    std::hint::black_box(body());
    let per_iteration = probe.elapsed();

    let iterations = if per_iteration.is_zero() {
        MAX_ITERATIONS
    } else {
        let affordable = (BUDGET.as_nanos() / per_iteration.as_nanos().max(1)) as usize;
        affordable.clamp(MIN_ITERATIONS, MAX_ITERATIONS)
    };

    let mut samples = Samples::new(label);
    for _ in 0..iterations {
        let start = Instant::now();
        std::hint::black_box(body());
        samples.push(start.elapsed());
    }
    samples
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Prints one result line in a stable, greppable shape.
pub fn report(samples: &Samples) {
    println!(
        "{:<46} n={:<4} min={:>9.3}ms p50={:>9.3}ms p95={:>9.3}ms",
        samples.label(),
        samples.count(),
        ms(samples.min()),
        ms(samples.p50()),
        ms(samples.p95()),
    );
}

/// Section heading so a full bench run reads as a report.
pub fn section(title: &str) {
    println!("\n== {} ==", title);
}

/// A temporary file that removes itself, so the save benchmark can exercise
/// the real write path without leaving anything behind.
pub struct TempFile {
    path: std::path::PathBuf,
}

impl TempFile {
    pub fn new(tag: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("wed-bench-{}-{}.txt", tag, std::process::id()));
        // A stale file from an interrupted run would make `create_new` fail.
        let _ = std::fs::remove_file(&path);
        Self { path }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
