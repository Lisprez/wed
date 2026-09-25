# tiny-neovim 开发计划

## 0. 文档信息

- 项目：基于当前 Rust 原型演进为 tiny-neovim
- 形态：单进程、终端原生、模态编辑器
- 技术栈：Rust 2021
- 首发平台：macOS
- 兼容目标：Linux、Windows 可编译，核心行为保持一致
- 文件模型：单缓冲区、单活动文件
- 当前状态：产品范围和架构方向已确认，尚未开始新核心实现
- 计划文件：`docs/tiny-neovim-development-plan.md`

## 1. 产品定义

tiny-neovim 不是完整 Neovim 的缩小版，也不是 Neovim 前端。它是一个面向日常高频文本编辑的、语法无关的 Vim 风格模态终端编辑器。

核心目标：

1. 键盘响应极快。
2. 命令行为正交、可组合。
3. 支持完整标准内置 TextObject。
4. 核心代码短小、易读、易测试。
5. 不引入插件、LSP、Tree-sitter 等非核心复杂度。
6. 所有高频操作都具备稳定、可预测、可验证的语义。

产品原则：

- 核心编辑逻辑独立于终端库。
- Motion、TextObject、Operator 统一通过 Range 组合。
- Normal、Insert、Visual、Operator-pending 使用同一套命令状态机。
- 不在现有 `src/engine.rs:18-408` 的单体分发函数中持续堆叠命令。
- 正确性、响应速度和可测试性优先于功能数量。

## 2. 当前原型基线

当前项目是一个小型 Rust TUI 编辑器，主要模块如下：

- `src/main.rs:18-73`：终端初始化、事件循环和退出清理
- `src/engine.rs:18-408`：编辑器状态、快捷键、文件读写、搜索、剪贴板
- `src/buffer.rs:1-238`：基于 `Vec<char>` 的 Gap Buffer
- `src/history.rs:7-44`：单字符粒度撤销/重做
- `src/viewer.rs:10-180`：终端文本和状态栏渲染
- `src/clipboard.rs:1-119`：Wayland、X11、Windows 剪贴板适配

当前原型与目标之间的主要差距：

- 当前 `Normal` 模式不是 Vim 模态，普通字符会直接插入。
- 当前只有一个 `selection_start`，无法表达 Visual Block。
- 当前历史按字符记录，粘贴和选区删除不能形成单一事务。
- 当前没有 count、operator-pending、Motion、TextObject 抽象。
- 当前文件保存不是原子操作，且多处忽略 `Result`。
- 当前渲染每次按键都会重复扫描文档。
- 当前剪贴板在 macOS 上没有实现。
- 当前没有测试、CI、锁定的工具链或可复现 benchmark。

## 3. v0.1 范围

### 3.1 编辑模式

| 模式 | 功能 |
|---|---|
| Normal | 普通编辑、移动、Operator、历史 |
| Insert | 插入、删除、换行、Tab、光标移动 |
| Visual Character | `v` |
| Visual Line | `V` |
| Visual Block | `Ctrl-V` |
| Operator-pending | `d`、`c`、`y` 等待 Motion 或 TextObject |
| Command-line | `:w`、`:q`、`:s` 等 |
| Search | `/`、`?`、`n`、`N` |

### 3.2 Motion

首版必须支持：

```text
h j k l
0 ^ $
w W b B e E
f F t T ; ,
( ) { } %
gg G
Ctrl-F Ctrl-B Ctrl-D Ctrl-U
```

后续可加入：

```text
ge gE
g_ g0 g^
```

### 3.3 Operator

首版必须支持：

```text
d   删除
c   修改
y   复制
>   增加缩进
<   减少缩进
```

必须支持：

- count
- operator count
- motion count
- operator count 与 motion count 的乘法
- `dd` 等双操作符
- operator + motion
- operator + TextObject
- Visual 模式下的 operator
- Visual Block 下的删除、修改、替换

### 3.4 编辑命令

首版必须支持：

```text
i I a A o O
x X r ~
D C cc S s
J
p P
u Ctrl-R
.
v V Ctrl-V o O
```

### 3.5 文件和命令

首版支持：

```text
:w
:q
:q!
:wq
:x
:e path
/pattern
?pattern
n N
* #
```

替换只提供高频子集：

```text
:s/old/new/
:s/old/new/g
```

支持有限语法：

```text
^ $ . * [] \\ | + ?
```

暂不实现：

- magic 和 very-magic
- `\\zs`、`\\ze`
- 完整 lookaround
- Vimscript/Lua 替换表达式
- 完整 Ex 命令语言

### 3.6 寄存器

首版支持：

```text
匿名寄存器
_ 黑洞寄存器
+ / * 系统剪贴板
```

后续加入：

```text
a-z 命名寄存器
0 最近一次复制
```

### 3.7 明确不做的功能

以下内容不进入 v0.1：

- 多文件、多窗口、多标签
- 插件系统
- Lua/Vimscript
- LSP
- Tree-sitter
- 语法高亮
- DAP
- 文件树
- Fuzzy Finder
- Git 集成
- 折叠
- 宏录制
- 多光标
- 外部命令过滤器
- 完整正则替换
- 自动格式化
- 终端复用器集成

## 4. TextObject 规范

TextObject 是本项目的核心能力。所有 TextObject 必须实现为统一的范围解析器：

```text
TextObject -> Range
Motion     -> Range
Operator   -> 消费 Range
```

### 4.1 支持对象

| 类别 | 命令 | 说明 |
|---|---|---|
| word | `aw` / `iw` | 小写单词 |
| WORD | `aW` / `iW` | 空白分隔的 WORD |
| sentence | `as` / `is` | 句子 |
| paragraph | `ap` / `ip` | 段落 |
| 双引号 | `a"` / `i"` | 双引号字符串 |
| 单引号 | `a'` / `i'` | 单引号字符串 |
| 反引号 | ``a` `` / ``i` `` | 反引号字符串 |
| 圆括号 | `ab` / `ib` | `a(`、`i(`、`a)`、`i)` 别名 |
| 方括号 | `a[` / `i[` | `a]`、`i]` 别名 |
| 花括号 | `aB` / `iB` | `a{`、`i{`、`a}`、`i}` 别名 |
| 尖括号 | `a<` / `i<` | `a>`、`i>` 别名 |
| 标签 | `at` / `it` | HTML/XML 风格标签块 |

`dgn` 作为搜索派生对象单独实现，不归入普通 TextObject。

### 4.2 统一语义

1. `a` 版本包含定界符和相关空白。
2. `i` 版本排除定界符。
3. 光标在对象内部、起始符或结束符上都能工作。
4. 支持嵌套定界符。
5. 支持转义定界符。
6. 支持跨行对象。
7. 支持 count，并遵循 Vim 的乘法规则。
8. Visual 模式中再次输入 TextObject 时扩展当前选择。
9. `daw`、`ci(`、`ya{` 等必须形成单次历史事务。
10. `at/it` 使用词法栈匹配，不引入 HTML/XML 解析器。
11. 空对象和未闭合对象的行为必须固定并加入测试。

### 4.3 必测场景

每种对象至少覆盖：

- 光标在对象内部
- 光标在起始符
- 光标在结束符
- 对象外调用
- 嵌套对象
- 空对象
- 未闭合对象
- 转义定界符
- 跨行对象
- `count = 1`
- `count > 1`
- `aw` 与 `iw` 的空白差异
- Visual 扩展
- `d`、`c`、`y` 组合
- Unicode 字符

## 5. 目标架构

### 5.1 模块结构

```text
src/
├── main.rs
├── app.rs
├── document.rs
├── editor.rs
├── input.rs
├── motion.rs
├── textobject.rs
├── operator.rs
├── selection.rs
├── history.rs
├── file.rs
├── render.rs
└── clipboard.rs
```

### 5.2 Document

`Document` 负责：

- 文本存储
- 字符、字节、行列转换
- 行索引
- UTF-8 操作
- 换行风格
- 文件路径
- 文档版本号

建议在 M0 阶段对 `ropey` 和当前 Gap Buffer 做小型 benchmark。默认优先评估 `ropey`，因为它能直接提供字符、字节和行索引，减少自定义文本索引代码。如果体积或启动性能不满足要求，再保留 Gap Buffer，但必须增加行索引，不能继续使用当前无索引的 `Vec<char>` 模型。

### 5.3 Editor

```rust
struct Editor {
    document: Document,
    cursor: Cursor,
    mode: Mode,
    selection: Option<Selection>,
    pending: PendingCommand,
    registers: Registers,
    history: History,
    message: Message,
    repeat: Option<Repeat>,
}
```

所有状态变更必须通过明确方法完成，避免多个模块直接修改 Editor 字段。

### 5.4 统一命令流

```text
Key
→ PendingCommand
→ Motion / TextObject / Operator
→ Range
→ Apply
→ UpdateState
→ Render
```

建议优先使用普通函数，而不是大量 trait：

```rust
resolve_motion(...)
resolve_text_object(...)
apply_operator(...)
```

这样可以保持代码短小，避免过度抽象。

### 5.5 Typed Position 和 Range

禁止继续无约束地传递裸 `usize`。定义明确的类型：

```text
Pos
Range
Selection
LineColumn
DisplayColumn
```

明确区分：

- 字符位置
- 字节位置
- 行号
- 终端显示列
- 字符式选区
- 行式选区
- 块式选区

### 5.6 结果类型

当前 `handle_key()` 返回 `bool`，无法区分错误、保存提示和退出。新架构使用明确的 Outcome：

```text
EditorOutcome
├── Continue
├── Render
├── Quit
├── OpenCommandLine
├── SaveRequired
└── Error
```

所有文件错误、剪贴板错误和命令错误必须进入 UI，不得静默丢弃。

### 5.7 历史系统

历史从单字符 Action 改为事务：

```text
一次 d i ( = 一个事务
一次粘贴 = 一个事务
一次 Visual Block 删除 = 一个事务
```

文档维护：

```text
revision
saved_revision
```

`dirty` 通过比较当前 revision 和保存 revision 计算，而不是由每次按键直接赋值。

### 5.8 渲染器

渲染器只接收只读 ViewModel，不直接修改 Editor。

优化目标：

- 维护行索引
- 只渲染可见行
- 避免每次统计完整文档
- 避免每个可见行从文档开头扫描
- 正确处理 CJK 宽字符
- 正确处理组合字符和 Tab
- 过滤终端控制字符
- 修复极小终端尺寸下的光标下溢

### 5.9 文件 IO

首版必须实现：

1. 使用 `PathBuf`。
2. 保留 LF/CRLF。
3. 在目标文件同目录创建临时文件。
4. 完整写入内容。
5. 必要时执行 `sync_all`。
6. 原子替换目标文件。
7. 保存失败时保持 dirty。
8. 只有保存成功才允许退出。

### 5.10 剪贴板

macOS 优先使用成熟的跨平台库，避免继续维护 Wayland、X11、Windows、macOS 四套分支。

必须保证：

```text
读取成功后才删除选区
复制成功后才执行剪切
错误明确反馈
```

## 6. 从当前原型迁移

### 阶段 A：建立新核心

- 新增独立核心模块或 `lib.rs`。
- 引入 typed position/range。
- 建立 Document。
- 为纯文本逻辑编写测试。
- 保留当前二进制作为行为基线。

### 阶段 B：替换输入和状态机

- 将 Crossterm `KeyEvent` 转换为内部 `Key`。
- 实现 Normal 和 Insert。
- 实现 count 和 pending command。
- 实现基础 Motion。
- 实现 `d`、`c`、`y`。

### 阶段 C：迁移历史和文件 IO

- 用事务 History 替换 `src/history.rs`。
- 修复 save/quit 状态机。
- 引入原子保存。
- 保留换行风格和文件权限。

### 阶段 D：实现完整 TextObject

- 优先实现 word、quote、pair。
- 再实现 sentence、paragraph。
- 最后实现 tag。
- 每完成一个对象立即加入 golden tests。

### 阶段 E：实现 Visual 和 Block

- Character Visual。
- Line Visual。
- Block Visual。
- Visual operator。
- Block insert/change/replace。
- `o`、`O` 和 `gv`。

### 阶段 F：恢复完整用户流程

- `/`、`?`、`n`、`N`。
- `:w`、`:q`、`:wq`、`:e`。
- 匿名和系统剪贴板。
- 搜索替换。
- 状态栏和错误消息。

### 阶段 G：删除旧架构

只有新核心通过全部验收后，才删除旧的模式分发、单字符历史、重复扫描渲染和旧剪贴板分支。

## 7. 里程碑

以下为单人全职开发的初步估算。

| 阶段 | 预计 | 交付物 | 验收标准 |
|---|---:|---|---|
| M0 范围与 ADR | 1–2 天 | 命令矩阵、TextObject 规范、依赖决策、benchmark 基线 | 明确所有 P0/P1/Out |
| M1 文档核心 | 3–5 天 | Document、Position、Range、文件 IO、基础渲染 | UTF-8、CRLF、空文件、错误处理测试通过 |
| M2 模态编辑 | 5–7 天 | Normal/Insert、count、Motion、Operator、Undo/Redo | `d2w`、`3dw`、`cw`、`u` 等通过 |
| M3 TextObject | 5–8 天 | 全部标准 TextObject | `daw`、`ci(`、`ya{`、`dat` 等通过 |
| M4 Visual/Block | 3–5 天 | Character/Line/Block Visual | `v`、`V`、`Ctrl-V` 及其操作通过 |
| M5 搜索与文件 | 3–5 天 | `/ ? n N`、Ex 命令、剪贴板、保存退出 | 文件流程无静默错误和数据丢失 |
| M6 性能与安全 | 3–5 天 | 行索引、视口优化、控制字符过滤、macOS 适配 | 达到性能门槛，安全测试通过 |
| M7 发布准备 | 2–3 天 | CI、benchmark、文档、发布流程 | 一条命令完成 fmt/test/clippy/build |

总预估约为 5–7 周，不包含后续高级功能。

## 8. 性能目标

初始目标，后续根据真实机器校准：

| 指标 | 目标 |
|---|---|
| 空文件启动 | p95 < 20ms |
| 1 MiB 文件启动 | p95 < 50ms |
| 50 MiB 文件启动 | p95 < 1s |
| 1 MiB 普通按键 | p95 < 2ms |
| 50 MiB 普通按键 | p95 < 10ms |
| 内存 | 接近文件大小的 1–2 倍 |
| 普通按键路径 | 不允许完整文档扫描 |
| 渲染 | 只处理可见行 |

Benchmark 至少覆盖：

```text
加载
保存
插入
删除
光标移动
行跳转
TextObject
搜索
50 MiB 视口渲染
```

当前 Gap Buffer 的 Gap 移动并不是任意位置 O(1)，因此实现前必须用 benchmark 验证实际复杂度，不能继续使用未经证明的绝对 O(1) 描述。

## 9. 测试策略

### 单元测试

覆盖：

- Position/Range 转换
- Buffer 编辑
- Gap/Rope 操作
- Motion
- TextObject
- Selection
- History
- Register
- Command parser
- Save/load

### Golden Command Tests

使用内部 Key 序列测试：

```text
iHello<Esc>
3dw
daw
ci(
ya{
vap
V3d
Ctrl-V...
```

测试不依赖真实终端，直接验证 Editor 状态转移。

### 属性测试

随机生成：

- 嵌套括号
- 转义字符
- Unicode
- 空行
- 随机编辑序列

使用简单 String 模型作为 oracle，对比新 Buffer 结果。

### 集成测试

- macOS PTY smoke test
- 终端 resize
- 极小终端
- 文件保存失败
- 非 UTF-8 文件
- 剪贴板失败
- 原子替换失败

### CI 门禁

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

CI 增加：

- macOS runner
- Linux runner
- Windows 编译检查
- benchmark 趋势检查
- 依赖漏洞扫描

## 10. 安全与可靠性要求

- 文件内容、文件名和状态消息中的控制字符必须过滤或转义。
- 保存必须是原子操作。
- 加载和保存错误必须显式反馈。
- 剪贴板失败不能导致选区被删除。
- 非 UTF-8 路径不能被静默转换为另一个路径。
- 终端初始化失败必须回滚 raw mode。
- 极小终端尺寸不能触发下溢或 panic。
- Windows FFI 或替代剪贴板实现必须进行边界审计。

## 11. 完成定义

v0.1 只有在以下条件全部满足后才算完成：

- 所有标准 TextObject 命令可用。
- Visual Block 完整支持。
- count、Operator、Register、Undo/Redo 组合行为稳定。
- 保存失败不会退出。
- 粘贴失败不会删除选区。
- 终端控制字符不会直接执行。
- 50 MiB 文件不会因每次按键产生明显卡顿。
- UTF-8、CRLF、Unicode 显示正确。
- 核心逻辑测试覆盖关键路径。
- macOS 可作为日常编辑器使用。
- README 中的所有性能声明都有可复现 benchmark。

## 12. 实施顺序

1. 完成 M0 的 ADR、命令矩阵和 benchmark 基线。
2. 先实现纯文本核心和测试，不接终端。
3. 再实现 Normal/Insert、count、Motion 和 Operator。
4. 完成全部标准 TextObject。
5. 实现 Visual 和 Visual Block。
6. 接入文件、搜索、命令和剪贴板。
7. 做性能、安全和 macOS 平台优化。
8. 建立 CI、发布流程和用户文档。

任何新增功能都应先证明它属于高频工作流，并且不能破坏命令正交性、响应速度或核心代码简洁性。

## 13. 当前实施进度

已完成第一批基础实现：

- 新增独立核心库 `src/lib.rs` 和 `src/core/`。
- `Document` 已切换到 `ropey`，保留字符位置 API。
- 已实现类型化位置、范围、Motion、Operator 和标准 TextObject 解析器。
- 已支持 Normal、Insert、Visual Character、Visual Line、Visual Block、Operator-pending。
- 已支持 count、`d/c/y`、基础行操作、插入事务、撤销/重做、寄存器、搜索和 `n/N`。
- 已接入新的应用层、终端渲染、文件加载、原子保存和未保存退出保护。
- 已支持 `:w`、`:q`、`:q!`、`:wq`、`:x`、`:e`、`:number` 和字面量 `:s/old/new/[g]`。
- 已生成 `Cargo.lock`，移除阻塞 macOS 构建的 `winres` 依赖。

当前验证结果：

```text
cargo check --offline
cargo test --offline
cargo clippy --offline --all-targets --all-features -- -D warnings
cargo build --release --offline
```

全部通过。下一批重点是系统剪贴板、Save As、Unicode 显示宽度、Visual Block 的原子事务、正则替换子集和 benchmark。
