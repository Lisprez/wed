# wed 深度性能分析与优化方案（2026-10-09）

本文在 `docs/perf-optimization-plan.md`（2026-10-05 落地版）之后，对 `src/` 全部
9,243 行做了逐文件通读，并用一个仓库外的探针工程补齐了现有 benchmark 测不到的两个
盲区：**后台线程的 CPU 开销**与**索引冷热状态对渲染路径的实际影响**。

代码级事实均附 `file:line`；每个性能数字都给出复现方式，见 §8。

---

## 1. 架构与数据流

```
                         ┌──────────────────────────────────────┐
main.rs  event loop  ──► │ App (app.rs)                          │  模式机：Editing / CommandLine / QuitPrompt
  raw mode guard          │   handle_key ──► convert_key ──►     │
  panic hook              │   Editor::handle_key                 │
  event coalescing        └──────────────┬───────────────────────┘
                                          ▼
                         ┌──────────────────────────────────────┐
                         │ Editor (core/editor.rs, 3628 行)      │  模态机：Normal / Insert / Visual / OperatorPending
                         │   preprocess_normal_key (多键序列)     │      / Search / CommandLine
                         │   motion / textobject / operator      │
                         └──────┬────────────────┬─────────────┘
                                ▼                ▼
              ┌──────────────────────────┐  ┌──────────────────────┐
              │ Document (document.rs)    │  │ History (history.rs)  │
              │   ropey::Rope  (char 索引) │  │   Transaction<Edit>   │
              │   mutation: u64            │  │   64 MiB 预算         │
              │   pairs: RefCell<Option<   │  └──────────────────────┘
              │     (u64, PairIndex)>>     │
              │   index_cmd / _result /    │
              │   _worker (后台线程)        │
              └──────────┬────────────────┘
                         ▼
              ┌──────────────────────────┐
              │ PairIndex (pairing.rs)   │◄── textobject.rs / pairing.rs 查询
              │  4 kinds × (opens,close_of)│
              └──────────────────────────┘
                         ▲
              ┌──────────┴────────────────┐
              │ Renderer (render.rs)       │  FrameKey 快短路 + 脏行增量重绘
              │  paintLine / RowPainter    │
              └───────────────────────────┘
```

**核心不变量**（这块设计是仓库最强的部分）：

- `Document::mutation` 是唯一真相源。所有派生结构（PairIndex、三条 render memo）都带
  mutation tag 缓存，靠 tag 比对失效（`document.rs:11-23`）——派生数据不可能与文档不一致。
- `Editor::revision` 只服务于渲染缓存（`render.rs:122`），与 document mutation 解耦。
- 编辑分两条路径：`apply_edit`（读回 rope）与 `apply_known_edit`（调用方已持有文本，
  `editor.rs:1702`），后者是为省一次 rope 读出而精心设计的。
- 消息/状态栏/搜索框均已按变更缓存，不是每帧 `format!`。

---

## 2. 控制流

```
crossterm event::read()  ──► Drain: poll(ZERO)  ──► 合并为一帧
   │
   ▼
App::handle_key  ── take_clipboard_errors / QuitPrompt / CommandLine / Ctrl 快捷键 / ':'
   │
   ▼
convert_key(KeyEvent) ── Key
   │
   ▼
Editor::handle_key ── match self.mode
   ├─ Normal           ── preprocess_normal_key（find/register/replace/g/count 五个 pending）
   │                       └─ handle_normal_key（大 match 派发）
   ├─ Insert           ── handle_insert_key
   ├─ Visual(_)        ── handle_visual_key（每个移动键都跑 motion::resolve，editor.rs:1033）
   ├─ OperatorPending  ── handle_pending_key（digit / g / find / motion / textobject）
   ├─ Search           ── handle_search_key
   └─ CommandLine      ── Continue（由 App 层处理）
```

控制流本身没有需要重构的地方。问题全部集中在**数据流**上：三个派生结构里最贵的那个
（PairIndex）的失效策略，与它的实际消费频率严重失配。

---

## 3. 实测数据

### 3.1 仓库 benchmark（`cargo bench`，1 MiB，本机）

| 场景 | 实测 p50 |
|---|---:|
| 插入 `i<char><Esc>` | 0.001 ms |
| 删除 `x` | 0.001 ms |
| 移动 `j+k` / `w+b` | 0.001–0.002 ms |
| `goto_line` | 0.000 ms |
| textobject `iw`/`aw`/`a"` | 0.000–0.001 ms |
| 选区 `ranges()`（100 行块） | 0.029 ms |
| 渲染一帧 22 行 | 0.010–0.031 ms |
| 搜索 `/`（深处唯一匹配） | 0.037 ms |
| `s///g` 字面量 / 正则 | 0.49 / 0.067 ms |
| `PairIndex::build`（1 MiB） | **1.4–2.9 ms** |

### 3.2 探针工程测到的、benchmark 测不到的部分

仓库 benchmark 只量**前台时间**。`Document::changed()`（`document.rs:66-76`）在每次
编辑后向后台线程投递一次 rope 快照，触发一整轮 `PairIndex::build`——这部分 CPU 与延迟
对 benchmark 完全不可见。用一个独立探针工程（`/tmp/wedprobe`，见 §8）测得：

| 测量项 | 1 MiB | 50 MiB |
|---|---:|---:|
| 单次按键前台耗时（`i x Esc`） | **0.61 µs** | **0.34 µs** |
| 每次按键的后台 PairIndex 重建 | **3.5 ms** | **90 ms** |
| 渲染路径括号高亮（索引冷，真实 Rust 代码） | 145 µs | 157 µs |
| 渲染路径括号高亮（索引冷，未闭合开符密集） | **4.43 s** | — |
| 渲染路径括号高亮（索引热） | 111 ns | 111 ns |
| 打字时（5 组/秒）进程 CPU 占用 | 1.2 % | **29 %** |
| 打字期间拿到"索引已热"的帧数 | — | **0 / 24** |
| `display_width()` 每字符 | 7.8 ns | 同 |
| 一帧 heap 分配次数 | 66 次 | 同 |

`/tmp/wedprobe/src/bin/warmhit.rs` 按渲染器的真实调用方式，在每次按键后立即查询配对：

```
50 MiB @ 200ms/组: 24 帧渲染
  index warm: 0 frames, 24 cold (15 of which >30us)
  avg cold-frame query: 575 us
```

**即：后台线程烧掉 29% 核心重建出来的索引，渲染路径一帧都没用上。**
原因在 `render.rs:621`——`pair_index_warm()` 在编辑刚发生的那个窗口必然为 false，而
打字越快这个窗口越大，索引越不可能热。

这个索引真正服务的是 `%` 和 pair text object（`i(`/`a(`）这类**稀疏**按键；而它付出的
是**每次按键**一次 O(N)。1 MiB 上是 3.5 ms / 0.61 µs ≈ 5700×，50 MiB 上是
90 ms / 0.34 µs ≈ 270,000×。

### 3.3 50 MiB 索引内存

`/tmp/wedprobe/src/bin/idxmem.rs`：bracket-dense 50 MiB 文档记录 6,765,008 个开符，
索引 = 2 × `Vec<usize>` = **103 MiB**，是文件大小的 2 倍。方案 P1-1 第⑷步规划的
`u32` 改造可将其减半至 51 MiB。

---

## 4. 逐文件关键发现（18 条）

### `src/core/textobject.rs`

**① `next_opening_pair_scan` × `matching_close` 是 O(limit²)，就在每帧渲染路径上**
（`textobject.rs:584-632`）

外层 `chars_from(...).take(limit)` 每遇到一个未转义开符，就调用 `matching_close`，
后者**从该开符自身位置重新扫最多 `limit` 个字符**。窗口内有 k 个未闭合开符 ⇒
O(k·limit)，上限 65536² ≈ 4.3×10⁹ 次字符迭代；而 `matching_pair_within` 对 4 个
bracket kind 各跑一遍（`textobject.rs:108-114`）⇒ 上限约 1.7×10¹⁰。

`textobject.rs:581-583` 的注释声称"phase one 已是到 EOF 的一趟，phase two 永不运行"
——只在窗口内该 kind 的开符 ≤ 1 个时成立。这是我实测出 4.43 s 的直接原因。

**② `is_escaped` 是 O(log N) rope 下降，按分隔符逐个调用**（`textobject.rs:653-666`）

`document.char_at(i)` 在 ropey 里是 `rope.char(i)`，一次 O(log N) 树下降 + UTF-8 解码。
调用点有 5 处（`164`、`272`、`539`、`541`、`594`），其中三处在扫描循环内。
同一文件的前向循环（`554-559`）和 `matching_close`（`616-621`）**已经在用 running
parity counter**，O(1) 每字符——只有反向那条没享受到。

**③ `PARTNER_SCAN_LIMIT` 管不到 quote 分支**（`textobject.rs:138-140`）

对 `"`/`'`/`` ` `` 调用 `matching_quote_partner` 时**没有把 `limit` 传下去**，而后者
扫整行（`155-167`）。压缩 JS / 单行 JSON 上这是每帧 O(N)。

**④ `tag_object` 无条件从位置 0 全文档扫描**（`textobject.rs:348-405`）

`chars_from(CharPos::ZERO)`，光标只用于 `392` 行的包含测试。50 MiB HTML 上的 `it`
固定一趟 O(N)，且每个标签一次 `String::new()` + 一次 `to_ascii_lowercase()` 分配。
方案 P2-3 早已指出但未落地。

**⑤ 正确性悬崖**（`textobject.rs:370-372`）：遇到第一个没有 `>` 的 `<` 就整体 `break`。
大文件里一个游离 `<` 会让搜索静默返回 `None`。

### `src/core/document.rs`

**⑥ `changed()` 每次编辑投递一次全文档 rope 快照**（`document.rs:66-76`）

数据见 §3.2。这是整个项目最大的隐藏开销。

**⑦ 这笔开销的受益方几乎为零**：0 / 24 帧拿到热索引（§3.2）。

**⑧ 索引内存是文件大小的 2 倍**（`pairing.rs:53-56`，见 §3.3）。

**⑨ `Document` 析构不 join worker 线程**，后台线程的异步 free 会打乱
`benches/alloc.rs` 的计数分配器。实测 `cargo bench --bench alloc` 在 Fast 规模就
panic（`benches/alloc.rs:179` subtract with overflow），release 下输出
`18014398509480953 KiB（17592102158734.99×）` 这类回绕负值。**benchmark 基础设施的
真实 bug。**

### `src/core/pairing.rs`

**⑩ `enclosing_at` / `first_balanced_pair` 二分之后还在线性走**
（`pairing.rs:166-174, 196-204`）

`enclosing_at` 向前回溯所有未匹配开符，`first_balanced_pair` 向后走过所有未匹配开符。
一个前 50 万个 `(` 都不闭合的文档上，`i(` 每次按 50 万次迭代。索引号称 O(log N)，
实际是 O(#unmatched)。

### `src/core/motion.rs`

**⑪ `Motion::FirstNonBlank` 逐字符调 `char_at`**（`motion.rs:54-72`）

`(start.0..end.0).find(|i| document.char_at(...))` ⇒ O(L·log N) 次 rope 下降，
直接违反 `document.rs:344-349` 自己写下的指引。三行改成 `chars_from(start).find(...)`。

### `src/render.rs`

**⑫ `RowPainter::flush` 的 `std::mem::take(self.run)` 丢掉缓冲区容量**（`render.rs:596`）

`Scratch` 的注释（`render.rs:22-23`）说"Buffer reused across frames so painting a row
does not allocate"，但 `mem::take` 把 `run` 换成容量 0 的空 `String`，下一次
`push_str` 重新分配。实测复现：22 行 × 3 个样式 run ⇒ **66 次 heap 分配**
（= 1 + 2 + 21×3）。改成 drain 而非 take ⇒ **1 次**。

**⑬ `width()` 没有 ASCII 快路**（`src/width.rs:144-156`）

两张表的最小项分别是 0x0300 和 0x1100（`src/width.rs:28, 63`，已核对），因此
`if code < 0x0300 { return 1 }` 是**精确**的。实测 **7.8 ns/char → 1.62 ns/char（4.4×）**；
一帧 1760 字符 ⇒ 省约 13.6 µs，而整帧实测才 10–31 µs——`width()` 很可能是帧绘制成本的
50 % 以上。

**⑭ 增量重绘与 `FrameKey` 是正确的**，且 `cached_matching_partner` 的键已纳入 mode
（`render.rs:40, 217-231`），方案 §4 提到的潜在 bug 已由构造方式修掉。

### `src/core/editor.rs`

**⑮ `substitute` 整份文档 String 存活到函数末尾**（`editor.rs:471`）

`text` 最后一次使用在 `482` 行，但值一直活到 `508` 行。`editor.rs:467-468` 的注释声称
"The text is released as soon as the match positions are known"——**与代码不符**。
50 MiB 文档的 `:s///g` 期间白扛一整份拷贝。加一句 `drop(text)` 即消。

**⑯ `replace_literal` 在生产环境不可达**：`:s` 的派发在 `app.rs:277-288` 永远走
`Substitution::parse` 正则路径。这个分块、零拷贝的搜索实现只有测试用。

**⑰ `find_first_match` 每次调用重写整个 64 KiB chunk**（`editor.rs:2104-2119`），
并对 `skip` 重走最多 64 Ki 字符（`2113`）。密集匹配下是 O(n×SEARCH_CHUNK) 内存流量。

**⑱ `apply_operator_range` 把同一段文本 clone 三次**：`text.clone()` 进 register
（`1486`/`1503`）、`text` 进 undo（`1492`）、`value.text.clone()` 进 clipboard（`1532`）。

### `src/core/history.rs` / `register.rs` / `clipboard.rs`

- `Transaction::bytes()`（`history.rs:52-57`）在每次 `push_undo` 和每次 budget 执行时
  重算（`195, 198, 214`）。10 万 edits 的替换事务 ⇒ 每次 10 万次迭代。
- `read_register` 对寄存器做 `.cloned()`（`editor.rs:1236`），每次粘贴一次深拷贝。
- clipboard 已是单 worker + mpsc，P2-2 落地良好；但**每次读都是一次进程 fork/exec**
  （`clipboard.rs:133`）。
- `src/width.rs` 另有一处与性能无关的正确性缺口：U+1F1E6–1F1FF、U+2600–27BF、
  U+1F900–1F90B 在现代终端是双格但被计为单格。

### 文档与实现的名实不符

- `README.md:7,16-17,33` 仍宣传 "Gap Buffer architecture" 与 O(1) 描述，实际是 ropey。
  `docs/tiny-neovim-development-plan.md:543` 明确禁止未经验证的绝对 O(1) 描述。
- `docs/perf-baseline.md` 的 50 MiB 索引结论只度量了前台延迟，**未度量后台 CPU**——
  这正是 29 % 核心占用逃过所有基准测试的原因。

---

## 5. 优化方案

按"收益 ÷ 风险"排序。分四个阶段，每阶段独立可验证、可回滚。

### 阶段一：渲染路径（最高收益，零语义风险）

**P1-A 用一次外向遍历取代五个嵌套扫描** — `textobject.rs:584-632`、`pairing.rs:242-274`

把 `matching_pair_within` 的 `enclosing_pair` + 4×`next_opening_pair` 换成：从光标出发
一次反向走到深度 0 的开符，再一次正向走到它的闭符。两次遍历共用**同一份 running
backslash parity**，顺带消灭 `is_escaped` 的 O(log N) rope 下降。

实测（`/tmp/wedprobe/src/bin/proto.rs`）：

| 语料 | 现有 | 外向遍历 | 倍数 |
|---|---:|---:|---:|
| 未闭合开符密集（1 MiB） | **4.426 s** | 130.8 µs | **33851×** |
| 仅闭符（1 MiB） | 191 ns | 129 ns | 1× |
| 惯用 Rust（1 MiB） | 145.2 µs | 161.9 µs | 1× |
| 惯用 Rust（50 MiB） | 157.5 µs | 181.9 µs | 1× |

**P1-B 渲染路径彻底摆脱索引依赖，并收紧 bound** — `render.rs:615-633`

`matching_partner_for_render` 改为：索引热 → O(log N) 精确值（保留现状）；索引冷 →
P1-A 的外向遍历，**bound 从 64 KiB 降到 2–4 KiB**。收缩理由：高亮是装饰，
`PARTNER_SCAN_LIMIT` 的存在本就承认它可以放弃；而全部成本都与 bound 成正比。
这样每帧恒定为几 µs，与文档大小、索引温度、语料密度全部无关。

**P1-C `width()` ASCII 快路** — `src/width.rs:144`

加 `if code < 0x0300 { return 1 }`。精确（两表最小项 0x0300 / 0x1100），实测 **4.4×**。

**P1-D `RowPainter::flush` 改为 drain 而非 `mem::take`** — `render.rs:596`

先 `queue!(..., Print(&self.run[..]))` 再 `self.run.clear()`。每帧 heap 分配 **66 → 1**。

> 一处诚实的说明：P1-C 的探针用等基数（26 / 75）的代表表复现二分查找的探针次数，
> 真实表见 `src/width.rs:27-138`。结论依据的是两表最小项均 ≥ 0x0300 这一可核对的
> 结构事实，而非探针表本身。

### 阶段二：配对索引（消除 29 % CPU 浪费）

**P2-A 索引改惰性 + 防抖，不再每次编辑都重建** — `document.rs:66-76, 103-141`

`changed()` 只作废缓存（`pairs = None`），不再投递任务。改为**按需**触发：

1. `pair_index()` / `pair_index_warm()` 发现冷且有待建标记时才派发后台作业——
   "用户真的问了才建"。
2. 另加静默期门闩：距上次编辑超过 ~250 ms 才允许后台建，打字期间最多 1 次/暂停。
   用已有的 `pair_index_warm()` 作为轮询点，无需新线程。

实测目标：打字 CPU **29 % → < 2 %**；暂停后 `%` 仍拿到热索引（O(log N)，111 ns）。

**P2-B `PairIndex` 位置改 `u32`** — `pairing.rs:53-56`

方案 P1-1 第⑷步，实测确认未落地。50 MiB bracket-dense 文档索引
**103 MiB → 51 MiB**，构建时间同步减半。

**P2-C `enclosing_at` / `first_balanced_pair` 去掉二分后的线性回溯** — `pairing.rs:160-205`

构建时顺带记录"每个开符在栈中的层号"或"前一个已匹配开符"，使两查询真正 O(log N)。
修掉"前 50 万个 `(` 不闭合时 `i(` 迭代 50 万次"。

**P2-D `next_opening_pair_scan` 改单趟栈式前向遍历** — `textobject.rs:584-632`

保留两阶段语义（`((` 要报内层对），但用**一个显式开符栈**一次前向遍历完成：
开符入栈下标，闭符出栈即产生一个平衡对，第一个出栈的就是首对。O(N) 替代 O(N²)。

### 阶段三：编辑与搜索路径

| 编号 | 改动 | 位置 |
|---|---|---|
| P3-A | `FirstNonBlank` 改 `chars_from` | `motion.rs:54-72` |
| P3-B | `matching_quote_partner` 接受 `limit` | `textobject.rs:138-140, 155-177` |
| P3-C | `substitute` 在 `482` 行后 `drop(text)`，并修正 `467-468` 注释 | `editor.rs:471` |
| P3-D | `Transaction` 缓存字节总数 | `history.rs:52-57` |
| P3-E | 寄存器改 `Rc<str>` | `register.rs:23`、`editor.rs:1236, 1532` |
| P3-F | `find_first_match` 从 `from` 精确偏移开始，不整块重写 | `editor.rs:2104-2119` |
| P3-G | 修 `benches/alloc.rs`：`Document` 析构时 join worker，或计数分配器的 realloc 改线程安全（现在 `fetch_add` + `store` 本身也是竞态） | `document.rs`、`benches/alloc.rs:39-50` |

### 阶段四：结构性补充（可择机）

- **P4-A** `tag_object` 从光标双向扫描（方案 P2-3，`textobject.rs:348-405`）：
  O(N) → O(标签间距)；顺带去掉每标签 2 次分配（`&str` + `eq_ignore_ascii_case`）。
- **P4-B** 把 `:s` 的无元字符 needle 分流到 `replace_literal`（`editor.rs:510`，
  现在生产不可达），或删除它。
- **P4-C** 修 `README.md` 的 Gap Buffer / O(1) 表述——
  `docs/tiny-neovim-development-plan.md:543` 要求性能声明必须有可复现 benchmark 支撑。
- **P4-D** CI 补 benchmark 趋势对比（方案 §9 四项门禁里唯一缺的）与 `cargo audit`。

---

## 6. 预期汇总

| 指标 | 现状 | 目标 |
|---|---:|---:|
| 渲染帧（良性语料） | 10–31 µs | ~4–8 µs |
| 渲染帧（未闭合开符密集，1 MiB） | **4.43 s** | ~130 µs |
| 渲染帧（真实 Rust 代码） | 145–492 µs | < 10 µs |
| 每帧 heap 分配 | 66 | 1 |
| 打字 CPU（50 MiB，5 键/秒） | **29 % 核心** | < 2 % |
| `%`（伙伴在数 KB 内） | 1.95 µs | 0.48 µs |
| PairIndex 内存（50 MiB） | 103 MiB | 51 MiB |

---

## 7. 风险与回滚

- 阶段一每个文件可独立 revert。
- P1-B 会改变"索引冷时的长距离高亮"外观——这正是 `PARTNER_SCAN_LIMIT` 已声明的语义，
  但需在 `docs/` 记一笔。
- 阶段二的语义等价性由 `pairing.rs:326 the_index_agrees_with_the_scan_on_random_input`
  守住；P2-D 的新栈式路径应扩充覆盖到该随机测试里。
- P2-A 引入时间门闩后，`Document` 会依赖时钟；建议把门闩阈值提为常量并在文档注明。

---

## 8. 复现方式

§3.2 的数字来自一个仓库外探针工程（`/tmp/wedprobe`），它直接依赖本仓库的 lib：

```bash
mkdir -p /tmp/wedprobe/src/bin && cd /tmp/wedprobe
cat > Cargo.toml <<'EOF'
[package]
name = "wedprobe"
version = "0.0.0"
edition = "2021"

[dependencies]
wed = { path = "/Users/lisprez/Projects/wed" }

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
EOF
```

各探针程序：

| 程序 | 测量内容 |
|---|---|
| `proto.rs` | §5 P1-A：外向遍历 vs 五个嵌套扫描（含 4.43 s 那一行） |
| `warmhit.rs` | §3.2：打字期间索引冷热帧数与冷帧耗时 |
| `tap.rs` | §3.2：进程 CPU 占用（配 `ps -p $PID -o time,%cpu`） |
| `hot.rs` | §4⑬：`width()` ASCII 快路倍数 |
| `idxmem.rs` | §3.3：PairIndex 内存 |
| `semantic.rs` | §6：`%` 的索引路径 vs 外向遍历 |
| `alloc.rs` | §4⑨：每帧 heap 分配计数 |

跑法示例：

```bash
cargo build --release --offline
./target/release/proto                                   # 阶段一 A 的收益
./target/release/warmhit 50 200                          # 索引冷热 + 冷帧耗时
./target/release/tap 50 200 6 & sleep 5; ps -p $! -o time,%cpu   # 后台 CPU
./target/release/hot                                     # width() 快路
./target/release/idxmem                                  # 索引内存
```

注意：`tap.rs` / `warmhit.rs` 的结果依赖机器核心数（本机 18 核）与打字节奏参数
（`ms_per_key`）。探针工程未纳入仓库，如需固化为可复现 benchmark，可移入
`benches/` 并接入 `harness/mod.rs` 的 `measure` 设施。
