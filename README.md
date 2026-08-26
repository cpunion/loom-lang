# loom-lang

[![compiler CI](https://github.com/cpunion/loom-lang/actions/workflows/ci.yml/badge.svg)](https://github.com/cpunion/loom-lang/actions/workflows/ci.yml)

状态：**Core 0.1–0.3 Executable Reference + C3 Multi-Package Repository Gate**

阶段：Core 0.1–0.3 已接入同一条 source → package graph → checked MIR → LLVM object → native executable 工具链；解释器保留为显式语义对照后端，三个冻结任务及一个 3-package/24-module repository workload 的正确性、性能、优化 IR 与 fuzz smoke 已进入自动门禁

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
- callable 省略返回类型时固定返回 `Unit`，不做返回类型推断；
- 非 `Unit` 结果必须进入使用位置或写 `discard expression`；普通具体值默认可显式丢弃，`MustScope`/未消费 `Task` 及其递归包装除外；
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
- composition bundle 与大型工程组合治理；文件系统/HTTPS registry dependency、认证发布、可信离线缓存、lockfile、只激活 optional dependency 的 package feature，以及 bin/test/lib target 已实现。

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
| 安装、LLVM 19 探测、release 校验与回滚 | [安装与发布](docs/10-installation-and-release.md) |
| Text/Bytes/Path、TextMap、JSON、typed I/O 与日志 | [Core 0.3 最小标准库](docs/11-standard-library.md) |
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

基础多包工程使用 `loom.toml`、path/文件系统或 HTTPS registry dependency、显式 feature 和 bin/test/lib target；manifest 的 `language = "0.3"`（省略时同值）固定源码语义和证明域，并进入 package/artifact/cache identity，未知版本拒绝加载。`loomc resolve` 生成锁定语言版本、registry 版本与 SHA-256 的 `loom.lock`，`--locked` 禁止隐式变更，`resolve --update` 才重新选择最高兼容 SemVer；`--offline` 只接受完整复核过的本地 bundle cache。feature 只激活显式 optional dependency，不做源码 `cfg`、隐式 import 或运行时注册。bin/test 可直接闭环，lib target 产出经过完整 MIR 校验的 portable `.loomlib`，不冒充尚未定义的稳定 native/FFI ABI。[application manifest](examples/packages/application/loom.toml) 可直接闭环：

```sh
cargo run -p loom-cli -- check --target app examples/packages/application
cargo run -p loom-cli -- resolve examples/packages/application
cargo run -p loom-cli -- build --target app --output target/package-app examples/packages/application
cargo run -p loom-cli -- test --target unit examples/packages/application
cargo run -p loom-cli -- run --target app examples/packages/application
cargo run -p loom-cli -- build --target utility --output target/utility.loomlib examples/packages/utility
```

网络 registry 配置使用 `{ url = "https://…", token-env = "LOOM_REGISTRY_TOKEN" }`；token 只从具名环境变量读取，任何带 token 的明文 HTTP 在发请求前拒绝，认证响应正文也不进入诊断。`loomc publish --registry NAME PATH` 发布确定性 source bundle；下载必须同时通过 index SHA-256、bundle 内嵌 package/version/language identity、路径和大小限制。cache 每次读取都会重新验证原始 bundle digest 与全部物化文件，sidecar 本身不是信任根。无认证 HTTP 仅允许 literal loopback 地址用于本地测试。

源码命令默认使用项目内 `target/loom/cache/v2` 的内容寻址缓存；`--cache-dir DIR` 可改位置，`--no-cache` 可做冷路径对照。`loomc cache stat` 可查看引用、blob 和可回收空间，`loomc cache prune` 只清理显式版本目录中的损坏引用与孤立 blob。缓存当前真实复用逐文件 lossless token/AST、带 package identity 的 canonical module public-interface、经过 decoder 与 MIR validator 的整图 checked MIR、按 root/witness reachability 裁剪的 LLVM target object，以及解释器 `.loomi`/portable `.loomlib` artifact。native executable 与 macOS dSYM 在 hermetic link bundle 能覆盖 linker 子工具、SDK/sysroot、CRT 和系统库前刻意不做持久缓存；每次从已验证 object 重新链接/生成 dSYM。不可达私有函数的等长实现修改会使 checked MIR miss，但可继续命中 object 并快速重链；损坏 ref/blob 只会安全 miss并重建。

`llvm` 是默认 backend；`--backend interpreter` 显式选择 `.loomi` 对照路径。把命令中的 `core01` 换成 `core02` 即走 static concept、associated type、readonly/mutable interface dispatch；换成 `core03` 即走 scoped/defer、后缀 `.await`、cleanup-aware `Result` 后缀 `?`、timer/fd readiness、可存储单 Task、静态 tuple 与动态 list join，以及 `all/settled/any/race`。Core 0.3 标准库还提供 `Text`/`Bytes`/`Path`、不可变 `TextMap[V]`、有界 canonical JSON、typed async file/socket I/O 与 canonical JSON-line logging。`File`/`Socket` 是 `MustScope`，由最内层块的 `scoped` 自动关闭。Rust native runtime 由 `LoomRuntime` 统一拥有 moving-GC `Heap`：需要运行时的同步 root 只创建并激活 Runtime，async root 才在同一 Runtime 上附加单线程 Executor；纯且无 fault、签名完全由 `Unit`/`Bool`/`Int`/`Float` 组成的 root 两者都不创建。Executor 的 macOS kqueue/Linux epoll reactor 和 blocking-I/O worker mailbox 都按首次真实使用懒初始化。平台无关 WaitSource/Registration ABI、真正返回 `Pending` 的 scheduler 以及取消/drain 均已闭环；Task fault record 实行 first-fault-wins，后续 fault record 返回成功以让 cleanup 继续，但不会覆盖最初的 primary fault。LLVM 的 numbered state dispatch 可恢复线性表达式及 if/match/block 内的 await。浮点 codec、标准值、scheduler、reactor、I/O 与 GC 均在同一个 Rust static runtime 中，不再编译 C++ runtime。`cargo test --workspace --all-targets` 固化 parser、静态语义、MIR 校验、LLVM verifier、native artifact、runtime reactor/GC、标准库双后端 fixture、CLI 和 LSP 的回归证据。

当前 native runtime ABI 总版本是 v7，精确 identity 为 `loom-value-v2/layout-v1/text-v1/wait-v1/task-v2/runtime-v1/gc-v7/shadow-stack-v1/witness-v1/int-list-v1/stdlib-v4`。LLVM 同步函数已经发射 collection-boundary-specific precise shadow-stack bitmap；managed allocation slowpath 可在分配前 collection，Rust helper 以 `RuntimeRootScope`/`NodeStream` 和 stable full-`Value` output 保持中间值可重定位。普通值 clone/node build 使用 Rust runtime 的非递归 work stack，不再生成 LLVM module-local 递归 helper。当前单线程 generated-code interval 不存在外部 collection request，编译器因此不在普通 loop backedge 或 return 插入伪 poll；只有真实可能移动对象的 allocator/clone/build/callee 边界才发布 root state。coroutine suspension 的 live-local metadata 来自正常、取消和 `defer` cleanup edge 共用的结构化 CFG backward liveness，并由 MIR validator 独立重算；Executor 只负责 task/join 与 wait 调度，不承载同步 root bitmap 或 runtime helper root scope。

跨函数 POD record concrete ABI 的 compiler-private 首阶段已经闭合：eligible direct/inherent callable 可以用 first-class LLVM aggregate 返回无 invariant、单态、直接 primitive-field 的 record；同类普通 by-value 参数和 readonly receiver 也使用 aggregate Value ABI，只有 `mut self` 使用 call-scoped InOut pointer。当前 call-site 证明有意只接受 flat `Copy(place)`，覆盖 native 或 universal caller 中已有 private stack-record storage 的参数，以及 direct/tail/显式 `return`/`if` 结果 producer；fallible callable 使用 `{status, aggregate}` 并保持 fault 传播。合同/invariant、nested/managed/generic POD、任意非 flat POD 参数表达式、export/unsupported boundary、统一 layout/clone/trace/drop plan 和更完整的 coroutine/`dyn` specialization 仍走 fallback 或留待后续，不把这条闭世界快路误报为一般 aggregate ABI。

LLVM 开发构建使用 O0 + global DCE，`--release` 切到 O2 + global DCE；省略 target triple 时使用实际宿主 CPU/features，任何显式 triple（即使规范化后等于宿主）都使用 generic/empty-feature portable policy。profile、规范化 target triple、data layout 与实际 CPU/features 都进入 object/cache identity。`loomc build --target-triple aarch64-unknown-linux-gnu --emit object ...` 会用对应 LLVM TargetMachine 产生真实 relocatable object。executable 交叉链接必须同时显式提供由目标平台 Loom 工具导出的 runtime bundle 和目标 linker；缺少它们仍以 `CrossLinkUnavailable` 拒绝，bundle 的 triple/data layout/runtime ABI 或 archive digest 不匹配也会在调用 linker 前失败，绝不误链宿主 runtime：

```sh
# 在目标平台工具安装中导出；发布包也自带该宿主平台的 runtime/ 目录。
loomc runtime export --output target/loom/runtime
loomc --target-triple aarch64-unknown-linux-gnu \
  --runtime-bundle /opt/loom/aarch64-linux-runtime \
  --linker aarch64-linux-gnu-clang \
  build --output target/app examples/core01
```

runtime archive 独立强制使用 generic CPU 且不附加 CPU feature；v2 manifest 明确记录并只接受该 portable runtime policy。manifest/runtime archive 和显式 linker executable 在每次链接前后重新验证；bundle 目录只允许 manifest 声明的 bounded regular archive，不接受额外文件、symlink 或路径穿越。这些验证不假装能覆盖 linker 隐式读取的全部宿主输入，因此不用于 native final-artifact cache key。

LLVM object 带稳定相对源码的函数/statement line table；Linux ELF 直接携带 DWARF，macOS 每次 native link 后重新生成标准 dSYM。Ubuntu 24.04 + LLVM 19 CI 会执行 workspace fmt/check/clippy/test、LLVM 与 interpreter 双闭环、package target 和 DWARF 验证；回归还覆盖 48-module call graph、512 one-shot completions 与 12-writer CAS contention。长驻 `AnalysisHost` 已按 module interface/semantic-shape/body 指纹复用未改 module 的 typed-HIR body semantics；任一声明形状变化会安全退回整图检查。跨进程仍以 validated checked-MIR 整图缓存为边界，不声称序列化 typed-HIR body。live、AST 编辑、AOP-like 组合、所有权语法和 operator runtime 不进入当前实现。

`loomc debug` 固定使用 LLVM development profile，在项目根目录启动 macOS 默认 LLDB、其他平台默认 GDB，也可用 `--debugger PROGRAM` 接入包装器。LLDB 收到 `EXECUTABLE -- ARGS...`，GDB 收到 `--args EXECUTABLE ARGS...`；调试进程继承终端，临时 executable 与 dSYM 在整个会话中保持有效。解释器、release、JSON 和交叉目标模式会显式拒绝，不伪装成源码调试。

`cargo run --release -p loom-quality` 执行冻结的 Core 0.1–0.3 双后端 main/test oracle、release LLVM root/DCE 统计、1.8 MB parser、32 次 artifact decode/validate、64-module 单 body 增量复用与 wall-clock 上界，并输出可归档 JSON。独立 `fuzz/` workspace 为 lossless syntax/recovery 和 checked-MIR artifact decoder/validator 提供 libFuzzer target；两者都在 Linux CI 持续运行。

基础跨语言性能基准另由 `loom-benchmark` 对照 Loom release LLVM、Go、Rust、C 与 C++。v1 在相同动态输入和 checksum 下记录一次优化构建（Loom 禁用项目缓存，其他工具保留各自标准工具链缓存策略）、native runtime 原始样本及 artifact size。当前 native 已落地 closed-world `FAULT`/`COLLECT`/`EXECUTOR` runtime-requirement 图，以及 `Unit -> i1`、`Bool -> i1`、`Int -> i64`、`Float -> double` 的 primitive private ABI；pure/no-fault body 没有隐藏 context/status，fallible body 保留 status/context。eligible POD record local 仍由入口栈节点承接并经 release SROA 提升；direct/inherent producer 直接返回 first-class aggregate，flat `Copy(place)` 的普通 by-value 参数和 readonly receiver 也按 aggregate Value 传递，只有同类 `mut self` method 使用 call-scoped InOut pointer。status result、fault propagation、direct/tail/显式 return/if producer，以及 universal caller 已有 private stack-record 参数都不再经过 clone/build managed chain。合同/invariant、nested/managed/generic POD、任意非 flat 参数表达式和 export/unsupported boundary 仍保留当前 universal `Value` fallback 与独立 managed value 语义。eligible 同步不逃逸局部 `List[Int]` 使用 compiler-private contiguous `{data, len, cap}`：canonical append loop 以 SSA 携带 header，只在 reserve 成功后重载 data/cap，并在元素写入后立即提交 len；对同一稳定长度的零起点完整 scan，closed-world 证明可删除逐元素 upper-bound 与不可达 `None` edge。POD 私有节点和 native list/scalar 临时都不进入 shadow roots，无 managed allocation 的循环及 return 也不生成 GC poll。terminal fault/status edge 带明确的 unlikely 权重，fault sink 标记为 cold/noinline，普通业务分支不因此带偏置。append/exact 子证明失败时 List 保留 checked generic-native 路径；只有 native-storage eligibility 失败才回退当前 universal `Value` lowering。

递归 `Int` 的 checked-entry/assumed-body 也已落地。分析按 MIR 结构工作，不识别 Fibonacci 函数名；但当前有意只接受纯、同步、非泛型、无 receiver/合同/witness 的单个 immutable `Int -> Int` 函数，局部仅限 immutable `Unit`/`Bool`/`Int`，且只能 direct self recursion。编译器从 0 开始对稠密输入做有界精确求值，最多检查到 128；每条递归边必须严格降低到已完成输入，所有语法上的整数运算和递归 site 也必须在成功域内被覆盖。当前 `fibonacci` 由此得到 `0..92` 安全域；checked entry 用 unsigned `<= 92` guard，因此负数自然落入 checked slow path，域内则进入纯 `i64 -> i64` assumed body，不携带 context/status，也不调用 overflow helper。外部 direct call 只在参数的完整推导 range 被安全域包含时才直接调用 assumed body；`fibonacci(93)` 和其他域外调用仍走 checked 路径并保留 `IntegerOverflow` 语义。任一别名、mutation、间接分派、suspension、cleanup、合同、未覆盖分支或非递减递归都 fail closed；这不是一般递归或源码 effect system。

这些都是 closed-world/compiler-private 快路：四种 primitive scalar ABI，以及上述 POD aggregate Value 参数/结果、readonly receiver 与 `mut self` InOut 首阶段已可跨函数，List 优化仍局限在函数内。`NativeLayout` catalog 描述 scalar 与无 invariant、单态、直接 primitive-field 的 POD record；callable `NativeSignatureShape` 只在 emitter、storage 和 requirement analysis 都支持的边界启用它。requirement 图为每个 callable 区分 invocation、universal body、native checked body 和可选 assumed body，不会因为同一源码函数而错误共享 managed materialization 的 `COLLECT`。POD universal fallback 是合同/invariant、非 flat 表达式、generic/export 与其他 unsupported boundary 的语义实现，不是旧语法兼容层；未被引用时 release global DCE 会删除它。下一步 P0 是合同/invariant 的 checked-entry/private-body，以及统一 layout/clone/trace/drop plan；完整 machine-instance graph、nested/managed POD、generic/`Text` direct ABI 和一般 container/coroutine/`dyn` layout 仍在后续阶段。warm/incremental build、peak RSS 和固定机器趋势属于下一层证据；繁忙共享主机上的手动 wall-time 只可作诊断，不能写成稳定性能结论。它与 `loom-quality` 的回归上界分开，不把一次共享机器结果解释为语言排名；完整方法、运行命令、证据边界和 P0–P2 后续计划见[质量与受控证据](docs/09-quality-and-controlled-evidence.md#31-跨语言基础基准)。

```sh
cargo +1.88.0 run --release -p loom-benchmark -- --help
cargo +1.88.0 build --release -p loom-cli -p loom-benchmark
target/release/loom-benchmark --output target/basic-benchmark.json
```
