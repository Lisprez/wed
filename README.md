# 🛡️ wed

[![Performance](https://img.shields.io/badge/Performance-verified-brightgreen)](docs/perf-baseline.md)
[![Security](https://img.shields.io/badge/Microsoft_Defender-Verified_Clean-blue)](#-security--false-positive-verification)
[![Language](https://img.shields.io/badge/Built_With-Rust-orange)](https://www.rust-lang.org/)

wed is a lightweight, terminal-based text editor written from scratch in Rust.
The text is stored in a [rope](https://github.com/cessen/ropey), and every edit
the editor performs is local: it touches only the characters being changed, so a
keystroke costs the same whether the document is one line or fifty megabytes.

Every performance claim in this README is measured by `cargo bench` and recorded,
with the commands that reproduce it, in [docs/perf-baseline.md](docs/perf-baseline.md).

---

## 🔥 Key Features

- **Local edits measured at microseconds.** `i<char><Esc>`, `x`, `j`/`k`, `w`/`b`
  and `goto_line` all stay under 0.002 ms p95 on a 50 MiB document; the cost is
  the rope's, not the file's.
- **Rope storage.** Text lives in a balanced tree of chunks, so an edit rewrites
  a chunk rather than the document, and undo remembers only what changed.
- **Rendering cost independent of file size.** A frame paints only the visible
  rows, so it costs the same on 1 MiB and 50 MiB.
- **Cross-platform:** Windows (`.exe`) and Linux.
- **100% transparent & secure:** open-source, and independently analyzed and
  whitelisted by major security vendors.

---

## 🚀 Performance Benchmarks

wed has been stressed with document loads up to **50 MB**. All figures below are
from `docs/perf-baseline.md`; every one of them is reproducible with the commands
listed there.

| Measurement | Result | Source |
| :--- | :--- | :--- |
| Load 1 MiB into the rope | ~0.20 ms | `cargo bench --bench edit` |
| Load 50 MiB into the rope | ~10.2 ms (target < 1 s) | `docs/perf-baseline.md` §1 |
| Ordinary keystroke, 50 MiB | < 0.002 ms p95 | `docs/perf-baseline.md` §2 |
| Memory after loading | ~1.13–1.14× the file size | `cargo bench --bench alloc` |
| One rendered frame (22 rows) | ~0.017 ms | `docs/perf-baseline.md` §5 |

*Note: rendering and ordinary editing touch only the visible rows and the edited
region respectively, so their cost does not grow with the file. See
`docs/perf-baseline.md` for the full tables, the known exceptions (horizontal
scrolling on a long line), and how to reproduce every number.*

---

## 🛡️ Security & False Positive Verification

wed prioritizes absolute security transparency. Our compiled release binaries are clean, open-source, and verified directly at the engine level.

To guarantee immediate trust on Windows platforms, wed's binary has been formally analyzed and whitelisted by **Microsoft Security Intelligence**:

- **Submission Status:** Completed / Clean (Not Malware)
- **Microsoft Submission ID:** `1311234b-5a50-497e-875d-f6a374ad9df4`
- **Determination:** Cleared completely from cloud & client signatures.

If your local system has a cached older signature, force an immediate database update using an elevated Command Prompt:
```cmd
cd "c:\Program Files\Windows Defender"
MpCmdRun.exe -removedefinitions -dynamicsignatures
MpCmdRun.exe -SignatureUpdate
```

---

## 📦 Installation & Build

### Prerequisites
Ensure you have the Rust toolchain installed.

### Building from Source
```bash
# Clone the repository
git clone https://github.com/scxr-dev/wed.git
cd wed

# Build the release profile
cargo build --release
```
The compiled binary will be available at `./target/release/wed`.

---

The author of the wed text editor is R. H. A. Ashan Imalka.

## 🤝 Contributing & Stars

If you like this ultra-lean text editor architecture, please **drop a ⭐ Star** on the repository! It helps more developers discover high-performance open-source tools.
