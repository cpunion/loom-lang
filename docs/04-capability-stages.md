# loom-lang 核心实现分期

状态：Core Delivery Plan / Core 0.1–0.3 Native Loop + Controlled Quality Gates Closed

证据等级：C1 executable reference + C3 implementation-controlled repository evidence

日期：2026-08-25

本文只安排普通编程语言闭环：源码、静态类型、约束/合同、`concept`、多态、自动内存管理、词法资源清理、异步任务、编译产物和工具链。live、AST 编辑、AOP-like 组合、operator runtime 与所有权语法不进入当前排期。

当前 reference implementation 已完成同一条前端到 native artifact 的 C1 纵向切片：`examples/core01` 覆盖静态核心、受约束值、record invariant 与 method contract；`examples/core02` 覆盖 static concept、associated type、接口参数和 readonly/mutable dispatch；`examples/core03` 覆盖 moving GC、scoped/defer、结构化 Task、真实 wait registration、静态/动态 join 与取消。三套冻结源码形成 C2 oracle；`examples/c3` 的 3-package/24-module workload 再把 package identity、direct dependency、跨模块 root graph 和双后端 main/test 纳入 C3 门禁。

## 1. C0：规范基线

权威分工：

- [项目章程](00-charter.md)定义产品范围；
- [最小语言核心规范](02-language-design-baseline.md)定义 Core 0.1 语义；
- [核心表面与代码风格](03-surface-and-style.md)定义惯用写法；
- [concept 与动态多态规范](05-concepts-and-dynamic-polymorphism.md)定义 Core 0.2；
- [可执行合同](06-executable-contract.md)固定 parser、数值、failure 与工具边界；
- [编译过程与后端定案](07-compiler-pipeline-and-backends.md)固定 root graph、LLVM、artifact 与缓存模型。
- [GC、词法清理与异步任务定案](08-memory-cleanup-and-async.md)固定 Core 0.3 的 GC、scoped/defer、Task/coroutine 与 join。

实现可以替换内部算法，但不得产生第二套类型、合同、派发或失败语义。

## 2. C1a：静态语言骨架

范围：

- UTF-8 source、lexer/parser 与 error-island 恢复；
- module/import/visibility；
- record、enum、Option、Result；
- fn、method、let/var、if、block、return、match 与显式 `discard`；
- rank-1 泛型与定义处检查；
- 普通 `test fn`；
- CLI 与 LSP 共享 source snapshot 和 analysis。

关门条件包括：全部 declaration/body 都要检查；module cycle、重复/不可见名字稳定诊断；封闭类型 match 必须穷尽；非 `Unit` 结果必须进入使用位置或显式 `discard`，未知泛型和资源/Task obligation 不得借此丢失；文件遍历与声明顺序不能改变结果；测试返回 Unit/Ok、Err 与 fault 必须可区分。

## 3. C1b：受约束数据

范围：

- `type T = Base where predicate`；
- proof-classified construction：已证明 `T(expr) -> T`，已否证为静态诊断，unknown 才是 `T(expr) -> Result[T, ConstraintError]`；
- 带 invariant record 的 checked literal；
- 结构化 `ConstraintError`；
- 约束值读取、运算后重新建立，以及常量、局部/tuple、nominal return、requires/assert/invariant 与 proof-pure 分支事实驱动的安全检查消除；赋值、inout、join 和零轮循环路径执行保守失效。

关门条件包括：任何输入边界都不能伪造已建立的约束值；debug/release 的接受结果一致；NaN、无穷、负零和边界值有固定语义；业务拒绝始终留在普通 `Result` 轨道。

## 4. C1c：method、mutation 与合同

范围：

- inherent method；
- 默认深只读 `self`；
- `mut self` 对 caller `var` place 的 call-scoped inout；
- invariant、requires、ensures、assert；
- `result`、`old(expr)` 与深值快照；
- 结构化 `ContractFault` 和 `RuntimeFault`。

关门条件包括：只读 receiver 不可写；mutable receiver 只接受 `var` place；正常返回写回同一 place；invariant/ensures 顺序固定；合同不可在 release 中整体关闭；checked `Int` 永远不得 wrap 或触发 LLVM undefined behavior。

## 5. C1d：原生工具链

默认路径是：

```text
source → parser → HIR → sema → checked MIR
       → root/witness reachability → LLVM IR → object → native executable
```

交付内容：

- `loomc check/build/test/run/debug/fmt`；
- LLVM 19 + Inkwell 后端；
- 优化前后 LLVM verifier；
- 平台 object 与 linker 产物；
- root-scoped Float parse/format native runtime；
- native test harness；
- formatter、JSON diagnostics，以及带 definition/references/hover、语义 completion、document/workspace symbols 和全局或 callable-local rename 的 LSP；
- 仅通过 `--backend interpreter` 显式选择的语义对照后端。

`build` 不再以解释程序镜像冒充编译产物。默认 artifact 必须能被操作系统直接识别并执行。

## 6. C1e：static concept

范围：

- 唯一行为抽象 `concept`；
- 显式 conformance、owner-orphan coherence、唯一性；
- `T: C` 定义处检查；
- associated type 与绑定；
- conditional conformance、overlap 与 proof-cycle 拒绝；
- static concept call 和 concept contract。

关门条件包括：泛型 body 只能使用签名提供的 proof；import/link/file 顺序不改变 conformance；同名 inherent/concept method 不靠猜测；conditional witness 能真实进入 native call chain。

## 7. C1f：接口参数与 `dyn C`

惯用参数写作 `value C`；`value dyn C` 只显式强调擦除，参数位置二者具有相同可观察语义。具体值自动适配。源码不书写 `view[...]`、borrow、lifetime 或 owning carrier。

需要擦除时，后端必须让 concrete data 与已选 witness 一起流动，但不固定内存布局。它可以直接调用、把 data/proof 拆成 SSA 参数，或在间接派发仍存在时使用当前 C1 的 compiler-private data/witness pair；这些表示不得被源码观察。

范围：

- `dyn concept` 定义处 compatibility checker；
- 完整 associated binding；
- concrete-to-interface coercion；
- `dyn C` 的返回、local、record/enum、tuple/list 与泛型嵌套；
- owned interface copy 的值隔离；
- witness dispatch 和可去虚化的静态路径；
- `mut self` 接口调用对 `var` place 的 call-scoped inout；
- concrete invariant 与 concept contract 的统一顺序。

关门条件包括：非 receiver `Self`、static/generic requirement 不能进入 erased ABI；未绑定 associated type 必须拒绝；mutable 接口 receiver 不是 `var` place 时稳定诊断；一等接口值可存储/返回且 copy 后互不别名；只有 compiler-private 同步调用写回载体不得逃逸、嵌套或跨 await。

## 8. root graph 与 DCE

`build` 的 bin target 从选定 entry 遍历；`test` 从全部 `test fn` 遍历；lib target 保存 validated checked-MIR 与 public export map，不承诺 native/open-world ABI。类型检查仍覆盖项目中的全部声明，不能因不可达而跳过错误。

可达分析至少包含：

- direct/inherent calls；
- generic instance 与 witness proof；
- static concept implementation；
- concrete-to-interface 建立的 live witness table；
- 每个动态调用实际使用的 requirement slot；
- compiler/runtime builtin symbol。

仅声明 `impl C for T` 不构成 root。当前不提供 universal `any`，也禁止 `A -> any -> dyn C` 的运行时 conformance 搜索；否则必须把潜在实现注册表整体视作 roots，会破坏 closed-world DCE。

## 9. C1g：词法资源清理

范围：

- `scoped name Type? = expression`；
- `defer { ... }`；
- compiler-known `Dispose`、`MustScope`、`NoSuspend`；
- 每个 block/if/match arm/loop body 独立的 LIFO cleanup stack；
- normal return、fault 和取消路径的一致 unwind；
- scoped stable binding、不可复制/逃逸和手动 double-dispose 拒绝。

解释器和 LLVM 必须执行相同 cleanup 顺序。`defer` 不是 Go 的函数级 defer；一般 async cleanup/destructor 不属于本门。

## 10. C1h：GC runtime

范围：

- compiler-known safepoint 与精确 roots；
- 可移动对象的 layout/trace metadata；
- coroutine frame、join slots 和 scheduler handles 的追踪；
- OOM 进程级 RuntimeFault；
- 无 finalizer、weak reference 或地址观察；
- native runtime 与 interpreter differential tests。

早期 non-moving 或 task-frame-only collector 只能作为实现台阶；关门要求 moving collector 不改变值、合同、concept 或 Task identity。

## 11. C1i：stackless async 与 Task

范围：

- `async fn`、后缀关键字 `.await`、`Task[T]`；
- `Duration` 以及通过同一等待注册 ABI 执行的最小 File/TCP Socket 文本 I/O；
- MIR suspension state、live-local frame promotion 和 cancellation state；
- ready queue、wait registration，以及“通知 push、执行 pull”；
- parent/child structured concurrency，无 detached task；
- compiler-private affine TaskCarrier flow，覆盖 await/join、同步显式 carrier 参数/返回转移、结构化 binding/match、分支合流与 scope-exit 审计；源码不增加 move/borrow/ownership；
- 未约束泛型不能承接 TaskCarrier，Task/MustScope obligation 也不能经 `dyn` 擦除；当前无 reparent ABI，TaskCarrier 传入 async callable 与 async 逻辑返回递归包含 Task 必须 fail closed；
- active inout/interface loan 与 `NoSuspend` 跨 await 拒绝；
- compiler-private single-pointer Task/coroutine descriptor ABI；
- interpreter 与 LLVM native 闭环。

Pending task 不得忙轮询；事件唤醒只入队，不直接重入 continuation。

## 12. C1j：Task join

范围：

- `Task.all(taskA, taskB).await` 的异构 tuple 结果；
- `Task.all(List[Task[T]])` 的动态 list 结果；
- `settled`、`any`、`race`；
- static/dynamic `JoinState`、取消 sibling 并 drain cleanup；
- input-order result slots、空集合合同和显式 tagged heterogeneous choice；
- 不经过 universal `any` 的 closed-world reachability/DCE。

## 13. C1k：module/package/cache

多文件 module、schema-versioned `loom.toml`、path/文件系统/HTTPS registry dependency、认证发布、可信离线 cache、SemVer requirement、optional-dependency feature、SHA-256 `loom.lock`、bin/test/lib target 与 `--target` 已接入 driver/CLI；lib 产物是 portable validated checked-MIR，不是稳定 native ABI。`resolve --update` 显式更新 pin，`--locked` 保证图不漂移，`--offline` 只使用重新验证过的 immutable bundle；feature 不增加源码 `cfg` 或运行时注册。无 manifest 的历史目录和单文件仍可编译；`crate` 不成为 Loom 关键字。

持久缓存已经落地逐 source lossless token/AST、module public-interface、整张 package graph checked MIR（连同稳定 diagnostics）、closed-world reachable LLVM object，以及解释器 `.loomi`/portable `.loomlib` artifact。checked-MIR v3 key 只包含 language/frontend/stdlib/contract、canonical package/dependency/feature semantic graph 与稳定相对源码内容；frontend build identity 的生产源码只来自 core/syntax/HIR/sema/MIR/lowering/driver，不散列 backend/runtime/interpreter/CLI 源码，并且排除 Cargo build profile 和 manifest target declaration，因此可跨 interpreter/LLVM、development/release 和目标策略复用。当前 workspace manifest/lock 仍作为两层共享的保守输入：无关依赖升级可能多产生 miss，但不会产生错误 hit。LLVM object v5 key 的 native fingerprint 再加入 codegen/MIR/runtime-ABI build identity、llvm-sys 构建时选中的 libLLVM 版本与二进制内容、运行时读到的 LLVM 数值版本、reachable function/witness/proof，以及真实 target/data-layout/CPU/optimization policy。native executable/dSYM 因宿主 clang/ld、SDK/sysroot、CRT 和系统库还不是 hermetic bundle，明确禁用 final cache 并每次从 object 重链；不用版本字符串伪装精确工具身份。mtime、绝对 checkout 路径、文件遍历顺序和编辑器状态不参与已启用 cache identity；读取时验证 ref、size、SHA-256，parse 重建源码，checked MIR 重新通过 artifact decoder/MIR validator。损坏只产生 miss，写入采用同目录原子替换。

当前增量分层状态：

1. source/token/AST：已真实复用；
2. public interface/semantic shape/body fingerprint：已接入长驻 host 的 typed-HIR selective body query；声明形状变化安全回退整图；
3. checked MIR：整图缓存；reachable function body 进入 object fingerprint；
4. generic/witness：当前 shared generic body，proof/witness edge 进入 reachability fingerprint；未来单态化才新增独立 instance entry；
5. development/release profile 与显式 target triple/data layout 下的 object：已真实复用；非宿主 triple 可真实发出 relocatable object；跨目标 executable 只有在显式 runtime bundle、linker、triple、data layout、generic/empty-feature runtime CPU policy、ABI 和 archive digest 全部匹配时才链接，否则 fail closed；
6. native final link：明确不缓存，每次从 object 重链；等待包含子工具、SDK/sysroot、CRT、system library 和 dSYM 配对的 hermetic link bundle。

当前明确宣称两层复用：同一长驻 host 的无关 module 不重查 body semantics；跨进程则恢复 validated whole-graph checked MIR，不序列化 typed-HIR body。不可达 private body 修改还可继续复用 object，native final 只做快速重链。

## 14. C1l：普通程序标准库与发布闭环

范围：

- 不可变、有效 Unicode scalar sequence 的 `Text`，以及显式分离的任意 `Bytes`；
- portable lexical `Path`、不可变 `TextMap[V]`、有界 canonical JSON；
- typed async file/TCP socket I/O、稳定 `IoErrorKind` 与 canonical JSON-line logging；
- 结构性 `MustScope`，包括 wrapper/pattern/argument 边界与 Task 暂存后的直接 scoped 解包；
- HTTPS registry resolve/publish、credential transport、bundle/cache 全量复核；
- `loomc runtime export`、目标 runtime bundle、显式 linker 与发布 archive。

`Text` 保持规范名称，不增加 `String`/`str` alias。当前已把旧的 `{tag, byte-length, byte-pointer}` payload 原子迁移为 `{tag, TextObject*}` 兼容 envelope；对象本身是带 versioned layout descriptor、缓存 scalar length 与 trailing UTF-8 的单个 managed allocation。下一步 typed lowering 再从已知 concrete `Text` 位置移除外围 tag；在此之前 generic、container、coroutine 与 `dyn` 边界继续使用 uniform envelope。release workflow 必须真实导出并重新链接宿主 runtime bundle，标准库 fixture 必须在解释器/native 两条路径产生相同值、文件、日志和错误结果。

## 15. 明确后置

- universal `any`、reflection、type registry、dyn downcast/upcast/intersection；
- stable plugin/dynamic-library/FFI witness ABI；
- 多组整数大小的普通算术表面；精确宽度整数只在真实低层需求出现后加入；
- 所有权、借用、lifetime、move-only/owned/shared 接口载体；
- 多线程共享内存 executor、持久化 coroutine、分布式执行和一般 effect/capability/provider；
- AOP-like 静态组合、live/AST 编辑、desired-state/operator runtime；
- operator overloading、自研 machine backend。

这些方向不得反向阻塞或修改当前普通语言的 `check/build/test/run` 闭环。
