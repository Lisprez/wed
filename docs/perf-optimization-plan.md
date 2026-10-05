# wed 性能优化方案

本文在逐文件通读（`src/` 全部文件、`benches/`、`docs/`）的基础上，结合 `cargo bench`
实测基线给出优化切入点。代码级事实均附 `file:line` 引用。

---

## 1. 现状：架构与数据流

```
main.rs (event loop)
  │ key/resize event
  ▼
App::handle_key ─────────────── clipboard::queue_contents (后台线程)
  │                                clipboard::get_contents (同步子进程)
  ▼
Editor (core/editor.rs, 状机: Normal/Insert/Visual/OperatorPending/Search/CommandLine)
  │  mutates via apply_edit / apply_known_edit
  ▼
Document (core/document.rs, ropey::Rope)
  │  mutation 计数作废缓存
  ▼
PairIndex (core/pairing.rs, 惰性重建)  ←──────── core/textobject.rs 查询
  ▼
Renderer (render.rs) ── 每帧全量重绘可见行 ──► crossterm queue!
```

- 文档落地：`app.rs::read_document` 分块读入；保存走临时文件 + rename（原子）。
- 撤销：`history.rs`，`Edit { start, removed, inserted }` 两个 String，事务分组，
  预算 64 MiB。
- 渲染缓存：`revision` 驱动的三条 memo（光标显示列/选区 ranges/括号匹配）。
- 括号配对：`Document::pair_index()` 惰性按 mutation 重建；渲染路径用
  `PARTNER_SCAN_LIMIT = 1<<16` 有界扫描（`textobject.rs:96`）。

## 2. 实测基线（`cargo bench`，1 MiB，2026-10-05 重新采集）

| 场景 | 实测 p50 | 评价 |
|---|---:|---|
| 插入/删除/移动/跳转 | < 0.001 – 0.002 ms | 达标 |
| 渲染一帧 22 行 | 0.010 – 0.021 ms | 快，但**光标移动 1 行仍发射 ~1.1–1.8 KB** |
| `matching_partner_for_render`（遇未闭合分隔符） | **0.59 – 1.23 ms / 次** | 渲染路径上的最大单点热点 |
| `PairIndex::build`（1 MiB） | 2.35 – 2.93 ms | 每次编辑后首个无界查询重建一次 |
| `%`（热索引） | 0.001 ms | 索引热时很好 |
| 搜索 `/`（深处唯一匹配） | 0.33 – 1.17 ms | 接近 10 ms 目标的 1/10，长行语料最差 |
| `s///g` 字面量 | 0.24 – 0.32 ms | 全文档物化文本 |
| `s///g` 正则 | 0.08 – 0.12 ms | 字面值反而慢（分块扫描 + 计数开销） |

> 基线备注：render bench 自己点出结论——"a change-aware renderer would emit 0"
> （`benches/render.rs:90-95`）。

## 3. 优化项（按收益排序）

### P0-1 渲染：变更感知的帧绘制（最大收益，正确性等价）

**现状**：每收到一个按键，`Renderer::render` 清屏式重写整个视口
（`render.rs:66-181`）。光标移动一行，内容没变，仍把 22 行全部重发（1112–1760 B），
且每行 `MoveTo + Clear(CurrentLine)` 两条指令。

**方案**：
1. **帧键短路**：缓存 `(document.revision, cursor, mode, AppMode, row_offset, col_offset,
   selection, viewport size)`；键不变直接 `return Ok(())`。纯移动/插入文本之外
   绝大多数按键（如 `Esc`、`:` 打开、无效键）不再重绘。
2. **行级脏集**：`Editor` 在每次编辑后记录
   `last_edit: Option<CharRange>`；`Renderer` 仅重绘 `编辑影响行 ∪ 旧/新光标行 ∪
   旧/新高亮行`。行内容本身可加 per-line 版本（ropey 无内建，需维护
   `Vec<u64>` 或与编辑区间做交集判断）。
3. **滚动分支**：`row_offset`/`col_offset` 变化 → 全量；否则增量。
4. **事件合并**：`main.rs` 主循环中每处理一个事件就 render 一次；改为
   `event::poll(Duration::ZERO)` 排干积压事件后再渲染一帧。一次快速键入多字只出一帧。

**预期**：光标移动帧字节数 → ~0；长行语料帧时间不变（本身就不高但 CPU 占用下降）。
**风险**：终端 resize / QuitPrompt 等全量路径要显式正确，配 `benches/render.rs`
重绘音量断言固化。

### P0-2 渲染路径上的括号配对：用索引，丢有界扫描

**现状**：`matching_partner_for_render`（`render.rs:429`）在普通模式下对光标 ±64K
字符做扫描；未闭合分隔符时 p50 ≈ 0.59–1.23 ms **每帧**。`PairIndex` 已存在，却因
"索引重建太贵"而没有用于渲染。

**方案（惰性 + 增量两档）**：
1. 保守版：渲染配对查询统一改走 `Document::pair_index()`（全文档精确、O(1) 查询），
   付出 = 每次编辑后下一次任何配对查询重建一次索引（2.6 ms @1MiB）。把成本从
   "每帧 0.6–1.2 ms" 变为 "每次编辑一次 2.6 ms"。对 50 MiB 需先由 P1-1 加速。
2. 进取版（见 P1-1 异步重建）。
3. 同步修复 `enclosing_at` 在位置落在未闭合开括号时的语义，保证与扫描等价——
   `pairing.rs` 测试已覆盖等价性，可直接复用。

### P1-1 PairIndex：O(1) 快照 + 后台重建 + 字节级加速

**现状**：`Document::changed` 每次编辑 `self.pairs = RefCell::new(None)`
（`document.rs:66`）——不仅丢缓存还分配新 RefCell；下次无界查询整文档重建
（`PairIndex::build`，`pairing.rs:53`），重建期间主线程停顿。

**方案**：
1. `changed()` 改为 `*self.pairs.borrow_mut() = None`，零分配。
2. `build` 改字节扫描：`rope.chunks()` 上对 ASCII 分隔符做 `memchr` 风格
   批量查找，反斜杠奇偶性按 chunk 预计算。预期 3–10×，使 1 MiB 重建进 ~0.3 ms。
3. 异步重建：`Rope` clone 是 O(1)（chunk 共享，Arc-like）。编辑后在后台线程用
   快照构建，完成则原子 swap；构建期间查询退化为有界扫描（正确，语义不变，
   有界保证不失控）。把停顿从编辑路径移出。
4. 内存：`usize` → 文本 < 4 Gi 时用 `u32` 存位置，delimiter 密集文件
   索引内存减半。

### P1-2 编辑路径微优化（`dit`、`x`、`i<char>` 已 <1µs，但组分配仍有空间）

1. `History::undo/redo` 每次 `edit.inserted.chars().count()` 重新计数
   （`history.rs:152-157`）。在 `Edit` 记录时存 `inserted_len`/`removed_len`（chars），
   O(1) 查询。`apply_known_edit` 已经两侧 by-value，可顺带携带长度。
2. Insert 会话中每个字符一个 `Edit`，`record_edit` 对 `removed:""`、`inserted:"x"`
   各一次 String 分配。合并策略：事务内相邻、光标连续的同类插入可记录为一个 Edit
   （需要光标位置在记录时可知——insert 路径天然可知），或用 `SmallVec`/长度内联
   的小字符串存 1–2 字节插入降分配次数。分配基线可用 `benches/alloc.rs` 复核。
3. `Edit.removed/inserted` 用 `Rc<str>`：yank 后 register 与撤销记录同文，
   `apply_operator_range`（`editor.rs:1439-1452`）对同一文本做了两份 `clone()`，
   改 Rc 后一次分配两处引用。

### P1-3 搜索与字面量替换：消除冗余字符计数

`find_first_match`（`editor.rs:2046`）每个命中后 `buffer[..at].chars().count()`
重数 chunk；长行语料 p50 0.62 ms。改为保有"chunk 起点到匹配点"的字节游标，
只对最终命中位置做一次 chars 计数增量。字面量替换用固定大小 needle 时可直接在
字节上 `find` 并用 `mem::copy` 组装输出，比当前逐 chunk 的 `Chars` 计算快一倍以上
（字面量 s///g 0.25–0.32 ms 而正则版 0.08–0.12 ms 的反常原因即在此）。

### P2-1 状态栏/消息：帧间缓存，别每帧 format!

- `render_status`（`render.rs:183`）每帧 `path.to_string_lossy().into_owned()`、
  `format!` 两份字符串；`App` 保存 `path` 本就有，缓存一个 `filename: String`
  在 `open_path/save_as` 时更新。
- `render_message` 每帧 `safe_text(message, width)` 产生新 String；缓存
  `(message 指针/ revision, width) -> String`。
- `search_prompt`（`editor.rs:381`）每帧 `format!`；Search 模式下按输入长度变化
  缓存。

### P2-2 clipboard：采集线程化，写入复用一个常驻 worker

`queue_contents`（`clipboard.rs:11`）每次 yank 起一个新线程（~10–50 µs 创建成本，
且快速连续 yank 顺序不保证）。改为 `OnceLock<Sender>` + 单个 worker 线程 +
mpsc channel，多次 yank 自然串行有序，`take_errors` 语义不变。

### P2-3 tag object：从头扫描 → 从光标反向解析

`tag_object`（`textobject.rs:329`）从文档头部扫到"包含光标的第一个匹配对"为止，
最坏 O(N)。改为：从光标向左找未闭合的 `<name>`（带栈深度），再向右找配对
`</name>`，工作量与成对标签间距成正比而非文档大小。需补等价性测试。

### P3-1 （可选）渲染行缓存

`paint_line`（`render.rs:285`）对每个可见行从其行首起步走到视口右缘。行长稳定时
可用 `(line_index, line_revision) -> Vec<Cell>` 缓存渲染结果；编辑集中在少数行时
9/10 的行直接复用。属于加法收益，P0-1 做完后用 50 MiB long-line 语料重新评估是否
需要——当前 0.01 ms/帧已经远低于 10 ms。

### P3-2 不做的事（有理有据）

- `Editor::substitute` 正则版全文档物化：分析见 `editor.rs:400` 注释，跨界匹配+
  行锚点的分块处理易错，峰值内存已经只有 rope+一份文本。保留，必要时只压缩
  `to_char_ranges` 的额外一遍扫描（与物化同遍完成）。
- `Editor::text()` 全量拷贝：仅测试/bench 使用路径，保留。

## 4. 数据流优化视角（派生数据的失效是全项目的同构问题）

三类派生数据当前各自为政：`PairIndex`（Document::mutation）、渲染 3 条 memo
（Editor::revision）、`History` 预算。建议统一为"revision-tagged memo cell"小工具
（写一次，所有调用点共享），消灭手写 match 的重复：当前缓存键错配在
`cached_matching_partner` 中已要求 mode ∈ Normal 之外给 None，而缓存键**没有** mode
会与 `Mode::Normal` 的不同 cursor/revision 组合交叉复用——`cached_mode` 有存是对的，
但统一抽象可防再有缺字段。

## 5. 控制流优化视角

- 主循环：`event::read()` 阻塞 → `poll(0)` 排干 → 合并渲染（见 P0-1⑷）。
- `preprocess_normal_key`（`editor.rs:517`）分派链顺序即优先级；`g_pending`、
  `register_pending` 等位与分派主干平级——可抽成显式小状态机，便于日后审计
  每个 pending 标志的完备性（`is_waiting_for_character` 对外暴露的键集合不在一个地方）。
- `match` 大分派按键：`motion_for_key` 走每次查表，编译器已优化为 jump table，
  不动。

## 6. 验证方法

1. 每项 P0/P1 落地后跑 `cargo bench`，对照 §2 表格；`WED_BENCH_SCALE=full`
   抽查 50 MiB 括号配对与未闭合分隔符场景。
2. 新增回归断言：
   - `benches/render.rs` 的 repaint 音量从 ~1112 B 降到光标移动不触发内容行重绘；
   - `benches/alloc.rs` 的 resident bytes 在 Insert 会话数据集不涨；
   - pairing 等价性随机测试（已有，`pairing.rs:225`）复用。
3. `cargo test` 全量；文档基线 `docs/perf-baseline.md` 更新一节附本次结果。

## 7. 建议执行顺序

| 步骤 | 内容 | 风险 | 回滚面 |
|---|---|---|---|
| 1 | P2-1 状态栏缓存 + P1-2⑴ 长度字段 | 低 | 渲染/history 两文件 |
| 2 | P0-1 帧键 + 事件合并 | 中 | main/render |
| 3 | P0-1 行级脏集 | 中 | render + editor 记录脏区间 |
| 4 | P0-2 渲染走索引 + P1-1⑴⑵ | 中 | document/pairing/render |
| 5 | P1-1⑶ 异步重建 | 中高 | document 线程安全性（Send 证明） |
| 6 | P1-3 搜索字节游标、字面量快路径 | 低 | editor |
| 7 | P2-2/P2-3 | 低 | clipboard/textobject |

第 3、4 步各自独立可验证；指标以 §2 为准，全部落地预期未闭合分隔符渲染热点
从 0.6–1.2 ms/帧 消除，光标移动帧输出趋零，索引重建从主线程移除。

---

## 8. 落地记录（2026-10-05）

已按执行顺序落地，每步均通过 `cargo test`（132 + 41 用例）与 `cargo bench` 验证。

| 步骤 | 变更 | 实测效果 |
|---|---|---|
| 1 | `history.rs`: `Edit` 增加 `removed_len`/`inserted_len`，undo/redo 不再 `chars().count()`；`app.rs`/`render.rs` 状态栏文件名与消息文本按变更缓存；`editor.rs::search_prompt`、`app.rs::command_prompt` 返回 `&str` 且按输入缓存 | 每帧去掉多处 String 分配 |
| 2 | `main.rs`：`event::read()` 后用 `poll(ZERO)` 排干积压事件再渲染一次；`render.rs::FrameKey` 对 revision/cursor/mode/selection/offset/尺寸/消息/两个 prompt 做相等判断，相等即跳过整帧 | 无操作按键、输入合并零重绘 |
| 3 | `editor.rs`：新增 `DirtyLines { lines, full }`，`apply_edit/apply_known_edit/record_edit` 记录脏行，跨行编辑（含 `\n`）置 `full`；undo/redo/replace_document 置 `full`；`Renderer` 仅重绘脏行 ∪ 光标行 ∪ 旧/新括号高亮行 ∪ 旧/新选区行，行偏移或尺寸变化退回整帧。新增 `painted_partner` 记录上一帧实际画出的高亮 | 光标移动在内容不变时只重一行 |
| 4 | `Document::changed` 不再分配新 `RefCell`；`PairIndex::build` 改字节扫描（ASCII chunk 走字节，混排回退逐字符）+ `Rope:byte_to_char` 校准位置；渲染配对查询在索引热时直接取 O(1) 索引、冷时退回有界扫描 | 索引构建 2.6ms → 1.1–1.4ms @1MiB |
| 5 | `Document` 内置单 `wed-pair-index` 后台线程：编辑后把 `(mutation, rope.clone())` 投递过去（只建最新一次），`try_promote_async` 在下次查询时把匹配的结果提升为缓存；冷索引仍同步构建保正确性 | 重建移出输入路径；20x warm `%` 查询 p50 0.001ms |
| 6 | 经测量取消：搜索/字面量替换的"冗余字符计数"实际每次调用只发生一次（~µs 级），`p50` 0.33–0.62ms 的来源是整段 chunk 复制，重构收益低，且改动易引入边界误差 | 保持现状 |
| 7 | `clipboard::queue_contents` 由"每次 yank 起一个线程"改为单 worker + mpsc 队列，顺序保证、无线程创建抖动；`is_escaped` 的块化版被证明更慢（短转义序列也会预载 1KB 块），已回退为直接反向走查 | 消除了我引入的回归 |

遗留的有界风险：冷索引的括号高亮有界扫描在 50 MiB 文档上仍约 0.6–1.2ms/帧（不随文档增长），字面量 `s///g` 仍是正则版 ~3×（语义要求逐条 undo 粒度，未改）。

## 9. 增量渲染的定量收益（benches/render.rs 新增增量帧基准）

```
frame: incremental after one keystroke, code corpus  p50=0.020ms p95=0.023ms
  code corpus:  full frame 1463 B, unchanged 0 B, cursor move 290 B, one edit 234 B
  wide corpus:  full frame 1629 B, unchanged 0 B, cursor move 301 B, one edit 237 B
  tabs corpus:  full frame 1965 B, unchanged 0 B, cursor move 327 B, one edit 250 B
  prose corpus: full frame 2109 B, unchanged 0 B, cursor move 342 B, one edit 252 B
```

原实现：每次按键整屏 22 行重绘（即全量的 1463–2109 B）。现在：无变更帧
0 B、光标移动约 1/6 到 1/7 的字节（旧/新两行 + 状态栏 + 消息行）、编辑
单行同量级；视口偏移变化、跨行编辑、undo/redo 退回全帧。
