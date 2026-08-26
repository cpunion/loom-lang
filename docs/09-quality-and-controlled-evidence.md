# 优化、性能、模糊测试与受控任务证据

状态：Active / C3 Multi-Package Repository Gate

日期：2026-08-25

本文固定实现质量证据，不增加 Loom 源码语法或运行时语义。三项冻结任务继续提供 C2 oracle；提交到仓库的 `examples/c3` 另提供一个非生成的 3-package/24-module workload，在解释器与 LLVM 后端上执行同一 main/test，并记录 package、module、checked MIR、root reachability 和时间数据。这形成最小 C3 repository gate，但不声称已经完成人类开发效率对照、长期生产运维或对所有领域普适。

## 1. 冻结任务

| 任务 | 源码 | SHA-256 | 主要机制 |
|---|---|---|---|
| constrained-contracts | `examples/core01/shop.loom` | `f3c6b8cad23cf4113e7555ac29d2307d853af10eff4ee89482ef4c8617a77472` | refined value、invariant、method contract、mutation |
| concept-polymorphism | `examples/core02/concepts.loom` | `60bc7e21bd475ae3fb0f795f25cbae92e4d86c7c48675abad02c9561d2701d4a` | static concept、associated type、first-class `dyn C` |
| structured-async | `examples/core03/tasks.loom` | `0981e9597a0a450c4a4bc035568be1e57fe50bb746dd827c7471aab45c0dae2d` | scoped/defer、Task、await、tuple/list join、取消 |

哈希变化会让门禁失败。任务语义确需修改时，必须同时审阅 oracle 并显式更新哈希，不能把漂移当作新证据。

每项任务必须满足：全项目静态检查无错误；解释器全部 `test fn` 通过且 `main` 返回 `Unit`；release LLVM native test harness 全部通过且 native `main` 输出 `Unit`。报告同时记录源码 bytes/lines/tokens、完整 MIR function/test 数、run/test root 的实际可达函数数，以及 analysis、解释执行、native build 和 native execution 时间。

另有一个不含 `main` 的异步回归门 [`fixtures/async-generic-contracts`](../fixtures/async-generic-contracts/main.loom)：它要求 conditional conformance proof 在首次 suspension 后仍由 Task frame 持有，并同时覆盖 async `requires`/`ensures` fault、`Task.settled` 与 `Task.any` sibling cancellation。`loom-quality` 会在解释器与 release LLVM native test harness 中各执行一次，避免只靠单 crate 测试或 development profile 证明该路径。

## 2. C3 repository workload

[`examples/c3`](../examples/c3/README.md) 固定一个 checkout service 形状的多包工程：`foundation` 提供 constrained values、record、enum 和 dynamic concept；`catalog` 作为 direct path dependency 提供 conformance 与业务组合；`application` 显式声明它实际 import 的两个 direct dependencies，并提供 bin/test target。门禁要求：

- 3 个 package、24 个源码 module 都进入 package-aware source/semantic identity；
- interpreter 与 LLVM 的 `application.main.start` 和两个普通 test 结果一致；
- 报告保存所有源码的 canonical SHA-256、完整 MIR 数及 main/test 实际 root 数；
- package isolation、transitive import、同名 module 注入和依赖写保护由独立负例测试验证，不能靠 workload 恰好不触发来代替。

## 3. 性能与增量门

`loom-quality` 使用绝对上界来阻止数量级退化、意外全图重查和挂死，不把一次 CI wall clock 当成语言运行时排名：

| 门 | 上界或结构要求 |
|---|---|
| 每任务 analysis | 10 s |
| 每任务 interpreter main + tests | 15 s |
| 每任务 release native main + tests build | 60 s |
| 每任务 native main + tests execution | 15 s |
| 约 1.8 MB lossless parser/recovery | 8 s |
| Core 0.3 checked-MIR artifact decode + validate 32 次 | 15 s |
| 64-module 单 body 修改 | 最多重查 1 module，至少复用 63 module，10 s |
| C3 repository analysis/native build | 15 s / 90 s |
| async generic contracts interpreter/native build/native run | 15 s / 60 s / 15 s |
| 整套受控任务 | 300 s |

门限故意高于正常机器的毫秒级结果，以吸收共享 CI 抖动；任何收紧都应先保留多次 runner 证据。结构门比 wall-clock 更强：单 body 修改若退化为全图检查，即使机器足够快也立即失败。

运行并保存机器可读报告：

```sh
cargo run --release -p loom-quality | tee target/c3-evidence.json
```

LLVM 19 CI 在每次 push/PR 运行该命令并上传 JSON artifact。报告包含规范化 target triple 与实际优化 pipeline，不能拿不同 target/profile 的数字直接比较。

### 3.1 跨语言基础基准

`loom-quality` 是回归门，不是横向排名。另设的 `loom-benchmark` 只回答一个更窄的问题：在同一台机器、同一组输入和可核验输出下，Loom 当前 release LLVM 路径与 Go、Rust、C、C++ 执行若干常用小程序时，一次优化构建、运行时间和产物大小处于什么量级。Loom 构建显式关闭自身项目缓存；其他工具仍保留各自标准工具链缓存策略，因此该单样本只作工程信息。它不改变语言规范，也不以一次数字证明某门语言整体更快。

每个 case 必须遵守：

- 五种实现使用同一算法、数值范围、输入规模和可观察 checksum；若语言语义无法公平对齐，该 case 必须拆分或排除，不能用不同算法换取更好数字；
- runner 独立计算 checksum，并把它作为动态 `EXPECTED` 参数交给五个 executable；程序必须在退出前比较实际结果，runner 再核验成功状态和固定输出。编译失败、运行失败、输出漂移或工具缺失直接使本次报告失败，不以零耗时参与汇总；
- runtime sample 使用独立进程、monotonic clock、明确 warmup 和多次 measurement，报告原始样本以及 median/min/max，不只保存最佳值；
- v1 build sample 对每种语言从源码构建一次 executable，Loom 显式使用 `--no-cache`；这项单样本数据只描述当前 cold build，不能当稳定统计。后续 warm/no-change 与单 body incremental suite 必须另表记录，不得把 Loom 持久缓存命中与其他语言冷编译直接比较；
- Loom 使用 release LLVM native artifact；Go、Rust、C、C++ 使用各自明确记录的 release/优化参数。禁止把 Loom interpreter、debug build 或不同目标架构混入 native runtime 表；
- v1 JSON 保存 OS、architecture、CPU、逻辑核数、编译器版本与完整命令、source SHA-256、profile/deadline、case/scale/checksum、原始纳秒样本、统计值和 artifact bytes；机器或工具链 identity 不同的报告不得直接合并排名；
- standard profile 保持冻结的基础规模；独立 `--throughput` profile 使用 int/record 100,000,000、list 10,000,000、fib 40 和默认 2 次 warmup/5 次测量，目的是让热计算更充分地压过进程启动噪声，仍不是语言排名。5 个样本的 p05/p95 等同 min/max，不能解释成稳定尾延迟；
- standard/throughput profile 在 build 前记录 1 分钟 load average，超过逻辑 CPU 数的 75% 时默认拒绝测量；`--allow-busy-host` 只用于明确保存带噪声的诊断结果，不能让该结果进入稳定趋势。quick correctness smoke 不做此限制；
- runtime timed region 是每个独立子进程从 spawn 到退出的 wall time，包含参数解析、实际计算、动态 checksum 比较和固定 `Unit` 输出，不包含 build、runner JSON 编码或其他 case。进程开销对所有实现一致，但极短 case 仍不应用来判断热点。quick/standard/throughput 每次 fixture 默认 deadline 分别为 10/30/60 秒，可用 `--timeout-seconds` 覆盖；超时进程必须被 kill/wait 回收并使报告失败，不能让算法退化无限挂起或继续采样；
- 在繁忙共享主机上手动放大 case 并读取 LLVM IR、汇编或 retired-instruction counter，只能定位结构变化，例如确认热循环少了固定次数的 header load。retired instructions 不在 v1 JSON 内；只有在同 binary/ISA/profile/scale 下保存原始样本、命令和 commit 才能作结构对照，同一次手动运行的 wall time 和语言倍数不得回填为受控 benchmark 结果；
- peak RSS、目标 triple/data layout、机器内存、能耗和 profiler 样本尚不在 v1 JSON 内；需要分析分配、GC 或 ABI 热点时必须作为后续同机 profile 证据补充，不能从 wall time 猜测原因。

基础入口由 workspace 的 benchmark runner 提供；具体参数以 `--help` 和 [`benchmarks/basic`](../benchmarks/basic/README.md) 为准：

```sh
cargo +1.88.0 run --release -p loom-benchmark -- --help
cargo +1.88.0 build --release -p loom-cli -p loom-benchmark
target/release/loom-benchmark --output target/basic-benchmark.json
target/release/loom-benchmark --throughput --output target/basic-benchmark-throughput.json
```

基准报告是当前实现的工程数据，不是语言规范、营销排名或 CI 接受语义。共享 CI 只适合 correctness smoke 与宽松的数量级退化检查；用于趋势比较的数字应来自固定硬件的定时任务，并保留完整原始报告。小型 synthetic case 也不能替代真实服务、长期运行、并发负载或人类开发效率证据。

## 4. LLVM 优化证据

development 使用 `default<O0>,globaldce`，release 使用 `default<O2>,globaldce`，且两者在优化前后都通过 LLVM verifier。机器 IR 回归进一步固定：

- 不可达私有函数在两个 profile 都不进入 object；
- closed-world callable graph 固定传播独立的 `FAULT`/`COLLECT`/`EXECUTOR` invocation/universal-body/native-checked-body/assumed-body requirements；POD native result 不继承 universal materialization 的 `COLLECT`，root IR 证明普通同步路径不创建 Executor、async 路径建立 Runtime + Executor，runtime fixture 另行证明 reactor/worker 按需创建；
- development 保留可达的 checked constant arithmetic helper；
- release 常量折叠并内联该 helper，移除对应 overflow intrinsic 与 machine function；
- eligible pure/no-fault concrete `Int` 私有 body 直接使用 `i64 fn(i64...)`，没有 status 或隐藏 pointer，pure root IR 也没有 Runtime/Executor creation；普通不可证明的 checked arithmetic 和合同路径继续使用 `{status, i64}` 加 context，并保留 root fault 回归；
- compiler-private checked-entry/assumed-body 不按函数名特判，但仅接受纯、同步、单 immutable `Int -> Int`、direct 且严格递减的 self recursion。编译器对稠密 `0..=128` 输入有界精确求值，要求所有整数运算/递归 site 都被成功域覆盖；Fibonacci 安全域是 `0..92`。checked IR 存在 unsigned `<= 92` guard（负数进 slow path），assumed IR 只有纯 `i64`、`nsw` arithmetic 和直接 assumed self call，没有 context/status/overflow helper；外部完整 range 证明只在被安全域包含时直调 assumed body。负数、`fibonacci(93)`/其他域外输入、非递减边、未覆盖分支、合同/有副作用操作/alias/mutation 均有 fail-closed 或 checked-overflow 回归；
- eligible primitive record local 在 development IR 使用有预算的 `record.local` 栈节点；ordinary by-value 参数、readonly receiver 和 direct/inherent POD result 以 first-class aggregate Value 传递，只有 `mut self` 使用 call-scoped InOut pointer。IR/native 回归覆盖 flat `Copy(place)` 参数、universal caller 的 private stack-record 参数、tail/显式 return/if producer、POD status result、fault propagation 与 InOut 写回。unsupported universal boundary 才从当前字段物化独立 managed chain，不让私有栈地址逃逸；fallback 保留 balanced root/build，未引用时 release global DCE 删除。POD header/field storage 不进入 shadow roots，普通 loop/return 不生成 GC poll；release golden/benchmark record 热路没有 node traversal、clone/build、root 或 GC allocation，checked accumulator 仍保留；
- eligible 同步不逃逸局部 `List[Int]` 在 development/release IR 中使用 compiler-private `{data, len, cap}` storage：canonical append loop 具有 preheader data/len/cap load 和 loop phi，非增长 backedge 不重载 header，reserve-success 只重载 data/cap，element store 后立即提交 len；storage/index/element 临时不进入 shadow roots，循环没有 synthetic GC poll，所有 status return 恰好 drop 一次；
- exact-length scan 证明只接受空初始化、零起点单 append build、相同 constant/immutable end、唯一 induction binding、无中间 mutation 和 direct exhaustive `Option[Int]` match；命中时 release IR 没有逐元素 past-end/`None` edge，所有不匹配形状保留 checked get，checked checksum 仍使用 overflow intrinsic；
- compiler-generated terminal fault/status branch 带 branch-weight metadata，context fault sink 是 cold/noinline；普通 `if`、match pattern 与业务 `Result` 分支保持无权重。runtime fixture 另固定 Task first-fault-wins，第二次 fault record 不覆盖 primary fault；
- escaping、`List[Text]`、async、multiple/conditional append 与观察顺序 hazard 有明确 universal 或 generic-native fallback 回归；
- LLVM synchronous shadow-stack 的 safepoint state/bitmap、Rust runtime clone/build ABI、collect-before allocation、runtime partial aggregate roots、stdlib stable output 和 coroutine CFG exact liveness 均有 moving/forced-collection 或 validator 回归；Executor 只提供 Task root source，不承载同步 root metadata；
- release/native 仍通过同一合同、checked integer 和双后端结果 oracle。

因此这里验证的是实际 IR 变化，不只验证命令行 profile 字符串不同。

## 5. 持续模糊测试

独立的 `fuzz/` workspace 避免普通 stable build 依赖 libFuzzer。当前 target：

- `syntax`：任意 UTF-8 的 lexer losslessness、token/span 单调性、parser recovery、diagnostic span 边界；
- `artifact`：任意 bytes 以及从合法 seed 产生的结构化 mutation，贯穿 JSON nesting/header、Float side table、entry 与完整 checked-MIR validator。
- `semantics`：生成有界 constrained-integer 程序，交叉检查 direct proof elimination、静态失败、运行时 validation 和解释执行，防止“消除检查”变成不可靠证明。

CI 用 nightly coverage instrumentation 各运行 20 秒。崩溃必须先最小化，再升级为普通 deterministic regression；只保存 fuzzer artifact 不算修复。具体本地命令见 [`fuzz/README.md`](../fuzz/README.md)。

## 6. 证据边界

当前可以称为 C3 implementation-controlled repository evidence：固定任务与多包 workload 的语义一致性、package boundary、root/DCE、编译执行预算、增量选择性以及恶意输入边界都有自动门禁。仍不能据此声称 Loom 比某门成熟语言更易开发，也不能把一个 24-module workload 外推为大型生产仓库的长期收益；这些结论仍需要预注册的人类 A/B 任务与独立外部项目证据。

## 7. 整体进展与后续顺序

当前既定 Core 0.1–0.3 范围已经完成 C0 规范闭合和 C1 native executable reference；三个冻结任务形成 C2 oracle，仓库内 3-package/24-module workload 形成 implementation-controlled C3 gate。这里的“C3”只描述仓库内持续运行的工程证据，不表示已有独立外部生产项目、跨语言性能优势或人类开发效率结论。

### P0：建立性能基线并关闭可信边界

1. v1 Go/Rust/C/C++/Loom 基础 runner、等价源码、checksum oracle 和原始 JSON 已落地；下一步把 correctness smoke 接入 PR，并在固定机器建立可比较的定时趋势，再扩展 cold/warm/incremental build、peak RSS 与 profiler 报告。
2. concrete scalar、递归 checked-entry/assumed-body 和 POD record 跨函数首阶段已完成：closed-world requirements 让 pure/no-fault scalar call 使用无 status/context 的私有 ABI，fallible path 使用 `{status, value}` 加 context。递归分析不硬编码 fib，但仅对纯、单 immutable `Int -> Int`、strict-descending direct self recursion 在稠密 `0..=128` 上精确求值，并要求全部 arithmetic/recursive site 被成功域覆盖。fib 因此有 `0..92` 安全域；checked entry 用 unsigned guard 把负数和其他域外值留在 checked slow path，域内 assumed body 是无 context/status/overflow helper 的纯 `i64`，外部完整 range 证明也只在被该域包含时直调。`fib(93)` 和其他域外输入仍保留 checked `IntegerOverflow`；别名、mutation、间接调用、suspension、cleanup、合同、非递减边或未覆盖分支都 fail closed，所以这不是一般递归/effect system。eligible POD ordinary by-value 参数、readonly receiver 和 direct/inherent result 使用 first-class aggregate Value，只有 `mut self` 使用 InOut pointer；flat `Copy(place)`、universal caller private stack-record、tail/显式 return/if、status result 与 fault propagation 均有回归。native body 不物化 managed result，完整 universal fallback 仍保持合同/invariant、nested/managed/generic、非 flat expression 和 export/unsupported boundary 的深复制、root 与 moving-GC 语义；release 可以删除未引用 fallback。record 静态字段读写和局部栈存储仍交给 LLVM SROA，因此 benchmark 热路没有 node allocation/traversal。
3. `list_build_scan` 的算法级退化及首轮热路冗余已关闭：eligible 同步不逃逸局部 `List[Int]` 使用 compiler-private contiguous `{data, len, cap}`，几何扩容使 append 摊销 O(1)，length/get 为 O(1)，且纯 `Int` storage 在所有退出上显式释放而不进入 GC。canonical append range 通过 loop SSA 避免非增长迭代的 header reload，reserve 后只重载 data/cap并立即提交新 len；相同稳定 end 的 exact scan proof 删除不可达的逐元素 upper-bound/`None` edge，但保留 checked checksum。closed-world use scan 对 escape、copy、async、generic/witness/defer、multiple append、end/binding 不同与观察顺序不确定形状 fail closed，并回退到 checked generic-native 或 current universal `Value` lowering；因此这里不承诺稳定 List ABI，也不把只适用于 `Int` local 的路径外推到 generic container。
4. terminal fault cold-layout hints 与 primary-fault 语义已由独立回归固定：生成的 terminal fault/status edge 是 unlikely，fault sink cold/noinline，普通控制流不被错误加权；native Task 实行 first-fault-wins，第二次 fault record 不覆盖最初 code/message/detail。
5. `NativeLayout` 已统一 scalar 与无 invariant、单态、直接 primitive-field POD record 的 shape-only 分类，record 栈存储、requirement graph 与 emitter 复用该 catalog；`NativeSignatureShape` 已为四种 primitive scalar 及 POD aggregate Value 参数/结果、readonly receiver、`mut self` InOut 建立共同 private-ABI selector。POD universal/native checked body 分开求 summary，eligible 递归 scalar 另有可选 assumed-body summary；first-class aggregate 不冻结 public `sret`/`byval`，分类本身仍不会绕过 storage/GC/逃逸证明。下一步 P0 是带 invariant/requires/ensures callable 的 checked entry + assumed private body，以及把 scalar/POD/List 特判收敛为统一 layout/clone/trace/drop plan 与 machine-instance identity；随后按 profiler 证据推进 nested/managed POD、generic container、`Text` direct ABI、known generic instance 和 coroutine/`dyn` layout descriptor。任意非 flat POD 参数表达式在具备共同 value-plan 前继续 fallback；不得改变值相等、checked overflow、合同、GC tracing 或 concept proof 语义。
6. 把用户定义 `MustScope` 的 canonical obligation identity 带入 versioned checked MIR，使 artifact/cache validator 能独立复核，不长期停留在只由 sema 保证的信任边界。
7. 保持 Core 0.1–0.3 双后端、标准库、package/cache、LLVM verifier、fuzz 和 release bundle 门持续通过；性能变化不能靠关闭合同、检查或 cleanup 获得。

### P1：优化 ABI、增量和开发体验

1. 在 P0 的 contracts/invariant 与统一 layout plan 上继续扩展 nested/managed aggregate、generic container 与 coroutine ABI，并在有实测收益时增加 hot-site specialization 或单态化；canonical type/proof arguments 作为独立 machine-instance cache entry，而不是继续堆叠按源码函数共享的 ad-hoc fast path。
2. 把当前长驻 `AnalysisHost` 的 module body selective reuse 扩展到可验证的跨进程 per-module typed-HIR/semantic cache；整图 checked MIR 仍保留为完整 artifact validation 边界。
3. 只有真实 async API 需要时，才实现原子 Task reparent、取消传播和失败回滚，随后按证据放宽 TaskCarrier async 参数/返回及 partial container transfer；源码仍不增加 ownership、borrow 或 lifetime。
4. 在 LLVM release/native 主后端稳定的前提下评估 Cranelift fast-dev backend，并补充 Loom 值 debugger pretty-printer 和更大的非生成工程 fixture。

### P2：独立阶段能力

- WebAssembly/WASI、32-bit/更多发布平台，以及有实际二进制、SIMD 或 FFI 需求时才加入的精确宽度整数；
- stable FFI/dynamic-library/plugin witness ABI 和仅限边界的 pin protocol；
- 多线程 shared-memory executor、持久化 coroutine、分布式执行，以及 generator/stream/actor 等独立高层类型；
- 预注册的人类 A/B 任务和由独立外部项目产生的长期生产证据。

universal `any` 到 `dyn C` 的运行时 conformance 搜索、所有权/借用语法、自研 machine backend、AOP-like/live/AST 编辑和 desired-state/operator runtime 不因性能基准自动进入排期；如需重启，必须另做最小例子、DCE/root、artifact 与运行时边界裁决。
