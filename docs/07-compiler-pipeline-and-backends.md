# loom-lang 编译过程与后端定案

状态：Normative Toolchain Design + Core 0.1–0.3 LLVM/Package/Cache C1 Reference

日期：2026-08-26

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

validator 还在 artifact/cache 边界复核可由 MIR 类型形状证明的 obligation：`Evaluate` 不得丢弃直接或递归携带 `Task`、预置 `File`/`Socket` 或未解析泛型义务的值，`MakeView` 也不得把这些义务擦除进 `dyn`。因此 `fn erase[T: C](value T) dyn C` 仍会 fail closed：`T: C` 只提供 conformance proof，没有证明未知 `T` 不隐藏 `MustScope` 或 Task obligation。用户定义的 `MustScope` 仍由 sema 保证，因为当前 MIR 没有可独立恢复的 canonical `MustScope` concept identity；在该 identity 进入 MIR 前，validator 不得靠 concept 名字猜测。

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

无 manifest 时默认 root 是 export `main`，显式 `--entry name` 选择另一个 export。manifest bin target 通过 `--target name` 选择其 entry；仅有一个 bin target 时可以省略。`--target` 与 `--entry` 互斥。native 与解释器 artifact 都固定该入口；`.loomi` 在 versioned envelope 中保存并校验所选 export，运行已构建 artifact 时不重新选择入口。

### `loomc test`

选定 test target 后，当前 package graph 的全部 `test fn` 是 roots，按稳定名称输出结果。空 test suite 是成功的空 harness。测试使用与普通函数相同的 parser、checker、MIR、LLVM 与合同路径。

### library/package target

manifest `kind = "lib"` target 不声明 entry。`loomc build --target name` 产出 versioned、经 decoder/MIR validator 校验且不绑定宿主平台的 checked-MIR `.loomlib`；相同制品可由内容缓存恢复。它保存当前 package graph 的 public export map，但不是 native archive、动态库或稳定 FFI ABI，`run`/`test` 会拒绝把它当 executable target。动态库/FFI/plugin 若进入设计，必须单独定义 ABI 与 open-world roots；当前没有该行为。

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

当前 C1 LLVM 后端采用混合实现。generic、`dyn`、一般 aggregate 和外部入口仍使用 compiler-private universal `Value` lowering，使 generic function 可以共享 machine body；static concept proof 通过 witness argument 传递。同步、非泛型且参数/结果完全由 primitive scalar 组成的 direct call 已生成私有 typed body 并保留 universal wrapper：`Unit -> i1`、`Bool -> i1`、`Int -> i64`、`Float -> double`。无 invariant、单态、直接 primitive-field 的 POD record 还支持一个有意收窄的跨函数首阶段：eligible direct/inherent producer 以 first-class LLVM aggregate 返回，eligible `mut self` method 以 call-scoped InOut pointer 接收 owner，其他参数仍须是 primitive scalar。closed-world native summary 为 pure/no-fault 时，body 没有 status 或隐藏 context；可能 fault/collect 时仍返回 `{status, value}` 并接收 context。安全的同步 POD local 把字段节点放入有预算的入口栈存储，release 由 SROA 把热循环字段提升为 SSA；private result/InOut call 不物化 managed chain，whole-value copy 或普通参数、readonly/contract/generic/export 等 universal boundary 仍建立独立 managed value。同步不逃逸的局部 `List[Int]` 另有 compiler-private contiguous storage。其余 concrete/layout specialization 仍是后续工作，且不得改变 checked overflow、value copy、mutation、ConstraintError 或合同结果。

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
- module 设置规范化后的 host 或显式 `--target-triple` 及该 TargetMachine 的 LLVM data layout；省略 triple 时使用实际 host CPU/features，任何显式 triple 都使用 generic/empty-feature policy；
- LLVM verifier 在优化前后各执行一次；
- development pipeline 是 `default<O0>` 加 global DCE；`--release` 切换为 `default<O2>` 加 global DCE；
- compiler-generated terminal fault/status branch 带 unlikely metadata，`loom_context_raise_fault_v1` 标记为 cold/noinline；普通 `if`/`match`/业务 `Result` 分支不套用该提示；
- TargetMachine 输出所选 triple 的 relocatable object；
- object emission 与 final link 是独立 API/cache 边界；native object 由 `clang` 链接，可由 `LOOM_CC` 覆盖；
- 用户函数和 statement/expr span 生成 DWARF line table；Linux ELF 直接保留 DWARF，macOS 在 object 尚存时用 `dsymutil` 生成标准 dSYM，并把 DWARF payload 与 executable 同 key 缓存；
- `loomc debug` 只走 development LLVM native 路径，在 project root 启动 LLDB/GDB 并继承终端；稳定相对源码因此可直接解析，临时 executable/dSYM 的生命周期覆盖完整调试会话；
- compiler-private runtime 是 Cargo 构建并嵌入 codegen crate 的 Rust static library；float codec、moving GC、Task scheduler 与 reactor 共用该 runtime，不编译 C++ 源码。

选择 Inkwell 的原因是它在 Rust 侧为 LLVM C API 提供更强类型的安全封装，支持本项目需要的 LLVM 版本，并且与 workspace 的 `unsafe_code = forbid` 边界相容。binding 仍是 pre-1.0 依赖，因此版本必须锁入 `Cargo.lock`，升级 LLVM/Inkwell 时运行完整 native golden。

## 6. compiler-private ABI

第一版 native ABI 优先语义完整与 generic sharing，不承诺公开稳定布局。

### 普通值

当前 C1 lowering 默认把 generic aggregate、`dyn`、coroutine、外部入口和尚未 specialize 的跨调用 MIR value 放入统一 tag/payload representation。这里称为当前 universal `Value` lowering，而不是对旧布局兼容性的承诺。tag 只帮助现有通用 clone/equality/trace/diagnostic helper 分派，不是语言 RTTI、reflection、stable ABI 或普通值的永久成本。record/enum/refined value 的 payload 由 compiler-managed nodes 表示；copy 做逻辑深拷贝，内部 transfer 可以转移当前表示。上节所述 primitive scalar 私有 ABI、POD result/`mut self` InOut、record scalar projection、局部 POD SROA 和局部 `List[Int]` 快路已绕过对应 envelope/node 成本，但 aggregate main/test/export、普通 POD 参数、readonly/contract callable、未知 generic、async frame 和动态分派仍保留 universal 表示。POD record 的私有栈节点不会直接逃逸：eligible private result/InOut call 直接 pack/unpack first-class aggregate，whole-value copy 及其余 universal boundary 才从当前字段建立独立 managed chain。完整 universal POD body 因而仍负责当前 fallback 的值复制和 moving-GC 语义，它不是历史 ABI 兼容层；不可达时 release global DCE 会删除它。`Text` 的对象侧表示已经闭合：envelope 只含 tag 与单个 `TextObject*`，长度和 inline UTF-8 位于带 versioned layout descriptor 的对象中，动态对象由 GC 整块移动，字面量使用同布局的 immortal global。该布局只能被 codegen/runtime helpers 观察。

`List` 有两条明确分开的 lowering。默认路径继续使用 current universal `Value`/managed `ValueNode` 表示，并保持既有逻辑复制、值相等和 GC tracing 语义。closed-world use scan 只有在 callable 同步、local 精确为 `List[Int]`、恰有一次空初始化，且所有使用都能证明不复制、不逃逸、不跨 `.await`/generic/witness/defer 边界时，才选择 compiler-private `{data, len, cap}` storage；当前允许的观察形状是局部 `add`、`length` 和直接穷尽匹配 `get` 的 `Option[Int]`。canonical 单 append range 在进入循环前读取 data/len/cap，并用 loop SSA/phi 携带三者；非增长路径不重载 header，reserve 成功后只重载可能改变的 data/cap，元素槽 store 到新 len 提交之间没有 fault point。因此下一迭代的 element 求值 fault、正常退出和 drop 总能看到一致的内存 header。容量不足时仍调用几何扩容 helper，不做隐式预 reserve。

对更窄的 exact-length 形状，分析还要求从空 list 开始、零起点 build range 每个正常迭代恰好 append 一次、range end 是同一常量或 immutable local，且其后零起点 scan 使用同一 end、唯一 induction binding、direct/Unit 形状的穷尽 `Option[Int]` match，中间和 scan body 都不修改 list。此时每个到达的 `get(induction)` 必有 `0 <= induction < len`，LLVM 可直接生成 `Some` 路径，删除逐元素 negative/past-end 与不可达 `None` edge；end、binding、direct/Unit 形状或同一 list 不变性中任一无法证明时都保留完整 checked get。`Some`/`None` arm 本身可以包含可观察行为；只有在 `None` 已被前述条件证明不可达时才删除该 edge。独立的 checked checksum、fault 和 cleanup 不因该证明删除。该 storage 只含 `i64` 元素、独立于 moving heap，不需要 GC root 或 Executor；add 为摊销 O(1)，get/length 为 O(1)，正常或 fault 退出都显式释放。它不是 public/generic List ABI。

最终 typed lowering 以静态类型和 layout descriptor 为依据：concrete scalar、`Text`、record 与已知 generic instance 不需要 per-value tag；enum 只保留自身 variant discriminant；`dyn C` 携带已选 witness/layout proof，但不增加 universal type id。GC trace metadata 可以位于公共 allocation header 或静态 descriptor，它仍不是源码可观察的类型标签。`TextObject` 已闭合对象侧表示，直接 typed local/call ABI 仍须在 generic layout argument、container element layout、coroutine slot layout 与 `dyn` payload layout 完成后移除外围 envelope。`NativeLayout` catalog 已描述 scalar 和无 invariant、单态、直接 primitive-field 的 POD record；record 栈存储与 callable selector 复用该 shape-only 分类。`NativeSignatureShape` 现在放行四种 primitive scalar callable，以及 POD result/`mut self` InOut 首阶段；emitter 再把 native-body closed-world requirement 绑定为 effect-bearing `NativeSignature`，并让声明、调用与 root gate 共用该结果。POD result 使用 compiler-internal first-class LLVM aggregate，由目标后端决定寄存器或机器返回槽，不手工冻结 public `sret`/`byval`；InOut receiver 使用只在该次调用存活的 private pointer。只有 selector、独立参数/结果 storage、native/universal requirement 与 GC/逃逸边界同时闭合时才启用，因此“分类为 POD”本身仍不能删除 allocation。普通 POD 参数、readonly receiver、带 invariant/requires/ensures 的 callable，以及 List/其他 managed 布局、统一 clone/trace/drop plan 与完整 machine-instance graph 仍待扩展。该优化不改变 checked MIR 或 cache 中的语言语义 identity。

### 函数

当前 universal `Value` body 使用五个 pointer operand 的统一调用形状：

```text
status fn(
    out_value,
    argument_nodes,
    conformance_proofs,
    requirement_proofs,
    context,
)
```

两个 proof operand 都是定长、稠密、按签名顺序索引的 pointer array：前者是 conformance 自身的 conditional prerequisite prefix，后者是该次 requirement/call 新增的 proof suffix；空 array 使用 null pointer。正常返回写入 `out_value` 并返回 0；ContractFault/RuntimeFault 走非零 status。需要 context 时，同步路径传入 `LoomRuntime`，async resume 路径传入 `LoomExecutor`；runtime 只通过 versioned context ABI 解析它。业务 `Result.Err` 仍是普通返回值，绝不与 status 混淆。该调用形状只是当前 lowering，不承诺作为 public native library ABI。

eligible native callable 按 requirement summary 使用两种私有调用形状：

```text
R native_fn(native arguments...)                       // pure + no fault
{status, R} native_fn(native arguments..., context)    // may fault/collect
```

`R` 可以是 `i1`/`i64`/`double` 或当前 first-class POD aggregate；native arguments 默认是 primitive scalar，首个 mutable POD receiver 可以是 InOut pointer。pure body 及其直接/递归调用没有 status、Runtime、Executor 或其他隐藏 pointer；checked arithmetic 或其他不能证明安全的 body 保留 status，并把 fault/collect 路由到调用方 context。primitive scalar 的 universal wrapper 负责入口拆箱、调用私有 body 和结果装箱；POD 则保留一份完整 universal fallback body，因为普通 aggregate 边界仍需要独立 managed result 和 moving-GC root。普通 POD 参数、readonly receiver 与合同/invariant callable 尚未套用 typed ABI；其中合同路径应采用一次 checked entry 后进入 assumed private body，而不是在热 body 重复 universal 检查。

### Runtime requirements 与 root context

LLVM codegen 在 root/witness reachability 之后，为可达闭世界建立 compiler-private requirement 图。三个独立 bit 是 `FAULT`（实现名 `MAY_FAULT`）、`COLLECT`（`MAY_COLLECT`）和 `EXECUTOR`（`NEEDS_EXECUTOR`）；它们描述当前 lowering 所需设施，不是源码 effect system，也不进入 concept 或公共函数类型。`COLLECT` 只表示可能进入 moving-GC collection boundary；普通 native allocation、只读 runtime 调用和私有 POD/List storage 不会误设该 bit。scanner 记录 local operation、builtin、direct/static/dynamic witness call edge，再以固定点传播 callee 的对应 invocation/native summary。每个 callable 分开保存 invocation、universal body 和 native body summary：POD private result 可以不含 materialization 的 `COLLECT`，同一源码的 universal fallback 仍保留 build/root requirement。async invocation 是建立 Task 的需求，deferred resume body 另行统计，避免把 resume 工作错误地当作同步构造器执行。

root context 由 invocation summary 与实际 ABI 共同决定：

- 只有 eligible pure/no-fault primitive-scalar root 可以完全不创建 context；
- 其余同步 root 只创建、激活一个 `LoomRuntime`，不创建 Executor；
- async/`EXECUTOR` root 先创建并激活 `LoomRuntime`，再把一个 `LoomExecutor` 附加到该 Runtime。

`LoomRuntime` 始终拥有 `LoomHeap`、同步 shadow-stack 链和 collector 状态；Executor 只借用该稳定 Runtime，并拥有 task/join/ready-queue 调度状态。compiler 的 root bitmap、layout/trace metadata 和 runtime helper 的临时 root scope 不挂在 Executor 上；collection 只把 Executor 所拥有的 Task 集合作为一个 runtime root source 遍历。OS reactor 在首次 wait registration 时初始化，blocking file/socket worker mailbox 在首次需要 worker 时初始化，因此 async root 本身不等于预先创建 kqueue/epoll 或 worker。root 先消费可能引用 heap 的结果，再按 Executor destroy → Runtime deactivate → Runtime destroy 的顺序清理；Runtime activation 或 Executor attachment 失败也走有界清理路径。Task fault record 实行 first-fault-wins：第一次成功记录的 code/message/detail 是 primary fault，后续 fault record 返回成功以让 unwind cleanup 继续，但不会覆盖该记录；当前 ABI 不向源码暴露 suppressed fault 列表。

当前 native runtime ABI 总版本是 v7，standard-library ABI 是 v4，coroutine/task ABI 是 v2，witness ABI 是 v1；精确 identity 为 `loom-value-v2/layout-v1/text-v1/wait-v1/task-v2/runtime-v1/gc-v7/shadow-stack-v1/witness-v1/int-list-v1/stdlib-v4`。`runtime-v1` 保留现有 Runtime lifecycle symbol names，不代表总 ABI 停在 v1。当前 codegen 使用 Runtime lifecycle/attach-Executor/context-fault 入口；普通值复制与 aggregate node 构造分别调用 Rust runtime 的 `loom_gc_clone_value_v1` 和 `loom_gc_build_value_nodes_v1`，owned proof 则由 `loom_gc_clone_witness_v1`、`loom_task_capture_witnesses_v1` 与 `loom_task_witness_v1` 管理。bundle identity 必须精确匹配，不提供旧总 ABI 或旧 stdlib ABI 的兼容入口。

### 接口

`dyn C` 在 checked MIR 中表示 concrete data 与已选 conformance proof 一起流动，不规定 LLVM 类型或内存布局。它是可返回、可存储、可嵌套的一等值；普通 copy 深复制 logical data，proof 可安全共享。静态可知调用直接落到 implementation；后端也可以把 data/proof 拆成 SSA 参数或完全消除接口值。当前 C1 只在间接调用幸存时使用 GC-managed data/witness 表示，并使用上节分离的 conformance/requirement proof arrays。同步 concrete-to-mutable-interface 参数使用 call-scoped stable proxy/copy-in-copy-out carrier，而不是把 caller 裸地址嵌入 `dyn`；异步参数只捕获 owned copy。

witness table 不是语言必须存在的对象。当前 64-bit compiler-private 布局是 `WitnessDescriptor { prerequisite_count: u64, method_count: u64, methods: pointer }`（size 24, align 8）与 `WitnessInstance { descriptor: pointer, prerequisites: pointer }`（size 16, align 8）。descriptor 和 concept-local method array 是 immutable globals；每个 concept 对本产物中可达的 method requirements 编排自己的稠密 slots。instance 的 prerequisites 是有序连续 proof-pointer array，conditional proof 保留 DAG 共享；该表示不包含运行时 type/concept id 或持久 source-address cache。root/witness reachability 在表示选择之前完成，因此 DCE 不依赖 fat-pointer、table prefix、运行时 type id 或 registry。

后端优化优先级是：去虚化/内联 → 调用签名特化 → 分离 SSA data/proof → 必要时物化 pair。单指针表示不作为 KPI；目标 ABI 或实测结果确有收益时，可以局部选择 box/header/inline/tagged representation，但不得改变显式 conformance、值复制、proof flow、fault 或 DCE 结果。

### managed object 与 GC

已落地的 runtime collector 追踪 `Value`、aggregate `ValueNode` 与 managed `Text`，回收不可达对象、复制存活对象并重写已登记 roots。每个 managed allocator 都先经过统一 slowpath；当 projected allocation charge 达到阈值时，collection 在实际分配前发生，因此 caller 必须在 allocator/call safepoint 前发布稳定 root。Rust runtime helper 用 `RuntimeRootScope` 保存可由 collector 重写的输入、部分结果和 scratch slot，用 `NodeStream` 增量构造 aggregate chain，并在每次可能移动的 allocation 返回后重新读取 root，不能跨 allocation 保存 heap-derived pointer 或 Rust borrow。stdlib v4 的 `List.get`、`TextMap.get`、process environment 与 float formatting runtime 边界都写入 caller 提供的完整、地址稳定 `Value` output slot，不再返回下一次 GC 即可能失效的 raw managed pointer/object。

普通 `Value` 深复制和 aggregate node 构造完全位于 Rust runtime：`loom_gc_clone_value_v1` 使用显式、非递归 work stack，`loom_gc_build_value_nodes_v1` 接收稳定 source-slot pointer array 和稳定 output slot。LLVM module 不再生成递归 clone/build helper；生成代码先把 source/output proxy 纳入当前 precise root state，再调用 runtime ABI。owned `dyn` 的非全局 proof 使用独立的非移动 mark-sweep proof arena：`loom_gc_clone_witness_v1` 以单次事务 worklist 深拷贝 prerequisite DAG，GC 从 live `dyn` 的 witness 字段追踪并 sweep proof instances。proof allocation 进入 GC live/reclaimed/threshold 统计，但 proof 地址不 relocation。native local `List[Int]` storage 只含非 managed `i64` 并由生成代码显式释放，不进入 trace 集合。

`shadow-stack-v1` 的同步 moving-GC 路径已经闭合。LLVM 为可能含 managed 值的同步函数发射地址稳定的 root slots、descriptor/frame、push/pop 和按 collection boundary 编号的 live bitmap；参数、`old`、output、词法 local、projection/`dyn` proxy 和临时值只在已初始化且实际存活的区间置位，move 会清空并停用 source root。真实可能 collection 的 callee、clone/build、builtin 与直接 managed allocator 都先发布对应 state，collector 可以原地重写 slot。当前 collector 只由 collect-before allocation slowpath 驱动，没有并发或外部 request flag，因此生成代码不在普通 loop backedge 或 return 插入 synthetic poll；POD 私有栈节点、native `List[Int]` storage 和 scalar 临时也不进入 shadow roots。纯 primitive-scalar 路径不会为“零 managed root”创建空 frame。

### Task 与 coroutine

`Task[T]` 是单个 managed pointer。每个 reachable async constructor 形成对 compiler-generated `resume`、`cancel`、`trace` 和 frame/result descriptor 的闭合边；descriptor 类似静态 witness table，但不是用户 concept、fat-pointer 第二字段或 runtime registry。coroutine descriptor v2 另有 `witness_count`；constructor 通过 `loom_task_capture_witnesses_v1` 一次性验证并深拷贝所有隐藏 proof roots 到 Task 私有的非移动 arena，`resume`/`cancel` 通过 `loom_task_witness_v1` 按稠密 index 访问独立 proof slots。proof slots 与 universal `Value` frame slots 独立，源栈 instance 只需存活到 capture 返回。

Loom MIR 先把 async body降低成 numbered suspension/cancellation states，再由 LLVM 发射普通 control flow。lowering 对结构化 control flow 建立显式 CFG，并以 use/def 的 backward least fixed point 计算每个 suspension 的精确、稳定排序 live-local set；CFG 同时覆盖正常 continuation、if/match/block、loop backedge、return/cancellation 和 active `defer` 的 LIFO cleanup edge。仅用于构造当前 task operand 的 local 不跨 suspension 保留，resume 或取消 cleanup 会读取的 local 必须保留；MIR validator 独立重算并拒绝陈旧、缺失或 all-locals fallback metadata。后端不直接采用 Rust `Future`/`Pin` 表面，也不把 C++ `promise_type` customization 暴露给语言。join node 与 ready queue 属于附加到 root Runtime 的 Executor，wait registration 属于其惰性 reactor；child completion 只能 enqueue waiter，不能在 runtime callback 栈上直接重入 continuation。

当前 native runtime 已落地 version 1 的平台无关 `WaitSource`/`Registration`/`ReadyNotification` C ABI，macOS backend 为 kqueue，Linux backend 为 epoll；timer 用同一 poll wait 的 monotonic deadline timeout，不依赖第二套 callback runtime。registration 是 generation-checked one-shot handle；fd 明确登记 readable/writable，child completion 使用独立 notification source。LLVM 的每个 async run root、以及 test harness 中的每个 async test root，各自建立一组 Runtime + Executor；async constructor 返回单指针 Task，`resume` 在 wait/join 未完成时返回 `Pending`，notification 只把 Task 加回 ready queue。numbered state dispatch覆盖线性 chain 与 if/match/block 内的 await；取消按挂起 state 进入对应 cleanup unwind。`Task.sleep`、`Task.waitReadable`、`Task.waitWritable`、可存储 Task、tuple/list join 和四种 join mode 均走该路径。

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
loomc --release build [--target NAME | --entry main] [--output target/loom/program] PATH
loomc build --target-triple aarch64-unknown-linux-gnu --emit object --output target/loom/program.o PATH
loomc test [--target NAME] PATH
loomc run [--target NAME | --entry main] PATH
loomc run --artifact target/loom/program
loomc run PATH -- arg1 arg2
loomc run --artifact target/loom/program -- arg1 arg2
```

`build` 默认产生按当前宿主 CPU/features 调优的 native executable；`run PATH` 在临时目录执行相同编译流程；`test` 生成 native test harness。任何显式 target triple（即使等于宿主）都以 generic CPU/empty features 闭环到真实 LLVM relocatable object。宿主 executable 默认使用编译器内嵌 Rust runtime 与宿主 linker；交叉 executable 必须成对提供 `--runtime-bundle DIR --linker PROGRAM`。runtime archive 的嵌套 Rust 构建清除继承 tuning 并固定 `target-cpu=generic`；v2 bundle manifest 必须与同一个规范化 triple/data layout/runtime ABI 精确匹配，并声明 `runtime_cpu = "generic"`、空 `runtime_cpu_features`。archive SHA-256 每次链接前后复核；缺少 bundle/linker 稳定报告 `CrossLinkUnavailable`，不把宿主 archive 伪装成目标 runtime。portable `.loomlib` 不接受 release/target-triple/object 选项。默认 native object 不承诺跨不同 CPU 可运行，当前 executable bytes 也不承诺 reproducible，因为系统 linker 可能加入平台 metadata；前端/MIR/cache identity 必须 deterministic。

`loomc runtime export --output DIR` 把当前宿主的内嵌 runtime 导出为只含 manifest 与 `libloom_runtime.a` 的原子目录。目标 runtime bundle 应由运行在该目标平台的相同版本 Loom 工具导出，再与具备该目标能力的显式 linker 配对：

```sh
loomc runtime export --output target/loom/runtime
loomc --target-triple aarch64-unknown-linux-gnu \
  --runtime-bundle /opt/loom/aarch64-linux-runtime \
  --linker aarch64-linux-gnu-clang \
  build --output target/loom/program PATH
```

bundle loader 拒绝未知 manifest 字段、额外目录/文件、symlink、路径穿越、oversize 内容和不安全 linker 参数；显式 linker 必须解析为 bounded executable regular file，并提供成功且有界的 `--version` identity。Loom 固定参数顺序为 target object、runtime archive、manifest link args、`-o OUTPUT`。

`--` 后的参数原样进入 `standard.process.arguments()`，但 executable path 不进入该 list。source run、native artifact 和 interpreted artifact 使用同一规则；环境变量由 child process/当前 interpreter host environment 提供，保持 `environment(Text) Option[Text]` 的相同 Unicode 边界。

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
- target：root package 上的 bin/test/lib root policy 和 artifact kind；
- crate 不是 Loom 源码关键字；Rust workspace crate 只属于编译器实现。

### manifest v1

当前实现同时闭环 path、文件系统 registry 与 HTTPS registry 依赖：

```toml
schema = 1
language = "0.3"

[package]
name = "application"
version = "0.1.0"
sources = ["src"] # 默认值

[dependencies]
utility = { path = "../utility", version = "^1" }
codec = { registry = "local", version = "^2", optional = true }

[registries]
local = "../registry"
remote = { url = "https://registry.example", token-env = "LOOM_REGISTRY_TOKEN" }

[features]
default = []
binary-codec = ["dep:codec"]

[[target]]
name = "app"
kind = "bin"
entry = "application.start" # 默认 main

[[target]]
name = "unit"
kind = "test"

[[target]]
name = "api"
kind = "lib" # 无 entry，产出 portable checked-MIR library
```

`language` 固定选择源码语义与静态证明域；省略时为兼容现有 manifest 等价于 `"0.3"`。当前编译器只接受 `0.3`，未知版本以 `UnsupportedLanguageVersion` fail closed。该值进入 resolved `PackageId`、`loom.lock`、module identity、checked-MIR/object/final-artifact cache key 与 `.loomi`/`.loomlib` envelope；扩大可证明检查消除能力必须发布新的 language version，不能静默改变旧包的推断类型。

dependency path 与文件 registry root 相对当前 manifest；文件 registry 布局是 `<root>/<package>/<semver>/loom.toml`。两类 registry 都选择满足 requirement 的最高版本。已有 `loom.lock` pin 优先，`loomc resolve --update` 忽略旧 pin 后更新；registry package 的 manifest 与被选 `.loom` source 进入 SHA-256，同一已锁版本内容变化会 fail closed。`--locked` 要求当前 feature/package graph 与 lock 完全一致。resolver 还检查 SemVer、feature 引用/循环、依赖循环、重复 package identity、越界 source root 和重复 target。

HTTPS registry protocol v1 使用 `GET /v1/packages/{package}` 取得带 version/SHA-256 的 index，使用 `GET`/`PUT /v1/packages/{package}/versions/{version}` 下载或发布确定性 JSON source bundle。`loomc publish --registry NAME PATH` 只接受 manifest 中具名的网络 registry。token 只从 `token-env` 指定的环境变量读取；认证请求必须使用 HTTPS，重定向被禁用，认证失败的响应正文不进入诊断。为本地协议测试，无认证 HTTP 只接受 literal `127.0.0.0/8` 或 `::1` authority；`localhost`、userinfo、query/fragment 和任何 HTTP token 均在网络请求前拒绝。

index schema、重复版本、SemVer、digest 和响应大小先验证；bundle 再验证 schema、package/version/language 内外 identity、内嵌 `loom.toml`、文件数量/大小、重复或冲突 path、绝对/反斜杠/`.`/`..` 路径及保留 cache 文件。下载以原始 bundle SHA-256 为 immutable identity，原子物化到项目的 `target/loom/registry/http`。每次 cache hit 都重新散列原始 bundle，并逐文件比对物化内容、拒绝额外文件/特殊文件/symlink；sidecar 只记录 identity，不是信任根。`--offline` 禁止 index/package 请求，只在上述完整复核成功时命中，否则返回 `OfflineRegistryMiss`。

feature 仅形成具名闭包并以 `dep:alias` 激活 `optional = true` 依赖；dependency 可用 `features = [...]` 与 `default-features = false` 请求下游 feature。它不裁剪同一 package 内的源码，不增加 `cfg` 表面，不隐式 import，更不激活 contribution/AOP 行为。source label 使用 `src/...` 与 `deps/name@version/...`，因此 package 整体移动不改变 FileId 顺序或缓存 identity。dependency alias 只属于 manifest graph；源码名字空间仍由显式 `module`/`import` 决定。registry 传输只解析 package bytes，不形成 open-world plugin root、运行期实现搜索或 contribution/AOP 激活。

### 为什么需要缓存

单文件 Core 不要求持久缓存，但多 module/package 必须有。没有缓存会在每次改动后重复 parse、type-check、instantiate 和 object emission；root graph 本身不能替代增量依赖追踪。

### 已实现缓存边界

默认 cache root 是 root package 下的 `target/loom/cache/v2`。schema 升级使用新版本目录，旧缓存不会被误读。`--cache-dir DIR` 只改变存储位置，`--no-cache` 强制冷编译；`cache stat` 报告引用、blob、字节与可回收空间，显式 `cache prune` 删除损坏引用和不可达 blob。JSON 模式输出 `cache_result` 的 layer、hit/miss/disabled 和 key。

当前真实缓存五个边界：

1. 每个 UTF-8 source 的 lossless token/AST；命中会直接跳过 lexer/parser，并重新绑定当前稳定 FileId；
2. 每个 module 的 canonical public interface；包含 import、exported declaration、generic bound、concept requirement、associated type、contract、public inherent method 与 conformance header，排除实现 body 和 source range；
3. 整张 package graph 的 validated checked MIR 与稳定 diagnostics；
4. 已选 run/test root 的 LLVM target object；key 只包含全局 type/concept schema、reachable function body、live witness method/proof edge、target/data layout/optimization 与 debug-source policy，不包含不可达私有函数 body；
5. 最终 native executable、macOS dSYM payload、解释器 `.loomi` 或 portable `.loomlib` artifact。

CAS 把 ref 与 SHA-256 blob 分开。读取时验证 schema、namespace、key、size 与完整内容 hash；checked MIR 再经过 versioned `.loomi` decoder 和完整 MIR validator，native artifact 只有在内容验证后才原子 materialize 并设置执行权限。写入为 blob-first、ref-last 的同目录原子替换；并发或损坏只能退化为 miss。最终输出路径不进入 key，因此相同 target 可 materialize 到不同位置。

整图 key 以 length-delimited fields 编码，包含 compiler source + Rust version fingerprint、debug/release profile、MIR/backend/stdlib/runtime ABI、canonical package/dependency/feature/target graph、registry checksum、稳定 source path 与 bytes、target triple/data layout、CPU/features、optimization/relocation 和 contract mode。编译器 fingerprint 只散列 workspace-relative compiler source path/content，不含 checkout 绝对路径。LLVM target identity 来自 emission 共用的同一个 TargetMachine policy，而不是另写一份猜测值；final native artifact 的派生 key 还包含内嵌 runtime identity，或外部 runtime manifest/archive SHA-256 与显式 linker path/executable/version identity，以及 macOS `dsymutil` identity。任一输入变化都会 miss；工具 identity 无法确认时停用 final-artifact cache 而不影响前端检查。

### 增量层与当前边界

```text
source hash
  → lossless token/AST cache                         已实现并复用
  → public interface fingerprint                    已实现并缓存
  → module typed-HIR body semantic query            长驻 host 已选择性复用
  → whole-graph checked MIR                         已跨进程缓存
  → reachable function/witness/proof fingerprint    已实现
  → target object cache                             已实现并复用
  → runtime/linker/debug-tool keyed final link       已实现并复用
```

这里把三种主张严格分开：长驻 `AnalysisHost` 的连续 snapshot 会同时比较 public interface、全声明 semantic shape 和 body fingerprint；声明形状不变时只重查 body 变化的 module，并复用其余 module 的 `BodySemantics`，形状变化则安全回退整图检查。跨进程仍从 validated whole-graph checked MIR 恢复，不序列化 typed-HIR body。不可达私有 body 修改还可复用同一 target object/final artifact，测试会在破坏 final ref 后证明只重新链接。当前 shared-generic ABI 没有独立 monomorphized machine instance；generic/witness proof 参数直接进入 reachable object fingerprint，将来增加单态化时再把 canonical type/proof arguments 拆成 instance CAS entry。

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
6. returned/stored/nested `dyn C` 在两后端运行，copy 后 mutable data 相互隔离；
7. unreachable conformance 不进入 live witness/method graph；
8. LLVM verifier 优化前后通过；
9. Rust float codec 与 interpreter 的 canonical boundary 一致，且生成物不依赖 C++ runtime；
10. interpreter differential tests 保留，但默认命令不使用解释器。

Core 0.3 的新增关门条件：

11. `scoped`/`defer` 在 normal return、fault 和 cancellation 上保持同一 LIFO 顺序；
12. async fixture 的 source run、native artifact run 与 interpreter 一致；
13. Pending task 使用 wait registration/ready queue，不忙轮询；
14. static tuple join 与 dynamic list join 保持 input-order result layout；
15. `all/any/race` 的 sibling cancellation 在返回前 drain cleanup；
16. async scheduler safepoint 能重定位 Task slots/results 引用的 managed objects，同时不改变 Task identity、proof address、合同或 concept behavior；
17. async descriptor 和 join runtime edge进入 root graph，未构造的 task/conformance 仍可被 DCE；
18. manifest path dependency、SemVer、cycle、bin/test target 与稳定 dependency source label 通过 driver/CLI 回归；
19. cache relocation identity 不含绝对路径，内容变化 miss，损坏 blob 安全 miss/修复，第二次 checked-MIR/final-artifact 构建真实 hit。
20. development/release 机器 IR 回归证明常量折叠、内联与不可达函数删除真实发生，不只比较 profile 名称；
21. 三个冻结 Core task 在解释器与 release LLVM main/test oracle 上一致，并满足 [性能、增量与 C2 implementation-controlled 门](09-quality-and-controlled-evidence.md)；
22. lossless syntax/recovery 与 artifact decoder/checked-MIR validator 的 libFuzzer target 在 CI 持续运行；
23. eligible POD result/`mut self` InOut 跨函数调用使用 first-class aggregate/private pointer，status 成功与失败边都先写回 receiver；universal fallback 保留独立 managed chain 与 moving-GC root，release 能 DCE 未引用 fallback，基准 hot loop 经 SROA 后没有 node allocation、clone/build 或 root；
24. native `List[Int]` 的 canonical append SSA、exact-length scan proof、保守 fallback、fault 后 header/drop 一致性与 checked checksum 均有 development/release/native 回归；
25. terminal fault/status edge 的 cold-layout hints 不污染普通业务分支，Task 第二次 fault record 不覆盖 primary fault。
26. concept-local reachable method slots、conditional proof DAG 的 owned `dyn` clone/GC sweep，以及 Task proof capture/access 均有 native/runtime 回归。
27. LLVM synchronous root-frame、safepoint state publication、runtime clone/build ABI、collect-before allocation slowpath 和 stdlib stable-output boundary 已用同步分配/调用/循环、moving relocation、超过 64 个 root、partial aggregate 及 forced-collection fixture 关门。
28. coroutine suspension metadata 必须等于 MIR validator 独立重算的 CFG exact liveness，并覆盖正常 continuation、取消与 `defer` cleanup，不接受 all-locals fallback。
