# wed 性能优化子任务清单（2026-10-10，评审修正版）

本文件是 `docs/perf-optimization-plan-2026-10-09.md` 经逐条核验与修正后的**可执行拆解**。
共 20 个任务，按编号顺序执行。每个任务自包含：改动位置、精确改法、禁止事项、验收命令与预期。
每个任务都标注了"与方案的差异"（方案原文有缺陷、按本文件为准的地方）。

---

## 0. 执行者须知

- 仓库根目录：`/Users/lisprez/Projects/wed`。所有命令在根目录运行。
- 工具链由 `rust-toolchain.toml` 固定（rustc 1.98.1）。不要改 `Cargo.toml` 的依赖与 profile。
- 每个任务完成后必须依次运行，全绿才算完成：
  ```bash
  cargo fmt --all
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all-targets
  ```
  `cargo test --all-targets` 是 CI 门禁，必须通过（T01/T02 完成前它可能因已知缺陷失败，见对应任务）。
- 代码注释一律用英文（与仓库现有风格一致）。不要顺手重构任务未列出的代码。
- 每完成一个任务并验收通过后，立即提交一次，提交信息格式：`T0X: <任务标题>`。提交只包含本任务改动的文件。
- 若验收失败且无法在合理时间内修复：用 `git checkout -- <本任务涉及的文件>` 回滚，在提交记录或笔记中写明失败原因，跳过依赖它的任务，继续下一个无依赖任务。
- 性能基准默认 Fast 规模（1 MiB 语料，秒级）。只有"总验收"才用 `WED_BENCH_SCALE=full`（50 MiB）。
- 行号可能随改动漂移：先用 `grep -n` 定位目标代码，再按描述修改；描述与行号冲突时以描述为准。
- 可选探针工程在 `/tmp/wedprobe`（可能已被清理）。存在时可用于交叉验证，不存在时不要重建，以仓库内 `cargo bench` 为准。

## 0.1 任务总览（编号 = 执行顺序；优先级 P0 最高）

| 编号 | 标题 | 优先级 | 依赖 | 可选 |
|---|---|---|---|---|
| T01 | 修复 alloc 基准的计数分配器竞态与回绕 | P0 | 无 | |
| T02 | Document 析构时关闭并 join 索引 worker 线程 | P0 | 无 | |
| T03 | width() 增加 ASCII 精确快路（P1-C） | P1 | T01,T02 | |
| T04 | RowPainter::flush 改为借出后 clear，保住缓冲容量（P1-D） | P1 | T01,T02 | |
| T05 | 渲染冷路径改用一次外向遍历（P1-A 修正版） | P1 | T01,T02 | |
| T06 | 渲染配对窗口从 64 KiB 收紧到 4 KiB（P1-B） | P1 | T05 | |
| T07 | matching_quote_partner 接受并应用 limit（P3-B） | P1 | T05,T06 | |
| T08 | 索引 worker 改为尾沿防抖，打字期间不再重建（P2-A 修正版） | P2 | T02 | |
| T09 | PairIndex 重设计：u32 存储 + parent 链 + O(log) 查询（P2-B+P2-C） | P2 | T01,T02 | |
| T10 | next_opening_pair_scan 单趟栈式重写 + 参考实现对拍（P2-D 修正版） | P2 | T05,T09 | |
| T11 | FirstNonBlank 改用 chars_from（P3-A） | P3 | T01,T02 | |
| T12 | substitute 提前释放整份文本拷贝（P3-C） | P3 | T01,T02 | |
| T13 | Transaction 缓存字节总数（P3-D） | P3 | T01,T02 | |
| T14 | 寄存器文本改为 Rc\<str\>（P3-E） | P3 | T01,T02 | |
| T15 | find_first_match 从 from 精确偏移开始（P3-F） | P3 | T01,T02 | |
| T16 | tag_object：游离 `<` 不再中断整个搜索 + 复用缓冲（P4-A 收窄） | P4 | T01,T02 | |
| T17 | tag_object 双向扫描重写（P4-A 完整版） | P4 | T16 | 可选 |
| T18 | 删除 replace_literal 并迁移测试/基准（P4-B） | P4 | T01,T02 | 可选 |
| T19 | README 修正：架构表述、O(1) 措辞与性能数字（P4-C） | P4 | T06 | 可选 |
| T20 | CI 补 cargo audit 与 alloc 基准（P4-D 收窄） | P4 | T01,T02 | 可选 |

---

## T01 修复 alloc 基准的计数分配器竞态与回绕

**优先级** P0（CI 门禁当前失败，必须先修）
**依赖** 无
**涉及文件** `benches/alloc.rs`

**目标** 让 `cargo test --all-targets`（debug）与 `cargo bench --bench alloc`（release）都不再 panic / 输出回绕巨值。

**现状**（已实测确认的缺陷，两处根因，本任务修第一处）：
1. `realloc`（约 39-50 行）先 `fetch_add` 再 `store`，两次原子操作之间其它线程（索引 worker）的更新会被 `store` 覆盖丢失：
   ```rust
   let live = LIVE.fetch_add(new_size, Relaxed) + new_size;
   let live = live.saturating_sub(layout.size());
   LIVE.store(live, Relaxed);
   ```
2. 多处 `live() - baseline` 形式的 usize 减法，在 worker 线程异步释放内存时会下溢：debug 直接 `attempt to subtract with overflow` panic（已在 179 行与 214 行复现）；release 回绕出 `1801439850948xxxx KiB` 一类巨值。

**改动步骤**
1. 用 `fetch_update` 重写 `realloc` 的计数逻辑：
   ```rust
   unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
       let grown = System.realloc(pointer, layout, new_size);
       if !grown.is_null() {
           // The old block is released and the new one taken, so the live delta
           // is the difference rather than either size alone. `fetch_update`
           // keeps concurrent updates (the index worker frees on its own
           // thread) from being lost, which a fetch_add + store pair does not.
           let grown_by = new_size.saturating_sub(layout.size());
           let shrunk_by = layout.size().saturating_sub(new_size);
           if let Ok(previous) = LIVE.fetch_update(Relaxed, Relaxed, |live| {
               Some(live.saturating_add(grown_by).saturating_sub(shrunk_by))
           }) {
               let now = previous.saturating_add(grown_by).saturating_sub(shrunk_by);
               PEAK.fetch_max(now, Relaxed);
           }
       }
       grown
   }
   ```
2. 把所有对 `LIVE`/`PEAK` 读数的裸减法改为 `saturating_sub`。用
   `grep -n "live() -\|- live()\|PEAK.load" benches/alloc.rs` 找全（约 11 处，分布于 `bench_load_peak`、`bench_pair_index_memory`、`bench_load`、`bench_insert`、`bench_session_growth`、`bench_regex_substitute_peak`、`bench_undo`、`report` 调用点）。示例：
   - `let retained = live() - baseline;` → `let retained = live().saturating_sub(baseline);`
   - `(baseline - live()) / 1024` → `(baseline.saturating_sub(live())) / 1024`
   - `PEAK.load(...) - before` → `PEAK.load(...).saturating_sub(before)`

**不要做** 不要改基准的测量逻辑、语料或输出格式；不要动 `settle()`、`common_word`。

**验收**
- `cargo test --bench alloc`（debug 路径）不再出现 `attempt to subtract with overflow`。
- `cargo bench --bench alloc` 输出中不再出现 `1801439850948` 开头的回绕值（个别数值在 T02 完成前仍可能偏小，属预期，不算失败）。
- `cargo fmt` / `cargo clippy` 通过。

**回滚** `git checkout -- benches/alloc.rs`

**与方案的差异** 方案把"panic 于 cargo bench"与"release 回绕"混在一句里；实际是 debug 的 `cargo test` 路径 panic、release 的 `cargo bench` 回绕，两者都要修。本任务同时修了方案未点名的 `realloc` 丢更新竞态。

---

## T02 Document 析构时关闭并 join 索引 worker 线程

**优先级** P0
**依赖** 无（与 T01 独立，但两者都完成门禁才完全稳定）
**涉及文件** `src/core/document.rs`

**目标** 让后台索引线程的生命周期与 Document 同步结束，内存释放变为确定性的（这是 T01 中竞态的根因）。

**现状** `Document` 没有 `impl Drop`（已确认）。worker 线程由 `ensure_index_worker()` 启动（约 103-141 行），`changed()` 每次编辑都向它投递快照（66-76 行）。Document 析构只丢弃 `Sender`/`JoinHandle`，线程在后台继续把 rope/索引释放掉，时间不定，打乱计数分配器。

**改动步骤**
1. 在 `impl Document` 之后（`mod tests` 之前）新增：
   ```rust
   impl Drop for Document {
       fn drop(&mut self) {
           // Close the command channel first: the worker's next recv() fails
           // and the loop exits, so the join below cannot hang. If a build is
           // in flight this waits for it, which is bounded by one index build.
           *self.index_cmd.borrow_mut() = None;
           if let Some(worker) = self.index_worker.borrow_mut().take() {
               let _ = worker.join();
           }
       }
   }
   ```
2. 不要改 worker 循环本身（T08 会改它的等待策略，本任务先保持阻塞 `recv()`）。T08 完成后 worker 阻塞在 `recv_timeout`，`Sender` 被 drop 时同样会以 `Disconnected` 立即退出，join 不会挂起——两任务兼容。

**不要做** 不要给 `Document::from_rope`（worker 内部用它构建快照文档）加任何行为；它的 `index_cmd` 是 `None`，drop 路径天然是空操作，不会递归。

**验收**
- `cargo test --all-targets` 全绿（此前会 panic 的 alloc 基准现在应当稳定通过）。
- `cargo bench --bench alloc` 的 `pairing: index released by invalidate`、`load` 等行数值落在合理范围（1.1-1.2× 量级），无回绕值。
- 测试套件整体时长无异常（每个文档析构最多等待一次索引构建的时间）。

**回滚** `git checkout -- src/core/document.rs`

**与方案的差异** 方案 P3-G 提出"析构 join worker 或改分配器线程安全"二选一；两者都必要，分别落在 T02 与 T01。

---

## T03 width() 增加 ASCII 精确快路（P1-C）

**优先级** P1
**依赖** T01,T02（仅为门禁稳定）
**涉及文件** `src/width.rs`

**目标** 消除每字符两次二分查找中的绝大多数（ASCII 文本）。

**现状** `width()`（约 144-156 行）对每个字符都做 `in_ranges` 二分。两张表的最小起始码点已核对：`ZERO_WIDTH` 首项 `(0x0300, 0x036F)`（约 28 行），`WIDE` 首项 `(0x1100, 0x115F)`（约 63 行）。因此 `code < 0x0300` 的字符必然是 1 格，无需查表。

**改动步骤** 把 `width()` 改为（快路必须放在 tab 判断**之后**，因为 tab 是 0x09，会落进 `code < 0x0300`）：
```rust
pub fn width(character: char, column: usize) -> usize {
    if character == '\t' {
        return TAB_STOP - (column % TAB_STOP);
    }
    let code = character as u32;
    // Exact fast path: the smallest code point in either table is 0x0300
    // (ZERO_WIDTH), so everything below it is one cell without a lookup.
    if code < 0x0300 {
        return 1;
    }
    if in_ranges(&ZERO_WIDTH, code) {
        0
    } else if in_ranges(&WIDE, code) {
        2
    } else {
        1
    }
}
```

**不要做** 不要改两张表的内容或顺序；不要动 `TAB_STOP`；不要给 `in_ranges` 加缓存。

**验收** `cargo test --all-targets` 全绿（width 与 render 的既有测试覆盖本行为）。
**回滚** `git checkout -- src/width.rs`

**与方案的差异** 无实质差异；方案未提快路位置必须在 tab 判断之后，已补。

---

## T04 RowPainter::flush 改为借出后 clear，保住缓冲容量（P1-D）

**优先级** P1
**依赖** T01,T02
**涉及文件** `src/render.rs`

**目标** 每帧堆分配从 66 次降到 1 次（探针已复现 66 到 1）。

**现状** `RowPainter::flush`（约 592-602 行）用 `std::mem::take(self.run)` 把 String 整个搬走再 `Print(text)`，缓冲区容量被丢弃，下一次 `push_str` 重新分配。`Scratch` 的注释（约 22-23 行）声称缓冲跨帧复用，实际没有。

**改动步骤** 把 `flush` 改为：
```rust
fn flush(&mut self) -> io::Result<()> {
    if self.run.is_empty() {
        return Ok(());
    }
    match self.style {
        Some(_) => queue!(self.writer, Print(&self.run[..]), ResetColor)?,
        None => queue!(self.writer, Print(&self.run[..]))?,
    }
    // Borrow and clear rather than `mem::take`: the buffer's capacity must
    // survive into the next run, or every row reallocates.
    self.run.clear();
    Ok(())
}
```
`queue!` 展开为 `Ok($writer.by_ref()).and_then(|writer| queue(writer, $command))...`，`self.writer` 的再借用与 `&self.run[..]` 是不可变借用，二者是 `self` 的不相交字段，可编译。若仍报借用错误，改用解构写法：
```rust
let Self { writer, run, style } = self;
match *style {
    Some(_) => queue!(writer, Print(&run[..]), ResetColor)?,
    None => queue!(writer, Print(&run[..]))?,
}
run.clear();
```

**不要做** 不要改 `push`/`new`/`finish`；不要动 `Scratch` 结构。

**验收**
- `cargo test --all-targets` 全绿（`coalesces_a_whole_row_into_one_print` 必须过）。
- `cargo bench --bench render` 输出的 repaint 字节数（full frame 1463/1629/1965/2109 B 等）与改动前完全一致（本任务只改分配，不改输出）。

**回滚** `git checkout -- src/render.rs`

---

## T05 渲染冷路径改用一次外向遍历（P1-A 修正版）

**优先级** P1（本次优化收益最大的单点）
**依赖** T01,T02
**涉及文件** `src/core/textobject.rs`、`src/core/mod.rs`、`src/render.rs`

**目标** 渲染路径索引冷时的括号高亮，从"enclosing 扫描 + 4 种括号各一次 fallback 扫描"换成"反向一次 + 正向一次"，消除 O(开符数 × 窗口) 的最坏情况（实测病态语料 4.48 s/帧）。

**背景（必须先读）** 当前 `matching_partner_within`（textobject.rs 约 128-153 行）对光标下的括号调用 `matching_pair_within`（约 92-116 行）：先 `enclosing_pair`，失败后对 4 种括号依次 `next_opening_pair`。后者的结果会被外层过滤器（143-149 行）**必然拒绝**（已证明：fallback 返回的 pair 起点 ≥ 光标+1，两个接受条件都不可能成立）。因此对渲染路径，fallback 是纯浪费，去掉它不改变任何可见行为。`%` 与 textobject 走的是 `matching_pair`/无界路径，**本次不动**。

**改动步骤**

1. 在 `src/core/textobject.rs` 的 `matching_partner_within` 之后新增（保持为 `pub`）：
   ```rust
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
           '"' | '\'' | '`' => return matching_quote_partner(document, position, character),
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
   ```
2. `src/core/mod.rs`：把 `matching_partner_outward` 加进 `pub use textobject::{...}` 列表（约 21-24 行）。
3. `src/render.rs` 的 `matching_partner_for_render`（约 615-633 行）：冷分支从
   `wed::core::matching_partner_within(&app.editor.document, app.editor.cursor, wed::core::PARTNER_SCAN_LIMIT)`
   改为
   `wed::core::matching_partner_outward(&app.editor.document, app.editor.cursor, wed::core::PARTNER_SCAN_LIMIT)`。
   热索引分支（`matching_partner`）与 `Mode::Normal` 判断保持不变。
4. 在 `src/core/textobject.rs` 的 `mod tests`（约 699 行起）末尾追加等价性测试。**`matching_partner_within` 是本任务的参考实现，不许改**：
   ```rust
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
   ```
   若 debug 下穷举测试超过约 10 秒，把各字母表的 `max_len` 从 6 降到 5（保留第一个字母表为 6），不得删测试。

**不要做**
- 不要改 `matching_pair_within`、`matching_pair`、`matching_partner_within`、`enclosing_pair_scan`、`next_opening_pair_scan`、`matching_close`。
- 不要把外向遍历接进 `%` 的路径（`motion.rs` 的 `MatchingPair` 经 `matching_pair` 有"未配对时跳到下一对"的语义，改它会变行为）。
- 不要改 `benches/`。

**验收**
- `cargo test --all-targets` 全绿，含两个新测试。
- `cargo bench --bench render`：`matching: matching_partner_for_render, unclosed` 行 p50 至少降到改动前的 1/3（改动前约 0.59-1.2 ms；预期 0.1-0.3 ms，窗口尚未收紧）。记录实际数字。
- render 的既有测试（`draws_selection_and_matching_partner_with_colour`、`enables_matching_partner_highlight_only_in_normal_mode`）通过。

**回滚** `git checkout -- src/core/textobject.rs src/core/mod.rs src/render.rs`

**与方案的差异（重要）** 方案 P1-A 的原型有以下问题，本任务已修正（第 4 点由穷举对拍额外发现）：
1. 反向走查缺少 `index < position` 守卫，光标在闭符上会返回 None 或外层对（已用探针实证）。上面的代码有该守卫。
2. 方案称"两次遍历共用 running parity、消灭 is_escaped"。这是错的：反向遍历到达某字符时，它"前面"的反斜杠尚未访问，running parity 算的是反斜杠在"后面"的奇偶，方向相反。反向腿必须用 `is_escaped`（每次只走很短的连续反斜杠），正向腿才用 parity。
3. 方案字面改 `matching_pair_within`，会连带改变 `%` 的语义；本任务收窄为渲染冷路径专用新函数。
4. 正向走查命中配对后必须复刻外层过滤器的判定：只有光标恰好落在该对的端点上（开符或闭符）才返回伙伴，否则返回 None。光标落在被转义的分隔符上时会走到这一支；上面的代码已包含该判定，穷举测试覆盖这类输入。

---

## T06 渲染配对窗口从 64 KiB 收紧到 4 KiB（P1-B）

**优先级** P1
**依赖** T05
**涉及文件** `src/core/textobject.rs`、`docs/perf-baseline.md`

**目标** 渲染冷路径每帧成本与语料密度彻底脱钩：窗口 65536 到 4096 后，最坏帧成本从约 0.1-0.3 ms 降到几十微秒。

**改动步骤**
1. `src/core/textobject.rs` 约 83 行：`pub const PARTNER_SCAN_LIMIT: usize = 1 << 16;` 改为 `1 << 12`。同时更新上方文档注释（约 73-83 行，现文提到 "65536" 与 64 KiB 语义）：说明这是**渲染高亮的放弃距离**，收紧到 4 KiB 的理由是"高亮是装饰，全部成本与窗口成正比，每帧几微秒优于跨文档的精确"。
2. 既有测试 `a_bounded_partner_search_gives_up_past_the_window` 与 `the_unbounded_partner_search_is_the_default` 都用 `PARTNER_SCAN_LIMIT + 4096` 构造距离，随常量自适应，应直接通过；若有断言硬编码 65536 或具体距离，改为跟随常量。
3. 在 `docs/perf-baseline.md` 末尾追加一节（中文，风格与既有附录一致）：`## 附录：渲染括号高亮改外向遍历并收紧窗口（2026-10-10）`。内容包括：渲染冷路径改为一次反向 + 一次正向的有界遍历（`matching_partner_outward`）；`PARTNER_SCAN_LIMIT` 由 65536 收紧为 4096；语义等价由穷举对拍测试保证；附上 T05 与 T06 前后 `cargo bench --bench render` 的 `unclosed` 行实测数字（p50/p95），并注明"窗口外的配对不再高亮，这是有界查询已声明的语义"。

**不要做** 不要改 `matching_partner`（无界精确查询）与 `%` 的行为；不要把常量改到 4096 以外。

**验收**
- `cargo test --all-targets` 全绿。
- `cargo bench --bench render`：`matching_partner_for_render, unclosed` 行 p50 < 100 µs（预期 10-40 µs）。记录数字写入基线文档。

**回滚** `git checkout -- src/core/textobject.rs docs/perf-baseline.md`

---

## T07 matching_quote_partner 接受并应用 limit（P3-B）

**优先级** P1
**依赖** T05,T06
**涉及文件** `src/core/textobject.rs`

**目标** 引号分支不再无视窗口。现状：`matching_partner_within` 对 `"`/`'`/`` ` `` 调用 `matching_quote_partner`（约 155-177 行）时不传 limit，后者扫完整行；压缩 JS / 单行 JSON 上这是每帧 O(行)（约 128-153 行的调用点，139 行）。

**改动步骤**
1. 函数签名改为 `fn matching_quote_partner(document: &Document, position: usize, quote: char, limit: usize) -> Option<CharPos>`。
2. 扫描循环（约 160 行）加窗口：`for (at, character) in document.chars_from(CharPos(start)).take(limit) {`（保留行尾 `break`）。
3. 两个调用点都传各自收到的 limit：`matching_partner_within`（约 139 行）传它的 `limit` 参数；T05 新增的 `matching_partner_outward` 的引号分支传它的 `limit` 参数。
4. 在 textobject 测试模块追加一个测试：单行文档里引号对位于 limit 之内时返回伙伴、之外时返回 None：
   ```rust
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
   ```

**不要做** 不要改引号配对算法（按行内位置两两配对）本身；不要动 bracket 分支。

**验收** `cargo test --all-targets` 全绿（含 T05 的穷举测试，它对引号字母表同样成立）。
**回滚** `git checkout -- src/core/textobject.rs`

---

## T08 索引 worker 改为尾沿防抖，打字期间不再重建（P2-A 修正版）

**优先级** P2（消除打字时约 29-37% 的核心占用）
**依赖** T02
**涉及文件** `src/core/document.rs`

**目标** 连续打字期间后台不再做任何索引重建；停止输入约 250 ms 后自动建好，暂停后的 `%` 与 `i(` 依然拿到热索引（现状经 `try_promote_async` 已是热查询）。

**背景（必须先读）** 方案 P2-A 提议"changed() 不再投递 + 前台静默期门闩（用 pair_index_warm 作轮询点）"。该机制有洞：主循环阻塞在 `event::read`（`src/main.rs`），帧只在事件后渲染，`pair_index_warm` 的唯一调用点在渲染里；打字停止后没有任何轮询点，门闩永远不触发，索引不会在空闲期建好，"暂停后 `%` 是热索引"反而会退化成按下时才开始建（50 MiB 上数十毫秒）。正确做法是把防抖放进 worker：尾沿防抖不需要前台轮询，空闲期由 worker 自己的超时触发。

**改动步骤**
1. 在 `document.rs` 顶部常量区新增：
   ```rust
   /// How long the index worker waits for the typing to stop before rebuilding.
   /// A trailing debounce on the worker side: every new snapshot resets it, so
   /// continuous typing never rebuilds, and a pause of this length rebuilds
   /// once without any foreground polling.
   const INDEX_REBUILD_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
   ```
2. 重写 worker 循环（`ensure_index_worker` 内，约 116-133 行）为：
   ```rust
   .spawn(move || {
       while let Ok((mut rev, mut rope)) = jobs.recv() {
           // Trailing debounce: absorb newer snapshots until the typing has
           // stopped for INDEX_REBUILD_DEBOUNCE. A disconnected channel means
           // the document is gone, so exit without building.
           loop {
               match jobs.recv_timeout(INDEX_REBUILD_DEBOUNCE) {
                   Ok((newer_rev, newer_rope)) => {
                       rev = newer_rev;
                       rope = newer_rope;
                   }
                   Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                   Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
               }
           }
           let document = Document::from_rope(rope);
           let index = PairIndex::build(&document);
           if results.send((rev, index)).is_err() {
               return;
           }
       }
   })
   ```
   需要 `use std::sync::mpsc::RecvTimeoutError;`（或写全路径）。
3. `changed()`（66-76 行）**保持投递不变**，只把注释（69-71 行）改为说明"worker 会做尾沿防抖，这里投递只是标记最新状态"。`pair_index()` 的同步构建冷路径（179-196 行）保持不变。
4. 在 `document.rs` 的测试模块新增（验证防抖不是无限期）：
   ```rust
   #[test]
   fn the_index_worker_rebuilds_after_the_typing_stops() {
       let mut document = Document::from_text("fn f() { (a) }");
       document.insert_str(CharPos(0), "x");
       let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
       while !document.pair_index_warm() {
           assert!(
               std::time::Instant::now() < deadline,
               "the index never became warm after the debounce"
           );
           std::thread::sleep(std::time::Duration::from_millis(25));
       }
   }
   ```

**不要做** 不要引入前台轮询、计时器线程或新的通道；不要改 `try_promote_async`；不要动渲染路径。

**验收**
- `cargo test --all-targets` 全绿，含新测试（5 秒预算内变热）。
- 可选交叉验证（若 `/tmp/wedprobe` 存在）：`cd /tmp/wedprobe/target/release && (./tap 50 200 6 & pid=$!; sleep 5; ps -p $pid -o time,%cpu; wait $pid)`，%CPU 应从约 30% 量级降到个位数。探针不存在则跳过。

**回滚** `git checkout -- src/core/document.rs`

**与方案的差异** 机制整体替换：不做前台门闩（空闲无轮询点，且会丢掉现状"暂停后已热"的优点），改为 worker 侧 `recv_timeout` 尾沿防抖；`changed()` 继续投递。

---

## T09 PairIndex 重设计：u32 存储 + parent 链 + O(log) 查询（P2-B+P2-C）

**优先级** P2
**依赖** T01,T02
**涉及文件** `src/core/pairing.rs`、`src/core/document.rs`

**目标** 三件事一次完成：(a) 位置存储 usize 到 u32，内存约减半；(b) `enclosing_at` 与 `first_balanced_pair` 去掉二分后的线性回溯（未匹配开符串、已匹配兄弟串两个最坏情形都变 O(log)，深嵌套残留 O(嵌套深度)，可接受）；(c) 对 4 Gi 字符以上文档的溢出守卫。

**背景** 现状（`pairing.rs` 约 40-221 行）：每类括号两个 `Vec<usize>`（`opens`/`close_of`），`UNMATCHED = usize::MAX`；`enclosing_at`（160-175 行）从最后一个开符向前逐个回走，`first_balanced_pair`（193-205 行）向后逐个走。50 MiB 密集语料实测 6,765,008 个开符、索引 103 MiB。

**新结构**（只存**已匹配**的对；未匹配开符不存储，查询天然跳过它们）：
- `pairs: [Vec<(u32, u32)>; 4]`：每类的 (open, close)，按 open 升序。
- `parents: [Vec<u32>; 4]`：与 pairs 平行，每个对的"最近包含对"在 pairs 中的下标，`NO_PARENT = u32::MAX` 表示无。
- `open_counts: [usize; 4]`：每类**所有**开符数（含未匹配），供 `open_count` 报告。

**改动步骤**

1. 结构体与常量（替换约 44-56 行）：
   ```rust
   /// Stands in for "this pair has no containing pair".
   const NO_PARENT: u32 = u32::MAX;

   /// Delimiter pairing for one revision of a document.
   ///
   /// Per kind, `pairs` holds every *matched* pair as `(open, close)` sorted by
   /// `open`, and `parents` holds for each entry the index of the innermost
   /// pair containing it. Openers that never found a partner are not stored at
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
   ```
   删除 `UNMATCHED` 常量。
2. `build`：开头加守卫；扫描循环保持现有 byte/char 两分支结构，只改 push/pop 体：
   ```rust
   pub fn build(document: &Document) -> Self {
       assert!(
           document.len_chars() < u32::MAX as usize,
           "the pair index supports documents under 4 Gi characters; \
            callers fall back to the scans beyond that"
       );
       let mut pairs: [Vec<(u32, u32)>; PAIR_KINDS.len()] = Default::default();
       let mut stacks: [Vec<u32>; PAIR_KINDS.len()] = Default::default();
       let mut open_counts = [0usize; PAIR_KINDS.len()];
       // ... 现有 backslashes / chunk_byte_start / chunk_char_start 与双分支扫描不变 ...
       // 开符处（两分支同样）：
       //     pairs[index].push((position as u32, u32::MAX));
       //     stacks[index].push((pairs[index].len() - 1) as u32);
       //     open_counts[index] += 1;
       // 闭符处（两分支同样）：
       //     if let Some(pair) = stacks[index].pop() {
       //         pairs[index][pair as usize].1 = position as u32;
       //     }
       // 扫描结束后：
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
           pairs[kind].shrink_to_fit();
           parents[kind].shrink_to_fit();
       }
       Self { pairs, parents, open_counts }
   }
   ```
   （删除原来的 `shrink_to_fit` 循环与 `Self { opens, close_of }`。）
3. 查询（替换 160-221 行的五个方法）：
   ```rust
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

   pub fn opener_pair_at(&self, kind: PairKind, position: usize) -> Option<(usize, usize)> {
       let index = slot(kind);
       let key = u32::try_from(position).ok()?;
       let found = self.pairs[index]
           .binary_search_by_key(&key, |(open, _)| *open)
           .ok()?;
       let (open, close) = self.pairs[index][found];
       Some((open as usize, close as usize))
   }

   pub fn first_balanced_pair(&self, kind: PairKind, from: usize) -> Option<(usize, usize)> {
       let index = slot(kind);
       let candidate = self.pairs[index]
           .partition_point(|(open, _)| (*open as usize) < from);
       let (open, close) = *self.pairs[index].get(candidate)?;
       Some((open as usize, close as usize))
   }

   pub fn pairs(&self, kind: PairKind) -> Vec<CharRange> {
       let index = slot(kind);
       self.pairs[index]
           .iter()
           .map(|(open, close)| {
               CharRange::new(CharPos(*open as usize), CharPos(*close as usize + 1))
           })
           .collect()
   }

   pub fn open_count(&self, kind: PairKind) -> usize {
       self.open_counts[slot(kind)]
   }
   ```
   方法名、签名、返回类型全部保持与现状一致（调用点不用改）。
4. 更新模块文档（文件头 1-26 行）：把"两个平行数组 + UNMATCHED"的描述改为新结构，写明 4 Gi 字符上限与内存口径（每匹配对 12 字节；匹配密集的 50 MiB 语料约 77 MiB，比旧的 103 MiB 小，但高于原方案"两个 u32 数组"的 51 MiB 目标，代价换来 O(log) 查询与 parent 链）。
5. 溢出守卫接到调用侧（`build` 的 assert 之上再加两道，保证生产路径永不触发）：
   - `pairing.rs` 的 `enclosing_pair`（约 242-259 行）与 `next_opening_pair`（约 263-274 行）的扫描分支条件改为 `if limit != usize::MAX || document.len_chars() >= u32::MAX as usize { return ...scan(...); }`。
   - `document.rs` 的 `pair_index_warm`（约 171-177 行）改为：
     ```rust
     pub fn pair_index_warm(&self) -> bool {
         self.try_promote_async();
         self.len_chars() < u32::MAX as usize
             && self
                 .pairs
                 .borrow()
                 .as_ref()
                 .is_some_and(|(tag, _)| *tag == self.mutation)
     }
     ```
6. 测试模块（约 276-470 行）：删除 `UNMATCHED` 的 import（约 278 行）与 `let _ = UNMATCHED;`（约 468 行）。其余测试（`unmatched_openers_are_recorded_as_having_no_partner`、`nested_pairs_are_nested`、`kinds_do_not_interfere`、`escaped_delimiters_are_literal`、`the_index_agrees_with_the_scan_on_random_input`、`the_cache_is_rebuilt_after_an_edit` 等）**全部保持原样且必须通过**——它们固定了新实现要复刻的语义。

**不要做** 不要改 `is_opener_at`、`slot`、`PAIR_KINDS`；不要改 `pairing::enclosing_pair`/`next_opening_pair` 的索引分支逻辑（只加守卫）；不要动 `benches/`。

**验收**
- `cargo test --all-targets` 全绿（尤其随机对拍测试）。
- `cargo bench --bench alloc` 的 `pairing: index resident after build` 行比改动前下降至少 15%（改动前 code/1 MiB 约 346 KiB；预期 200-270 KiB）。
- 可选：50 MiB 密集语料索引内存从 103 MiB 降到约 77 MiB（用 `/tmp/wedprobe` 的 idxmem 思路，或 `WED_BENCH_SCALE=full cargo bench --bench alloc` 观察 pairing 行）。

**回滚** `git checkout -- src/core/pairing.rs src/core/document.rs`

**与方案的差异** 合并了 P2-B 与 P2-C（分开做会让 P2-C 的 parent 数组把 P2-B 省下的内存吃回去）；修正 P2-C 的机制描述："前一个已匹配开符"修不了已匹配兄弟串（`()()()…` 光标在文末仍要回走全部），正确结构是"最近包含对"parent 链；内存预期从 51 MiB 修正为约 12 字节/匹配对。

---

## T10 next_opening_pair_scan 单趟栈式重写 + 参考实现对拍（P2-D 修正版）

**优先级** P2（T05 之后它已不在渲染热路径上，但仍是公共 API 有界查询的 O(N²) 地雷）
**依赖** T05,T09
**涉及文件** `src/core/textobject.rs`

**目标** 把 `next_opening_pair_scan`（约 584-632 行）从"每个开符各自重扫一遍"的 O(开符数 × 窗口) 改成单趟栈式 O(2×窗口)，语义与旧实现完全一致。

**背景（必须先读）** 旧语义（两阶段）：候选 = 窗口中每个未转义开符（位置在 `[start, start+limit)`）；对每个候选，从它自身出发做深度计数（窗口 `limit` 从开符起算），**按位置顺序取第一个能配上的**开符，返回其 (open, close)。方案原文说"开符入栈、闭符出栈、第一个出栈的就是首对"——这在 `(())` 上错误：先出栈的是内层 (1,2)，旧语义要的是外层 (0,3)。正确规则是：**在"被弹出的开符"里取位置最小者**。另外窗口语义要按旧实现复刻：开符在 `[start, start+limit)` 内、且其闭符距开符 < limit 才计入，因此扫描要看到 `[start, start+2*limit)`。

**改动步骤**
1. 把旧实现整体改名保留为测试参考（加 `#[cfg(test)]`）：
   ```rust
   #[cfg(test)]
   fn next_opening_pair_scan_reference(
       document: &Document,
       cursor: CharPos,
       kind: PairKind,
       limit: usize,
   ) -> Option<(usize, usize)> {
       // 旧实现原样搬到这里（含 matching_close 调用）
   }
   ```
   `matching_close`（约 605-632 行）只被它使用，同样加 `#[cfg(test)]`。
2. 新实现替换原 `next_opening_pair_scan`：
   ```rust
   pub(crate) fn next_opening_pair_scan(
       document: &Document,
       cursor: CharPos,
       kind: PairKind,
       limit: usize,
   ) -> Option<(usize, usize)> {
       let (open_character, close_character) = delimiters_for(kind);
       let start = cursor.0.min(document.len_chars());
       // One forward pass. An opener's pair is reported only if the pair
       // completes inside the window, and the answer is the completed pair
       // with the smallest opener position -- exactly what the old two-phase
       // scan returned. The scan reaches 2 x limit because the old form let a
       // pair's closer sit up to `limit` past its own opener.
       let opener_window_end = start.saturating_add(limit);
       let scan_end = start.saturating_add(limit.saturating_mul(2));
       let mut stack: Vec<usize> = Vec::new();
       let mut best: Option<(usize, usize)> = None;
       // Seed the escape parity from the backslash run just before the
       // window: the old scan judged every opener with `is_escaped`, which
       // looks before `start`, and a window that begins mid-run must give its
       // first characters the same status.
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
   ```
   （若工具链的 `Option::is_none_or` 不可用，用 `best.map_or(true, |(best_open, _)| open < best_open)`。）
3. 在测试模块追加对拍（穷举，含有界与无界）：
   ```rust
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
                               next_opening_pair_scan_reference(document, CharPos(from), kind, limit),
                               "from {from}, limit {limit}, kind {kind:?}, text {:?}",
                               document.to_string(),
                           );
                       }
                   }
               }
           });
       }
   }
   ```
   （`for_each_short_document` 由 T05 引入；若 T05 未做则先补上同一 helper。debug 下若超过 15 秒，把 `max_len` 降到 5。）

**不要做** 不要改 `enclosing_pair_scan`（它本来就是一次反向 + 一次正向，无此问题）；不要改 `pairing.rs` 的对拍测试（它现在会同时检验索引与新扫描在无界下的等价，是额外保险）。

**验收**
- `cargo test --all-targets` 全绿，含新对拍与既有的 `the_index_agrees_with_the_scan_on_random_input`。
- 可选：`/tmp/wedprobe/target/release/hot` 的 `bounded render scan, unmatched openers` 行从约 4.5 s 降到 1 ms 以内（探针存在时）。

**回滚** `git checkout -- src/core/textobject.rs`

**与方案的差异** 修正了"第一个出栈即首对"的错误规则（`(())` 反例），补全了窗口语义（开符窗口 + 闭符距开符 limit），并保留旧实现作为对拍参考。另外补了窗口起点的转义奇偶种子：旧实现用 `is_escaped` 能看到窗口之前的反斜杠，单趟实现必须显式把窗口前那段反斜杠计入 parity，否则窗口从反斜杠串中间开始时，首个字符的转义判定会错（穷举对拍已抓到并验证修复）。

---

## T11 FirstNonBlank 改用 chars_from（P3-A）

**优先级** P3
**依赖** T01,T02
**涉及文件** `src/core/motion.rs`

**现状** `Motion::FirstNonBlank`（约 54-72 行）用 `(start.0..end.0).find(|index| document.char_at(CharPos(*index))...)`，逐字符做 O(log N) rope 下降，违反 `document.rs` 自己写下的指引（约 344-349 行：扫描应走 `chars_from`）。

**改动步骤** 改为：
```rust
Motion::FirstNonBlank => {
    let line = document.pos_to_line_col(cursor).line;
    let start = document.line_start(line);
    let end = document.line_end(line);
    let target = document
        .chars_from(start)
        .take_while(|(position, _)| position.0 < end.0)
        .find(|(_, character)| !character.is_whitespace())
        .map(|(position, _)| position)
        .unwrap_or(start);
    result(
        target,
        CharRange::new(target, cursor),
        RangeKind::Characterwise,
        false,
    )
}
```

**验收** `cargo test --all-targets` 全绿（motion 测试覆盖 `^` 行为）。
**回滚** `git checkout -- src/core/motion.rs`

---

## T12 substitute 提前释放整份文本拷贝（P3-C）

**优先级** P3
**依赖** T01,T02
**涉及文件** `src/core/editor.rs`

**现状** `substitute`（约 470-508 行）：`let text = self.document.to_string();`（471 行）最后使用于 482 行 `to_char_ranges(&text, &byte_ranges)`，但值活到函数末尾（508 行），编辑循环期间白扛一份文档拷贝。注释（467-468 行）声称 "The text is released as soon as the match positions are known"，与代码不符。

**改动步骤**
1. 在 482 行 `let ranges = to_char_ranges(&text, &byte_ranges);` 之后加：
   ```rust
   // The ranges are owned now, so the whole-text copy can go before the
   // edit loop rather than lingering to the end of the function.
   drop(text);
   ```
2. 修正 464-469 行的注释：把 "The text is released as soon as the match positions are known" 改为与代码一致的表述（说明位置确定后即显式 `drop(text)`）。

**不要做** 不要改 `to_char_ranges`、`match_ranges` 或编辑循环；不要动 `byte_ranges` 的使用。

**验收** `cargo test --all-targets` 全绿（替换相关测试全部覆盖此路径）。
**回滚** `git checkout -- src/core/editor.rs`

---

## T13 Transaction 缓存字节总数（P3-D）

**优先级** P3
**依赖** T01,T02
**涉及文件** `src/core/history.rs`

**现状** `Transaction::bytes()`（约 51-58 行）每次调用重算全部 edit 的字节数，在 `push_undo`（195 行）、redo 清空（198 行）、`enforce_budget`（214 行）反复触发；十万级 edit 的替换事务每次重算十万次迭代。

**改动步骤**
1. `Transaction` 增加字段（约 46-49 行）：
   ```rust
   #[derive(Clone, Debug, PartialEq, Eq)]
   pub struct Transaction {
       pub edits: Vec<Edit>,
       /// Total bytes of undoable text, maintained on push so no caller has
       /// to walk the edit list again.
       pub bytes: usize,
   }
   ```
2. 删除 `impl Transaction { fn bytes(&self) ... }`（51-58 行）。
3. 两处构造点：
   - `begin()`（约 108 行）：`Transaction { edits: Vec::new(), bytes: 0 }`。
   - `record()` 的 else 分支（约 116 行）：
     ```rust
     let bytes = edit.removed.len() + edit.inserted.len();
     let transaction = Transaction { edits: vec![edit], bytes };
     ```
   - `record()` 的 Some 分支（约 112-114 行）在 push 前累加：
     ```rust
     transaction.bytes += edit.removed.len() + edit.inserted.len();
     transaction.edits.push(edit);
     ```
4. 三个使用点改为字段：195 行 `transaction.bytes`、198 行 `abandoned.bytes`、214 行 `oldest.bytes`。

**不要做** 不要改 undo/redo 的移动逻辑（Transaction 在栈间移动不改变 bytes，天然正确）；不要动 `held` 的语义。

**验收** `cargo test --all-targets` 全绿（history 的预算测试固定了语义）。
**回滚** `git checkout -- src/core/history.rs`

---

## T14 寄存器文本改为 Rc\<str\>（P3-E）

**优先级** P3
**依赖** T01,T02
**涉及文件** `src/core/register.rs`、`src/core/editor.rs`、`src/app.rs`

**目标** 去掉每次 yank/delete 的寄存器深拷贝：寄存器与剪贴板待发文本共享同一份 `Rc<str>`（同一文本原先 clone 两次，现在只分配一次）。

**改动步骤**
1. `register.rs`：
   - 顶部 `use std::rc::Rc;`；`RegisterValue.text: Rc<str>`（约 22-25 行）。
   - 构造器保持接收 `String`，内部转换（`impl From<String> for Rc<str>` 存在）：
     ```rust
     pub fn new(text: String, kind: RegisterKind) -> Self {
         Self { text: Rc::from(text), kind }
     }
     ```
     `characterwise` 不变（走 `new`）。
   - 测试中 `registers.unnamed().map(|v| v.text.as_str())`（约 150 行）改为 `.map(|v| &*v.text)`（`Rc<str>` 没有 `as_str`）。
2. `editor.rs`：
   - 顶部加 `use std::rc::Rc;`。
   - `pending_clipboard_text` 字段（约 231 行）与 `take_clipboard_text` 返回类型（约 384-386 行）改为 `Option<Rc<str>>`。
   - `write_paste_text`（1290 行）与 `insert_at_cursor`（1302 行）的 `let mut text = value.text;` 改为 `let mut text = value.text.to_string();`（粘贴路径需要可变 String，粘贴是低频操作，一次拷贝可接受）。
   - 五处 `RegisterValue { text: ..., kind }` 构造（约 1470、1485、1501、1571、1595 行）：`text: text.clone()` 改为 `text: Rc::from(text.as_str())`（若该 String 之后不再使用，也可 `Rc::from(text)` 移动；以编译器提示为准）。`write_register` 的 `value.text.clone()`（1532 行）不变（现在克隆的是 Rc）。
   - `read_register` 的 `.cloned()`（1236 行）不变（Rc 克隆）。
   - 所有对 `take_clipboard_text()` 返回值的测试断言（用 `grep -rn "take_clipboard_text" src` 找全）改为与 `&str` 比较，例如 `assert_eq!(editor.take_clipboard_text().as_deref(), Some("foo"));`。
3. `app.rs`：`set_clipboard(&text)`（约 125 行）保持原样（`&Rc<str>` 会在调用处自动解引用为 `&str`）；若编译失败改为 `set_clipboard(&*text)`。

**不要做** 不要改 `clipboard.rs`（它接收 `&str` 后本就拷贝进通道）；不要动寄存器种类与黑洞语义。

**验收** `cargo test --all-targets` 全绿（寄存器、粘贴、剪贴板相关测试全部覆盖）。
**回滚** `git checkout -- src/core/register.rs src/core/editor.rs src/app.rs`

---

## T15 find_first_match 从 from 精确偏移开始（P3-F）

**优先级** P3
**依赖** T01,T02
**涉及文件** `src/core/editor.rs`

**现状** `find_first_match`（约 2094-2124 行）：`chunk = (from / SEARCH_CHUNK) * SEARCH_CHUNK` 从块边界开始，首块把 `from` 之前的内容整块写进 buffer 再走 `skip` 逐个字符跳过（最多 64 Ki 字符）；匹配不可能开始于 `from` 之前（开始于之前的匹配本就会被排除），这段是纯浪费。

**改动步骤** 把循环改为从 `from` 起步、块间重叠照旧：
```rust
fn find_first_match(
    document: &Document,
    query: &str,
    from: usize,
    buffer: &mut String,
) -> Option<CharPos> {
    let len = document.len_chars();
    let needle = query.chars().count();
    buffer.clear();
    buffer.reserve(SEARCH_CHUNK + needle);
    // Start exactly at `from`: a match that begins before it is not wanted, so
    // there is nothing to read or skip before it.
    let mut position = from;
    while position < len {
        // Overlap by the needle so matches spanning a boundary stay visible.
        let end = (position + SEARCH_CHUNK + needle - 1).min(len);
        buffer.clear();
        document.write_slice(CharRange::new(CharPos(position), CharPos(end)), buffer);
        if let Some(found) = buffer.find(query) {
            return Some(CharPos(position + buffer[..found].chars().count()));
        }
        position += SEARCH_CHUNK;
    }
    None
}
```
同时更新该函数上方注释（约 2085-2093 行，描述"从块边界开始 + earliest skip"的部分已不成立）。

**不要做** 不要动 `find_last_match`；不要改 `SEARCH_CHUNK`；不要改 `buffer` 的复用方式。

**验收** `cargo test --all-targets` 全绿（搜索相关测试）；`cargo bench --bench edit` 的 `search:` 行不劣化（±10% 以内）。
**回滚** `git checkout -- src/core/editor.rs`

---

## T16 tag_object：游离 `<` 不再中断整个搜索 + 复用缓冲（P4-A 收窄）

**优先级** P4
**依赖** T01,T02
**涉及文件** `src/core/textobject.rs`

**现状** `tag_object`（约 348-405 行）：遇到第一个没有 `>` 的 `<` 就 `break`（370-372 行），一个游离 `<` 会让整个搜索静默返回 None；每个标签分配一个 `body` String。

**改动步骤**
1. 常量：在函数上方加 `const TAG_BODY_LIMIT: usize = 1024;`。
2. 标签体扫描加界：内层 `for (position, inner) in chars.by_ref()` 改为最多读 `TAG_BODY_LIMIT` 个字符（用 `chars.by_ref().take(TAG_BODY_LIMIT)` 形式，注意先 `chars.by_ref()` 再 `take`）；若在界内没有找到 `>`，**不要 break**，改为 `continue`（外层循环从迭代器当前位置继续，即跳过这段已消费的 `>`-free 区域）。在注释中说明：超过 1024 字符仍未闭合的 `<` 按字面文本处理，其后续至多 1024 字符内不会再被当作标签起点（有界放弃）。
3. 把 `body` 的 String 提升到循环外复用：`let mut body = String::new();` 放在 `while let Some(...)` 之前，每个标签开始时 `body.clear();`，`body.push(inner)` 照旧。
4. 追加测试：
   ```rust
   #[test]
   fn a_stray_angle_bracket_does_not_hide_later_tags() {
       // The leading `<` never closes; the search must still find the pair
       // after it instead of aborting.
       let document = Document::from_text("<broken <a>text</a>");
       let cursor = CharPos(document.to_string().find("text").unwrap());
       let range = super::resolve(
           &document,
           cursor,
           TextObject::Tag { around: false },
           1,
       );
       assert!(range.is_some(), "a stray `<` must not abort the search");
   }
   ```
   （`TextObject::Tag` 与 `resolve` 均为本模块可见；断言只要 Some 即可，具体范围语义由既有 `selects_quotes_and_tags` 覆盖。）

**不要做** 不要改标签匹配算法（栈、rposition、大小写折叠、自闭合判断）；不要改从位置 0 起扫的行为（那是 T17 的范围）。

**验收** `cargo test --all-targets` 全绿，含新测试。
**回滚** `git checkout -- src/core/textobject.rs`

**与方案的差异** 方案 P4-A 想要"从光标双向扫描"的整体重写；本任务先做低风险部分（正确性悬崖 + 分配），完整重写见 T17（可选）。

---

## T17 tag_object 双向扫描重写（P4-A 完整版）

**优先级** P4（可选：低能力执行者建议跳过，不影响其它任务）
**依赖** T16
**涉及文件** `src/core/textobject.rs`

**目标** `tag_object` 从"位置 0 全扫"改为"从光标反向找最近的未闭合开标签，再正向找其配对闭标签"，工作量与标签间距成正比而非文档大小。

**做法要点**
1. 把现实现整体保留为 `#[cfg(test)] fn tag_object_reference(...)`（原样搬移，不改逻辑），作为对拍参考。
2. 新实现（算法骨架）：
   - 反向阶段：用 `chars_before(cursor + 1)` 扫描；维护一个 `Vec<String>` 的"待配平栈"与标签体缓冲。反向遇到 `>` 时记为候选结束位，继续向左找 `<`，取出标签体（需要反向收集再反转或先记录字节区间）；按与参考实现相同的分类规则（`trim`、去前导 `/` 判闭标签、去尾随 `/` 判自闭合、首词小写为名字）。闭标签把名字压入待配平栈；开标签：若名字在待配平栈中则弹出（它由更右的闭标签配对），否则它就是我们要找的开标签，进入正向阶段。
   - 正向阶段：从该开标签之后，用 `chars_from` 正向扫描，按与参考实现相同的规则配平（对同名闭标签用"最近同名开标签"配对，即参考实现 `stack.iter().rposition` 的语义），找到包含光标的第一个"已闭合且包含光标"的标签对，返回与参考实现相同的 `CharRange`（`around` 与否两种形态）。
   - 反向/正向的标签体扫描都受 `TAG_BODY_LIMIT` 约束，游离 `<`/`>` 的处理与 T16 相同。
3. 对拍测试（必须，`for_each_short_document` helper 由 T05 引入，若不存在先补上同一 helper）：
   ```rust
   #[test]
   fn the_bidirectional_tag_search_agrees_with_the_reference() {
       for alphabet in [['<', '>', '/', 'a', 'b', ' ']] {
           for_each_short_document(&alphabet, 5, &mut |document| {
               let len = document.len_chars();
               for cursor in 0..=len {
                   for around in [false, true] {
                       assert_eq!(
                           tag_object(document, CharPos(cursor), around),
                           tag_object_reference(document, CharPos(cursor), around),
                           "cursor {cursor}, around {around}, text {:?}",
                           document.to_string(),
                       );
                   }
               }
           });
       }
   }
   ```
   字母表长度 5 穷举为 6^5=7776 份文档，若加 `['a','<','>','/',' ','"']` 仍应秒级完成；再加 2000 份长度 12 的固定种子随机文档（xorshift，种子写死）。
4. **退出条件**：若对拍无法在合理时间内做到全绿（标签分类规则的细节差异），回滚本任务，保留 T16 的成果，并在笔记中记录差异样例。不允许放宽对拍（例如跳过 None 比较）。

**验收** 对拍测试全绿；既有 tag 测试（`selects_quotes_and_tags`）全绿。
**回滚** `git checkout -- src/core/textobject.rs`

---

## T18 删除 replace_literal 并迁移测试/基准（P4-B）

**优先级** P4（可选）
**依赖** T01,T02
**涉及文件** `src/core/editor.rs`、`src/app.rs`、`benches/edit.rs`、`benches/alloc.rs`

**背景** `Editor::replace_literal`（`editor.rs` 约 510 行起）在生产路径不可达：`:s` 的派发（`app.rs` 约 277-288 行）永远走 `Substitution::parse` 的正则路径；实测正则路径还更快（0.067 ms 对 0.462 ms，1 MiB code 语料）。保留它只会继续误导基准。

**改动步骤**
1. `editor.rs`：删除 `replace_literal` 整个函数及其文档注释；修正 459 行附近 `substitute` 的文档里 "Shares the shape of [`Self::replace_literal`]" 的引用（改为描述自身即可）。
2. 迁移测试（约 3027-3112 行的 9 个测试）：把 `editor.replace_literal("a", "b", global)` 换成
   ```rust
   editor.substitute(&Substitution::from_parts("a", "b", global).unwrap())
   ```
   （`Substitution` 已在本文件 import。）注意返回值语义：`replace_literal` 返回"实际替换数"（跳过等值匹配不计），`substitute` 返回"匹配数"（等值匹配也计）。若某断言因此不同（例如等值替换的返回值），把期望值改为 `substitute` 的语义并在提交信息里说明；文档内容断言不变。
3. `app.rs` 测试 `substitution_is_undoable_and_leaves_the_document_clean_when_redundant`（约 484-500 行）中的两次 `replace_literal` 调用同样改为 `Substitution::from_parts(...)` + `substitute`（`assert_eq!(..., 2)` 对两条路径都成立）。
4. `benches/edit.rs`：删除 `bench_replace_literal`（约 383-397 行）、其调用（约 132 行）与其上方文档注释；保留正则替换的基准用例。
5. `benches/alloc.rs`：`bench_substitute`（约 205-221 行）与 `bench_undo`（约 281 行）里的 `editor.replace_literal(&needle, "zz", true)` 改为
   ```rust
   let substitution = wed::core::Substitution::parse(&format!("s/{needle}/zz/g")).unwrap();
   editor.substitute(&substitution)
   ```
   （`bench_substitute` 已有一个正则版本 `bench_regex_substitute_peak`，本项完成后两者语义一致；保留两个名字或合并为一个均可，不要删 `bench_regex_substitute_peak`。）输出行标签保持不变。

**不要做** 不要改 `Substitution`、`to_char_ranges` 或正则路径本身；不要动 `substitute` 的返回语义。

**验收** `cargo test --all-targets` 全绿；`grep -rn "replace_literal" src benches` 只剩注释性文字或零结果；`cargo bench --bench edit` 与 `--bench alloc` 正常跑完。
**回滚** `git checkout -- src/core/editor.rs src/app.rs benches/edit.rs benches/alloc.rs`

---

## T19 README 修正：架构表述、O(1) 措辞与性能数字（P4-C）

**优先级** P4（可选）
**依赖** T06（引用收紧后的最终数字）
**涉及文件** `README.md`

**背景** README 目前宣传 "Gap Buffer architecture" 与未经证明的绝对 O(1) 表述，违反 `docs/tiny-neovim-development-plan.md:543`（性能声明必须有可复现 benchmark 支撑）；性能表与实测双向不符：50 MB 行写 0.45 ms / 232.20 MB，实测约 10.2 ms / 约 58 MB（见 `docs/perf-baseline.md`），且表内 1 MB 0.68 ms 与 10 MB 0.39 ms 自相矛盾。

**改动步骤**
1. 开头介绍段与 Key Features：删除 "Gap Buffer architecture"、"$O(1)$ local text insertion" 等绝对表述，改为事实性描述：文本存储为 rope（ropey），本地编辑在基准中为常数时间（引用 `docs/perf-baseline.md`）。
2. 顶部徽章 `Latency-0.68ms`：移除或改为指向 `docs/perf-baseline.md` 的 "Performance-verified" 徽章（不写具体未验证数字）。
3. Performance Benchmarks 表：替换为与 `docs/perf-baseline.md` 一致的数字并注明出处，例如：1 MiB 加载约 0.2 ms、50 MiB 加载约 10.2 ms（目标 < 1 s）、普通按键 p95 < 0.002 ms、加载后内存约为文件大小的 1.1-1.2 倍、渲染成本与文件大小无关（每帧只画可见行）。每行后注明"数据与复现方式见 docs/perf-baseline.md"。
4. 保留 Security、Installation、作者与 Star 段落不动。

**不要做** 不要改动 `docs/perf-baseline.md` 里已记录的历史数字；不要引入基线文档中不存在的新数字。

**验收** `grep -n "Gap Buffer\|O(1)" README.md` 无未加限定的绝对表述残留；README 中每个性能数字都能在 `docs/perf-baseline.md` 中找到出处。
**回滚** `git checkout -- README.md`

---

## T20 CI 补 cargo audit 与 alloc 基准（P4-D 收窄）

**优先级** P4（可选）
**依赖** T01,T02（alloc 基准在 CI 运行前必须已稳定）
**涉及文件** `.github/workflows/ci.yml`

**改动步骤**
1. 新增审计作业（放在 `benchmark` 之后）：
   ```yaml
     audit:
       name: cargo audit
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - uses: rustsec/audit-check@v2
           with:
             token: ${{ secrets.GITHUB_TOKEN }}
   ```
2. `benchmark` 作业的 Run benchmarks 步骤追加一行 `cargo bench --bench alloc`（T01/T02 之后它已稳定）。
3. 在 `benchmark` 作业的注释里补一句：数字与 `docs/perf-baseline.md` 的对比目前是人工的；自动趋势对比（存基线 + 容差比较）留作后续工作，不在本任务内实现。

**不要做** 不要改现有三个作业的门禁命令；不要引入除 `rustsec/audit-check` 外的第三方 action。

**验收** `ci.yml` 为合法 YAML（本地可用 `python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/ci.yml'))"` 校验，若本机无 PyYAML 则目检缩进）；新作业与新增步骤存在。
**回滚** `git checkout -- .github/workflows/ci.yml`

---

## 21. 全部完成后的总验收

全部任务（或跳过可选任务后）完成后，在仓库根目录执行：

```bash
cargo test --all-targets
WED_BENCH_SCALE=full cargo bench
```

对照下表核对（50 MiB 数字来自评审时的实测，允许机器差异，方向必须一致）：

| 指标 | 优化前实测 | 完成后预期 |
|---|---:|---:|
| 病态渲染帧（未闭合开符密集，1 MiB） | 4.48 s | < 100 µs |
| 渲染帧（真实代码，索引冷） | 145-193 µs | < 50 µs |
| 打字 CPU（50 MiB，5 键/秒） | 29-37% | < 5% |
| 每帧堆分配 | 66 | 1 |
| PairIndex 内存（50 MiB 密集） | 103 MiB | 约 77 MiB |
| `matching_partner_for_render, unclosed`（1 MiB bench） | 0.59-1.2 ms | < 100 µs |
| `cargo test --all-targets`（debug） | 曾 panic | 全绿 |

若 `/tmp/wedprobe` 仍存在，可额外运行 `proto`、`hot`、`warmhit 50 200`、`tap 50 200 6` 交叉验证（命令与预期见各任务）；不存在则跳过。

最后把 50 MiB 的实测结果追加到 `docs/perf-baseline.md`（若 T06 已追加过一节，本节作为"总验收复核"续写），并提交。

---

## 变更记录

- 2026-10-10 创建。基于对 `docs/perf-optimization-plan-2026-10-09.md` 的逐条核验与修正；关键修正：P1-A 反向转义方向性错误与闭符守卫、P2-A 改为 worker 侧尾沿防抖、P2-D 栈规则改为"位置最小者"、P2-B/C 合并并加溢出守卫、P3-G 拆为 T01+T02（含 CI 门禁）。
