# loom-lang 编译过程与后端定案

状态：Normative Toolchain Design + Core 0.1–0.3 LLVM/Package/Cache C1 Reference

日期：2026-08-24

本文固定从普通 `.loom` 源码到 native executable 的完整流程。范围只含源码、静态类型、受约束值/合同、concept、多态、自动内存管理、词法清理、结构化 Task、编译产物和工具链；不含 live、AST 编辑、AOP、operator runtime 或所有权语法。Core 0.3 的语言语义见 [GC、词法清理与异步任务定案](08-memory-cleanup-and-async.md)。

## 1. 后端边界

编译器分为稳定前端和可替换后端：

```text
source files
  → project/module discovery
  → lexer + parser
  → HIR + name resolution
  → static types + contracts + concept proof
  → checked MIR
  → root reachability + generic/witness instances
  → backend IR
  → object file
  → platform linker
  → native executable
```

`checked MIR` 是唯一可信后端输入。后端不得重新猜名字、conformance、associated type、合同顺序或 mutability；MIR validator 失败属于 compiler defect，不能降级执行。

当前 crate 边界：

| crate | 职责 |
|---|---|
| `loom-syntax` | token、parser、恢复、AST |
| `loom-hir` | declaration/body identity 与 source map |
| `loom-sema` | 名字、类型、place、合同、concept proof、coercion |
| `loom-lowering` | typed HIR → executable MIR |
| `loom-mir` | compiler-private typed IR 与 fail-closed validator |
| `loom-codegen-llvm` | roots、live witness、LLVM、object、link |
| `loom-interpreter` | 显式选择的语义 oracle，不是默认 build/run 后端 |
| `loom-driver` | manifest/package/target graph、内容缓存与 CLI/LSP 共享 snapshot |
| `loom-cli` | check/build/test/run/fmt 的 host boundary |

## 2. command 与根集合

前端检查范围和 native codegen 可达范围必须区分。

### `loomc check`

发现并解析选定 package graph 中的全部 `.loom` module，对全部 declaration/body 做静态检查；不因函数当前不可达而跳过类型或合同错误，也不生成 LLVM IR/object。

### `loomc build --entry main` / `--target name`

无 manifest 时默认 root 是 export `main`，显式 `--entry name` 选择另一个 export。manifest bin target 通过 `--target name` 选择其 entry；仅有一个 bin target 时可以省略。`--target` 与 `--entry` 互斥。native artifact 固定该入口；运行已构建 artifact 时不重新选择入口。

### `loomc test`

选定 test target 后，当前 package graph 的全部 `test fn` 是 roots，按稳定名称输出结果。空 test suite 是成功的空 harness。测试使用与普通函数相同的 parser、checker、MIR、LLVM 与合同路径。

### library/package target

未来 library target 的 roots 是 manifest 明确列出的 public exports 和 ABI metadata，不是“所有 `pub` 自动永久保留”。动态库/FFI/plugin 若进入设计，必须单独定义 open-world roots；当前没有该行为。

## 3. 调用图与动态边

从 roots 做闭包是合理且必需的，但不能只扫描直接 `call`：

```text
direct/inherent call        → concrete FunctionId
async constructor           → constructor + resume/cancel/trace descriptor
await/join                  → child edges + live join/runtime helpers
static concept call         → selected witness method
generic call                → function + type/witness instance
dyn construction/coercion   → live witness table
dyn method call             → live witness × used requirement slot
builtin                     → compiler/runtime symbol
```

当前实现将 root analysis 与 LLVM emission 分开。`ReachableProgram` 稳定记录：

- reachable functions；
- reachable witnesses；
- 每个 witness 真正使用的 requirement slots。

仅存在一个 `impl C for T` 不形成 live edge。可达代码必须实际构造或传递该 witness，table 才进入候选集合；动态调用也只保留同 concept 的已 live witnesses。LLVM 优化后再运行 global DCE。

## 4. 泛型与实例

语义上泛型在定义处检查。后端允许三种等价实现：

1. concrete type 单态化；
2. uniform representation + witness 参数；
3. 两者混合，并在 hot/known call site specialize。

当前 C1 LLVM 后端使用 uniform compiler-private Value ABI，使 generic function 可以共享 machine body；static concept proof 通过 witness argument 传递，concrete call 仍可被 LLVM 内联和去虚化。未来优化不得改变 checked overflow、value copy、mutation、Violation 或合同结果。

缓存中的完整实例键应是：

```text
InstanceKey = (
  definition identity,
  canonical type arguments,
  canonical witness proof arguments,
  target triple + data layout,
  compiler/backend/stdlib version,
  optimization + contract mode,
)
```

即使当前共享一个 generic body，也应保留该抽象键，避免将来切换单态化时破坏缓存模型。

## 5. LLVM 后端

当前定案：

- LLVM 19.1；
- Rust 使用 `inkwell 0.10` 的 `llvm19-1-prefer-dynamic` feature；
- 不直接编写 `llvm-sys` FFI 或项目内 unsafe binding；
- module 设置 host target triple 与 LLVM data layout；
- LLVM verifier 在优化前后各执行一次；
- 当前 optimization pipeline 是 `default<O2>` 加 global DCE；
- TargetMachine 输出 native object；
- object emission 与 final link 是独立 API/cache 边界；native object 由 `clang` 链接，可由 `LOOM_CC` 覆盖；
- 用户函数和 statement/expr span 生成 DWARF line table；Linux ELF 直接保留 DWARF，macOS 在 object 尚存时用 `dsymutil` 生成标准 dSYM，并把 DWARF payload 与 executable 同 key 缓存；
- compiler-private runtime 是 Cargo 构建并嵌入 codegen crate 的 Rust static library；float codec、moving GC、Task scheduler 与 reactor 共用该 runtime，不编译 C++ 源码。

选择 Inkwell 的原因是它在 Rust 侧为 LLVM C API 提供更强类型的安全封装，支持本项目需要的 LLVM 版本，并且与 workspace 的 `unsafe_code = forbid` 边界相容。binding 仍是 pre-1.0 依赖，因此版本必须锁入 `Cargo.lock`，升级 LLVM/Inkwell 时运行完整 native golden。

## 6. compiler-private ABI

第一版 native ABI 优先语义完整与 generic sharing，不承诺公开稳定布局。

### 普通值

MIR value 在 LLVM 层使用统一 tag/payload representation。record/enum/refined value 的 payload 由 compiler-managed nodes 表示；copy 做逻辑深拷贝，move 可以转移当前表示。该布局只能被 codegen/runtime helpers 观察。

### 函数

内部函数使用统一调用形状：

```text
status fn(out_value, argument_nodes, witness_nodes)
```

正常返回写入 `out_value` 并返回 0；ContractFault/RuntimeFault 走非零 status。业务 `Result.Err` 仍是普通返回值，绝不与 status 混淆。

### 接口

`dyn C` 在 checked MIR 中表示 concrete data 与已选 conformance proof 一起流动，不规定 LLVM 类型或内存布局。静态可知调用直接落到 implementation；后端也可以把 data/proof 拆成 SSA 参数或完全消除接口值。当前 C1 只在间接调用幸存时使用 compiler-private data/witness pair，并沿用同一内部函数签名。

witness table 不是语言必须存在的对象。若当前后端选择物化它，只允许为 live requirement slot 引用 method。root/witness reachability 在表示选择之前完成，因此 DCE 不依赖 fat-pointer、table prefix、运行时 type id 或 registry。

后端优化优先级是：去虚化/内联 → 调用签名特化 → 分离 SSA data/proof → 必要时物化 pair。单指针表示不作为 KPI；只有未来长期存储、目标 ABI 或实测结果确有收益时，才可以局部选择 box/header/tagged representation，并且不得改变显式 conformance、proof flow、fault 或 DCE 结果。

### managed object 与 GC

Core 0.3 的 native ABI 使用 compiler/runtime-private layout tag、精确 frame slots、safepoint 和 root protocol；它们不进入源码或稳定 library ABI。当前 stackless lowering 只在一次 `resume` 返回之后收集，因此所有跨 safepoint live value 都已位于 Task frame/result roots，不需要让裸 SSA 地址跨越 safepoint。collector 追踪 `Value` 与 aggregate `ValueNode`，回收不可达对象、复制存活对象并重写 frame/runtime roots；Task 指针和 immutable witness metadata 留在非移动区。runtime fixture 会保存旧地址并验证根被改写、垃圾被回收，同时 differential tests 保持 value copy、equality、contract、concept dispatch 与 fault 一致。

### Task 与 coroutine

`Task[T]` 是单个 managed pointer。每个 reachable async constructor 形成对 compiler-generated `resume`、`cancel`、`trace` 和 frame/result descriptor 的闭合边；descriptor 类似静态 witness table，但不是用户 concept、fat-pointer 第二字段或 runtime registry。

Loom MIR 先把 async body降低成 numbered suspension/cancellation states，再由 LLVM 发射普通 control flow。后端不直接采用 Rust `Future`/`Pin` 表面，也不把 C++ `promise_type` customization 暴露给语言。join node、ready queue 和 wait registration 属于 root-scoped runtime；child completion 只能 enqueue waiter，不能在 runtime callback 栈上直接重入 continuation。

当前 native runtime 已落地 version 1 的平台无关 `WaitSource`/`Registration`/`ReadyNotification` C ABI，macOS backend 为 kqueue，Linux backend 为 epoll；timer 用同一 poll wait 的 monotonic deadline timeout，不依赖第二套 callback runtime。registration 是 generation-checked one-shot handle；fd 明确登记 readable/writable，child completion 使用独立 notification source。LLVM 的每个 run/test root 复用同一个 executor；async constructor 返回单指针 Task，`resume` 在 wait/join 未完成时返回 `Pending`，notification 只把 Task 加回 ready queue。numbered state dispatch覆盖线性 chain 与 if/match/block 内的 await；取消按挂起 state 进入对应 cleanup unwind。`Task.sleep`、`Task.waitReadable`、`Task.waitWritable`、可存储 Task、tuple/list join 和四种 join mode 均走该路径。

## 7. 数值与目标平台

- Loom `Int` 永远是 checked signed i64；
- 不采用随目标改变位宽的默认 `int`；
- pointer width 只存在于 LLVM data layout 和分配边界；
- index/length 进入地址运算前检查非负与 target address range；
- 将来可显式增加 `I8/I16/I32/I64`、`U8/U16/U32/U64`；
- `ISize/USize` 若因 FFI 必须加入，只属于低层边界，不成为默认算术、公共协议或持久化类型。

这保证跨 32/64 位 target 的语言算术与合同结果一致，同时允许 object/cache 按 target triple 分开。

## 8. artifact 与命令闭环

默认命令：

```sh
loomc check PATH
loomc build [--target NAME | --entry main] [--output target/loom/program] PATH
loomc test [--target NAME] PATH
loomc run [--target NAME | --entry main] PATH
loomc run --artifact target/loom/program
```

`build` 产生平台 native executable；`run PATH` 在临时目录执行相同编译流程；`test` 生成 native test harness。当前 build metadata 不承诺 reproducible binary bytes，因为系统 linker 可能加入平台 metadata；前端/MIR/cache identity 必须 deterministic。

解释器只通过以下形式显式选择：

```sh
loomc --backend interpreter build --output program.loomi PATH
loomc --backend interpreter run --artifact program.loomi
```

它用于 differential testing、diagnostics 与 bootstrap，不得让默认 `build/test/run` 偷回解释执行。

## 9. 多 module、package 与缓存

### 分层

- module：源码名字空间和 import 节点；
- package：`loom.toml` 管理的一组 modules、版本和显式依赖；
- target：root package 上的 bin/test root policy 和 artifact kind；
- crate 不是 Loom 源码关键字；Rust workspace crate 只属于编译器实现。

### manifest v1

当前只实现可离线、可重复解析的本地依赖闭环：

```toml
schema = 1

[package]
name = "application"
version = "0.1.0"
sources = ["src"] # 默认值

[dependencies]
utility = { path = "../utility", version = "^1" }

[[target]]
name = "app"
kind = "bin"
entry = "application.start" # 默认 main

[[target]]
name = "unit"
kind = "test"
```

dependency path 相对当前 manifest；依赖包也必须有 schema v1 manifest。resolver 检查 SemVer、循环依赖、重复 package identity、越界 source root 和重复 target。source label 使用 `src/...` 与 `deps/name@version/...`，因此 package 整体移动不改变 FileId 顺序或缓存 identity。dependency alias 只属于 manifest graph；源码名字空间仍由显式 `module`/`import` 决定，不做隐式 alias 重写。当前没有 registry、网络解析、lockfile、feature、library/dynamic target 或 open-world plugin root。

### 为什么需要缓存

单文件 Core 不要求持久缓存，但多 module/package 必须有。没有缓存会在每次改动后重复 parse、type-check、instantiate 和 object emission；root graph 本身不能替代增量依赖追踪。

### 已实现缓存边界

默认 cache root 是 root package 下的 `target/loom/cache/v1`。`--cache-dir DIR` 只改变存储位置，`--no-cache` 强制冷编译；JSON 模式输出 `cache_result` 的 layer、hit/miss/disabled 和 key。

当前真实缓存五个边界：

1. 每个 UTF-8 source 的 lossless token/AST；命中会直接跳过 lexer/parser，并重新绑定当前稳定 FileId；
2. 每个 module 的 canonical public interface；包含 import、exported declaration、generic bound、concept requirement、associated type、contract、public inherent method 与 conformance header，排除实现 body 和 source range；
3. 整张 package graph 的 validated checked MIR 与稳定 diagnostics；
4. 已选 run/test root 的 LLVM target object；key 只包含全局 type/concept schema、reachable function body、live witness method/proof edge、target/data layout/optimization 与 debug-source policy，不包含不可达私有函数 body；
5. 最终 native executable、macOS dSYM payload 或解释器 `.loomi` artifact。

CAS 把 ref 与 SHA-256 blob 分开。读取时验证 schema、namespace、key、size 与完整内容 hash；checked MIR 再经过 versioned `.loomi` decoder 和完整 MIR validator，native artifact 只有在内容验证后才原子 materialize 并设置执行权限。写入为 blob-first、ref-last 的同目录原子替换；并发或损坏只能退化为 miss。最终输出路径不进入 key，因此相同 target 可 materialize 到不同位置。

整图 key 以 length-delimited fields 编码，包含 compiler source + Rust version fingerprint、debug/release profile、MIR/backend/stdlib/runtime ABI、canonical package/dependency/target graph、稳定 source path 与 bytes、target triple/data layout、CPU/features、optimization/relocation 和 contract mode。编译器 fingerprint 只散列 workspace-relative compiler source path/content，不含 checkout 绝对路径。LLVM target identity 来自 emission 共用的同一个 TargetMachine policy，而不是另写一份猜测值；final native artifact 的派生 key 还包含精确 Rust runtime archive SHA-256、所选 linker 与 macOS `dsymutil` 的 version identity，工具 identity 无法确认时停用 final-artifact cache 而不影响前端检查。

### 增量层与当前边界

```text
source hash
  → lossless token/AST cache                         已实现并复用
  → public interface fingerprint                    已实现并缓存
  → whole-graph typed HIR/check + checked MIR       MIR 已缓存；sema 仍整图
  → reachable function/witness/proof fingerprint    已实现
  → target object cache                             已实现并复用
  → runtime/linker/debug-tool keyed final link       已实现并复用
```

这里把两种主张严格分开：不可达私有 body 修改已经可以复用同一 target object/final artifact，测试还会在破坏 final ref 后证明只重新链接；但 checked-MIR miss 后，当前 semantic analyzer 仍对整张 package graph 的全部 declaration/body 做检查。module interface fingerprint 已为 selective query 提供正确依赖键，尚未据此宣称“无关 module 不重新 type-check”。当前 shared-generic ABI 没有独立 monomorphized machine instance；generic/witness proof 参数直接进入 reachable object fingerprint，将来增加单态化时再把 canonical type/proof arguments 拆成 instance CAS entry。

cache 必须 content-addressed，不以 mtime、绝对路径、文件遍历顺序或编辑器状态作为语义输入。每个 entry 至少包含：

- schema/compiler/backend/stdlib version；
- canonical module/package identity；
- source/interface/body hashes；
- dependency fingerprints；
- target triple、CPU feature policy、optimization；
- contract/runtime ABI version。

cache miss 与冷构建必须得到同一 checked MIR、diagnostics ordering、reachability 和运行结果。当前 relocation/content/corruption、逐源 parse hit、interface body-insensitivity、DCE-aware object hit、dSYM relocation、12-writer contention 与 CLI hit 测试固定这一行为；cache corruption 只能造成安全 miss/重建，不能执行未验证 IR。

## 10. `any` 与 DCE 边界

当前没有 universal `any`，并禁止运行时做：

```text
A → any → dyn C
```

因为这会要求保留或注册 `A` 的所有潜在 conformances，使跨 module points-to 很快退化并阻碍 DCE。未来 `any` 只能先显式还原 concrete type，或在包装时已经携带所需 witness；open-world reflection/plugin registry 必须作为独立 artifact mode，并把 registry 明确列为 roots。

## 11. 其他后端

后端接口保留，但当前只把 LLVM 设为 release/native 主后端。

| 后端 | 推荐角色 | 取舍 |
|---|---|---|
| LLVM | 默认 AOT/release、跨目标、优化与 LTO | 依赖和编译延迟较重，但生态与优化最完整 |
| Cranelift | 未来 fast-dev backend、快速 object/JIT、差分测试 | 编译快、Rust 集成自然；优化深度和 target 范围通常小于 LLVM，API 仍需跟随版本 |
| WebAssembly | 未来 sandbox/portable artifact target | 需要单独定义 WASI/host ABI，不替代 native backend |
| C source | bootstrap/debug escape hatch，不作为主后端 | 工具链普遍，但难保留精确 MIR/合同/调试与优化控制 |
| 自研 machine backend | 暂不推荐 | 维护 calling convention、register allocation、object/debug format 的成本不符合当前阶段 |

Cranelift 已提供 codegen、frontend、multi-function module、object 与 JIT crates，适合作为第二实现验证后端抽象；在 LLVM native 闭环、缓存和调试信息稳定前不并行实现。

## 12. 验收

当前必须持续通过：

1. Core 0.1、Core 0.2 与 Core 0.3 在 LLVM/interpreter 两个后端的 `check/build/test/run`；
2. build artifact 被平台识别为 native executable；
3. source run 与 artifact run 结果一致；
4. static generic/conditional witness native 回归；
5. mutable interface method 正常写回 owner place；
6. unreachable conformance 不进入 live witness/method graph；
7. LLVM verifier 优化前后通过；
8. Rust float codec 与 interpreter 的 canonical boundary 一致，且生成物不依赖 C++ runtime；
9. interpreter differential tests 保留，但默认命令不使用解释器。

Core 0.3 的新增关门条件：

10. `scoped`/`defer` 在 normal return、fault 和 cancellation 上保持同一 LIFO 顺序；
11. async fixture 的 source run、native artifact run 与 interpreter 一致；
12. Pending task 使用 wait registration/ready queue，不忙轮询；
13. static tuple join 与 dynamic list join 保持 input-order result layout；
14. `all/any/race` 的 sibling cancellation 在返回前 drain cleanup；
15. moving GC 能重定位 coroutine frame/results，同时不改变 Task identity、合同或 concept behavior；
16. async descriptor 和 join runtime edge进入 root graph，未构造的 task/conformance 仍可被 DCE；
17. manifest path dependency、SemVer、cycle、bin/test target 与稳定 dependency source label 通过 driver/CLI 回归；
18. cache relocation identity 不含绝对路径，内容变化 miss，损坏 blob 安全 miss/修复，第二次 checked-MIR/final-artifact 构建真实 hit。
