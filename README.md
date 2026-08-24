# loom-lang

状态：**Core 0.1–0.3 Executable Reference + C2 Implementation-Controlled Gates**

阶段：Core 0.1–0.3 已接入同一条 source → package graph → checked MIR → LLVM object → native executable 工具链；解释器保留为显式语义对照后端，冻结任务的正确性/性能、优化 IR 与 fuzz smoke 已进入 CI

日期：2026-08-25

`loom-lang` 先实现一组小而完整的 Core 0.1：代数数据类型、函数与方法、显式失败、穷尽匹配、模块、基本泛型、普通测试、受约束值与契约编程。在此基础上，Core 0.2 已确认由同一个 `concept` 同时支持静态泛型约束和显式动态派发。

目标是先回答一个更小的问题：

> 一门普通静态语言能否让“合法值、合法对象状态、函数契约和可复用行为接口”成为编译器持续执行的语言事实，同时保持熟悉、可阅读的文本编程体验？

## 已确认核心基线

- `record`、`enum`；
- `fn` 与带 receiver 的 method；
- `Option[T]`、`Result[T, E]`；
- 穷尽 `match`；
- `module`、显式 import 与 public/private 边界；
- rank-1 基本泛型；
- 普通 `test`；
- `type Price = Float where self >= 0` 一类名义受约束类型；
- proof-classified construction：`Price(10.0)` 静态成立时直接得到 `Price`，未知输入才得到 `Result[Price, ConstraintError]`；
- proven contract elimination：有独立类型/路径依据的 `requires`、`ensures`、invariant 与 `assert` 不进入 checked MIR，unknown/failing 路径仍保留完整 fault/blame；
- `invariant`、`requires`、`ensures`、`assert`；
- 默认只读 receiver 与显式 `mut self`。

紧随 Core 0.1 的 **Core 0.2 已确认扩展**是：

- 唯一的行为抽象 `concept`、显式 conformance 和 associated type；
- `T: Concept` 的定义处检查与静态派发；
- Go 风格书写的接口参数 `value Display`，具体实参只在静态证明存在时自动适配；
- `dyn concept` 与 `dyn C` 表示携带显式 conformance proof 的一等擦除接口，可返回、存储和嵌套；copy 隔离 logical data，物理布局不是语言 ABI，当前 LLVM C1 仅在间接派发仍然存在时使用 compiler-private data/witness 表示；
- 不引入 `view[...]`、borrow、lifetime、`box/shared` 等所有权语法。

Core 0.1 权威基线见 [最小语言核心规范](docs/02-language-design-baseline.md)，具体书写见 [核心表面与代码风格](docs/03-surface-and-style.md)；Core 0.2 的行为抽象见 [concept 与多态规范](docs/05-concepts-and-dynamic-polymorphism.md)。lexer/parser、数值、failure、Task 表面、native artifact 和工具边界见 [Core 0.1–0.3 可执行合同](docs/06-executable-contract.md)，完整编译流程见 [编译过程与 LLVM 后端](docs/07-compiler-pipeline-and-backends.md)。

[GC、词法清理与异步任务定案](docs/08-memory-cleanup-and-async.md)已经进入 Core 0.3：自动且地址不可观察的可移动 GC；块级 `scoped`/`defer`；stackless coroutine；单指针 `Task[T]`；显式后缀 `.await`；ready-queue executor；静态异构 tuple join、动态同构 list join，以及 `all/settled/any/race`。源码仍不增加 ownership、borrow、lifetime、`Pin` 或用户可实现的 coroutine trait。

## 尚未确认

以下仍需单独讨论和小实验，不属于 Core 0.1：

- AOP-like 静态组合、注入点、贡献与排序；
- desired-state、operator 与持续调和；
- capability/provider 与一般 effect system；
- 多线程 shared-memory executor、分布式执行和持久化 coroutine；
- `example`、`scenario`、`property` 等专用验证声明；
- 网络 registry 发布/认证、composition bundle 与大型工程组合治理；文件系统 registry dependency、lockfile、只激活 optional dependency 的 package feature，以及 bin/test/lib target 已实现。

普通 `test` 已足够验证当前核心；不会为了未来能力提前保留关键字或运行时模型。

`concept` 描述一个类型提供哪些显式操作；`dyn concept` 只把其中可擦除的 receiver methods 投影为动态调用。它不扫描实现、不按名字注入行为，也不等同于 AOP contribution、capability/provider 或运行期插件发现。

## 文档权威关系

| 主题 | 权威文档 |
|---|---|
| 项目边界和裁决原则 | [项目章程](docs/00-charter.md) |
| Core 0.1 语义 | [最小语言核心规范](docs/02-language-design-baseline.md) |
| Core 0.1 表面写法 | [核心表面与代码风格](docs/03-surface-and-style.md) |
| Core 0.2 concept/dyn 语义与表面 | [concept 与动态多态规范](docs/05-concepts-and-dynamic-polymorphism.md) |
| Core 0.1–0.3 parser/checker/runtime 可执行合同 | [Core 0.1–0.3 可执行合同](docs/06-executable-contract.md) |
| root graph、LLVM、artifact、缓存边界 | [编译过程与 LLVM 后端](docs/07-compiler-pipeline-and-backends.md) |
| GC、scoped/defer、Task、coroutine 与 join | [GC、词法清理与异步任务定案](docs/08-memory-cleanup-and-async.md) |
| 优化、性能预算、fuzz 与受控任务证据 | [质量与受控证据](docs/09-quality-and-controlled-evidence.md) |
| 实现顺序与开放问题 | [核心能力分期](docs/04-capability-stages.md) |

[历史设计草案](docs/draft/README.md)保存此前的声明式组合、AOP-like 与 desired-state/operator 方案；其中 [Checkout 对照实验](docs/draft/03-checkout-composition-experiment.md)及其 [fixture](docs/draft/04-checkout-composition-fixture.md) 均不是当前语言规范。

## 当前交付形态

Core 0.1 采用普通、静态的工具链：

```text
.loom 普通文本
  -> lexer / parser / type checker / contract checker
  -> diagnostics / executable program / ordinary tests
  -> standard LSP

Git add / commit / branch / merge 仍是普通 Git
```

workspace 已提供 `loomc`、LLVM native artifact、普通测试 runner、formatter 和 LSP；解释式 `.loomi` 只作为显式对照后端。Core 0.1、Core 0.2 与 Core 0.3 的验收源码分别是 [shop.loom](examples/core01/shop.loom)、[concepts.loom](examples/core02/concepts.loom) 和 [tasks.loom](examples/core03/tasks.loom)，三者都有可执行 `main`：

```sh
cargo run -p loom-cli -- check examples/core01
cargo run -p loom-cli -- build --output target/core01 examples/core01
cargo run -p loom-cli -- test examples/core01
cargo run -p loom-cli -- run examples/core01
cargo run -p loom-cli -- run --artifact target/core01
cargo run -p loom-cli -- debug --debugger lldb examples/core01
```

`loom-lsp` 与 CLI 复用长驻 `AnalysisHost`，现已提供 diagnostics、hover、definition、references、prepare rename/rename、语义 completion、document symbols 和 workspace symbols。引用与重命名按定义身份覆盖跨文件全局声明以及 callable 内的泛型参数、参数和局部变量；源码存在错误时会拒绝生成不完整的引用编辑。

基础多包工程使用 `loom.toml`、path/文件系统 registry dependency、显式 feature 和 bin/test/lib target；manifest 的 `language = "0.3"`（省略时同值）固定源码语义和证明域，并进入 package/artifact/cache identity，未知版本拒绝加载。`loomc resolve` 生成锁定语言版本、registry 版本与 SHA-256 的 `loom.lock`，`--locked` 禁止隐式变更，`resolve --update` 才重新选择最高兼容 SemVer。feature 只激活显式 optional dependency，不做源码 `cfg`、隐式 import 或运行时注册。bin/test 可直接闭环，lib target 产出经过完整 MIR 校验的 portable `.loomlib`，不冒充尚未定义的稳定 native/FFI ABI。[application manifest](examples/packages/application/loom.toml) 可直接闭环：

```sh
cargo run -p loom-cli -- check --target app examples/packages/application
cargo run -p loom-cli -- resolve examples/packages/application
cargo run -p loom-cli -- build --target app --output target/package-app examples/packages/application
cargo run -p loom-cli -- test --target unit examples/packages/application
cargo run -p loom-cli -- run --target app examples/packages/application
cargo run -p loom-cli -- build --target utility --output target/utility.loomlib examples/packages/utility
```

源码命令默认使用项目内 `target/loom/cache/v2` 的内容寻址缓存；`--cache-dir DIR` 可改位置，`--no-cache` 可做冷路径对照。`loomc cache stat` 可查看引用、blob 和可回收空间，`loomc cache prune` 只清理显式版本目录中的损坏引用与孤立 blob。缓存当前真实复用逐文件 lossless token/AST、带 package identity 的 canonical module public-interface、经过 decoder 与 MIR validator 的整图 checked MIR、按 root/witness reachability 裁剪的 LLVM target object，以及最终 native/`.loomi` artifact。不可达私有函数的等长实现修改会使 checked MIR miss，但可继续命中 object/final-link；损坏 ref/blob 只会安全 miss并重建。

`llvm` 是默认 backend；`--backend interpreter` 显式选择 `.loomi` 对照路径。把命令中的 `core01` 换成 `core02` 即走 static concept、associated type、readonly/mutable interface dispatch；换成 `core03` 即走 scoped/defer、后缀 `.await`、cleanup-aware `Result` 后缀 `?`、timer/fd readiness、可存储单 Task、静态 tuple 与动态 list join，以及 `all/settled/any/race`。`standard.time.milliseconds` 提供平台无关 `Duration`；`standard.file.open_read/create` 和 `standard.net.connect` 返回真实异步 `File`/`Socket` task，这两类 `MustScope` 资源由块级 `scoped` 自动关闭。Rust native runtime 提供平台无关 WaitSource/Registration ABI、macOS kqueue/Linux epoll reactor、真正返回 `Pending` 的单线程 scheduler、取消/drain 和精确 moving GC；LLVM 的 numbered state dispatch 可恢复线性表达式及 if/match/block 内的 await。浮点 codec、scheduler、reactor、I/O 与 GC 均在同一个 Rust static runtime 中，不再编译 C++ runtime。`cargo test --workspace --all-targets` 固化 parser、静态语义、MIR 校验、LLVM verifier、native artifact、runtime reactor/GC、CLI 和 LSP 的回归证据。

LLVM 开发构建使用 O0 + global DCE，`--release` 切到 O2 + global DCE；profile、规范化 target triple 与 data layout 都进入 object/cache identity。`loomc build --target-triple aarch64-unknown-linux-gnu --emit object ...` 会用对应 LLVM TargetMachine 产生真实 relocatable object；非宿主 executable 因尚无对应 Rust runtime archive/linker 而以 `CrossLinkUnavailable` 拒绝，不会误链宿主 runtime。

LLVM object 带稳定相对源码的函数/statement line table；Linux ELF 直接携带 DWARF，macOS 生成并随 final artifact 缓存标准 dSYM。Ubuntu 24.04 + LLVM 19 CI 会执行 workspace fmt/check/clippy/test、LLVM 与 interpreter 双闭环、package target 和 DWARF 验证；回归还覆盖 48-module call graph、512 one-shot completions 与 12-writer CAS contention。长驻 `AnalysisHost` 已按 module interface/semantic-shape/body 指纹复用未改 module 的 typed-HIR body semantics；任一声明形状变化会安全退回整图检查。跨进程仍以 validated checked-MIR 整图缓存为边界，不声称序列化 typed-HIR body。live、AST 编辑、AOP-like 组合、所有权语法和 operator runtime 不进入当前实现。

`loomc debug` 固定使用 LLVM development profile，在项目根目录启动 macOS 默认 LLDB、其他平台默认 GDB，也可用 `--debugger PROGRAM` 接入包装器。LLDB 收到 `EXECUTABLE -- ARGS...`，GDB 收到 `--args EXECUTABLE ARGS...`；调试进程继承终端，临时 executable 与 dSYM 在整个会话中保持有效。解释器、release、JSON 和交叉目标模式会显式拒绝，不伪装成源码调试。

`cargo run --release -p loom-quality` 执行冻结的 Core 0.1–0.3 双后端 main/test oracle、release LLVM root/DCE 统计、1.8 MB parser、32 次 artifact decode/validate、64-module 单 body 增量复用与 wall-clock 上界，并输出可归档 JSON。独立 `fuzz/` workspace 为 lossless syntax/recovery 和 checked-MIR artifact decoder/validator 提供 libFuzzer target；两者都在 Linux CI 持续运行。
