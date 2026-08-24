# loom-lang GC、词法清理与异步任务定案

状态：Core 0.3 Confirmed Normative Design / C1 Native Loop Closed

日期：2026-08-24

本文固定自动内存管理、`scoped`/`defer`、stackless coroutine、`Task[T]` 与任务组合语义。它扩展 Core 0.1–0.2，但不引入 Rust 风格 ownership/borrow/lifetime 表面，也不引入 live、AST 编辑、AOP-like 组合或 operator runtime。

本文中的“必须”“不得”是规范要求。最后一节单独列出 reference implementation 当前已经闭合和仍在开发的部分；实现进度不能反向改变语义。

## 1. 总体边界

Core 0.3 增加：

- 自动 tracing GC；
- 块级 `scoped` 与 `defer`；
- compiler-known `Dispose`、`MustScope`、`NoSuspend` marker concepts；
- `async fn`、显式 `await`、`Task[T]`；
- Loom MIR 自己降低的 stackless coroutine；
- 单线程 cooperative executor 的第一实现；
- 结构化父子任务、取消和 join；
- 固定元数的异构 tuple join 与动态同构 list join；
- `Task.all`、`Task.settled`、`Task.any`、`Task.race`。

Core 0.3 不增加：

- 源码级 ownership、borrow、lifetime、`Pin` 或地址稳定性承诺；
- finalizer、GC 析构、weak reference；
- detached task、隐式后台任务或 callback/`then` 链；
- 用户可实现的 `Future`/coroutine runtime trait；
- async concept requirement、async `defer` 或一般 async destructor；
- universal `any`、运行时 conformance 搜索或反射 registry；
- 多线程共享内存执行。多线程可以后续扩展 executor，但不得改变本语义。

## 2. 自动内存管理

### 2.1 可观察语义

GC 是语言运行时的一部分，不是源码协议：

1. 源码不写所有权、借用、引用计数或释放内存；
2. managed object 地址和移动不可观察，源码不得依赖稳定地址、对象地址排序或指针相等；
3. collector 可以移动对象；移动不得改变值相等、record invariant、合同、concept conformance、动态派发或 Task identity；
4. 栈帧、coroutine frame、scheduler roots、暂存结果和 compiler/runtime handles 都属于精确 root 集合；
5. 分配、移动和回收只能在 compiler-known safepoint 上观察 runtime state；
6. GC metadata 必须描述当前 coroutine state 中真正初始化且存活的 managed fields；保守扫描可以作为早期实现，但不是公开 ABI。

GC 不提供用户可观察的回收时机。程序不得通过内存是否已经回收来同步任务或表达业务状态。

### 2.2 外部资源

文件、socket、锁、事务、native handle 等外部资源不由 GC 语义关闭。它们必须通过显式操作、`scoped` 或 `defer` 完成词法清理。

Core 不提供 finalizer 或析构回调：

- collector 回收内存时不得调用用户代码；
- 忘记关闭外部资源是静态 `MustScope` 错误或程序缺陷，不能等待 GC 补救；
- `Dispose` 是词法清理协议，不是 GC finalizer；
- GC 回收已完成的 task/frame 只释放内存，不重复执行其 cleanup。

### 2.3 OOM 与 FFI

OOM 是不可捕获、进程级 `RuntimeFault`。运行时可以在终止前输出安全诊断，但不得把 OOM 包装成业务 `Result.Err`、Task outcome 或可继续执行的异常。

FFI 后置并采用显式边界：

- 优先复制 plain data；
- 必须暴露 managed storage 时使用 compiler/runtime pin boundary；
- pin 只存在于 FFI 边界，不扩散成普通源码的所有权或 `Pin` 类型系统；
- native 代码不得在 boundary 生命周期之外保留可移动对象的裸地址。

## 3. `scoped` 与 `defer`

### 3.1 词法 scope

清理边界是最内层词法块，不是 Go 风格的整个函数：

- 普通 `{ ... }`；
- `if`/`else` block；
- 每个 `match` arm block；
- loop body；
- 函数体。

离开该块的所有路径都执行已经注册的 cleanup：正常落下、尾表达式、`return`、合同或运行时 fault、任务取消。cleanup 严格按注册顺序的逆序执行。

### 3.2 `scoped` binding

表面写法：

```loom
scoped file = openFile(path)
scoped socket Socket = connect(address)
```

规则：

- 不写 `scoped let`；
- binding 本身稳定，不能重新赋值；
- resource 可以通过自己的 `mut self` method 修改内部状态；
- scoped value 不能复制、逃出所在 scope、存入更长寿命 aggregate 或由 closure/task 捕获；
- scoped value 不能作为普通返回值从当前 scope 离开；新创建的 `MustScope` 返回值必须在 caller 处立即进入新的 `scoped` binding；
- compiler 在 binding 成功建立后注册一次静态选择的 `Dispose.dispose(mut self) Unit`；
- 对 scoped value 手动调用同一个 `dispose` 是静态错误，避免双重清理。

`standard.resource` 中的 compiler-known concepts：

```loom
concept Dispose {
    method dispose(mut self) Unit
}

concept MustScope {}
concept NoSuspend {}
```

`Dispose` 必须保持上述唯一 requirement，marker concept 必须为空且不能声明为 `dyn concept`。

- `scoped` 要求静态存在唯一 `Dispose` conformance；
- `MustScope` value 必须使用 `scoped`，不能用普通 `let`、丢弃表达式或仅靠 `defer` 绕过；
- 活跃的 `NoSuspend` value 不能跨越 `await`；
- 这些 concept 不产生运行时实现搜索或全局 registry。

### 3.3 `defer`

```loom
defer {
    closeWithProtocolName(handle)
}
```

`defer` 注册任意同步 cleanup block，因此可以调用名称不是 `dispose` 的释放协议。block 在 scope exit 时执行，读取届时的词法 binding/value；注册本身不提前执行 block 内容。

Core 0.3 的 cleanup block：

- 返回 `Unit`；
- 不得包含 `return`、`await`、新的 `defer` 或新的 `scoped` binding；
- 不得让 scoped value 逃逸；
- 可以调用普通同步函数和 method；
- 与 compiler 生成的 scoped disposal 共用一个 LIFO cleanup stack。

例如：

```loom
scoped first = acquireFirst()
defer { releaseByName(second) }
scoped third = acquireThird()
```

离开 scope 时顺序固定为 `third.dispose()`、`releaseByName(second)`、`first.dispose()`。

若正常路径上的 cleanup 产生 fault，第一个按 LIFO 实际观察到的 cleanup fault 成为主失败，其余 cleanup 仍继续执行。若已经因原始 fault/取消开始 unwind，cleanup fault 不替换原始失败；实现可以把它记录为 suppressed diagnostic，但源码不能捕获它。

## 4. `async fn`、`Task[T]` 与显式暂停

### 4.1 表面与类型

async 函数签名写逻辑结果类型：

```loom
async fn fetch(url Text) Response {
    ...
}
```

不写 `Task[Response]` 作为声明返回类型。类型规则是：

```text
fetch(url)          Task[Response]
fetch(url).await    Response
```

`async` 是函数修饰符，`Task[T]` 是一次异步执行的 typed handle。`Promise[T]` 不作为同义词；未来若需要外部手动完成的一次性结果槽，应单独设计 completion primitive。

调用 async 函数会：

1. 分配 compiler-generated coroutine frame；
2. 建立父子关系；
3. 把新 Task 放入 executor ready queue；
4. 返回 `Task[T]`，绝不在 caller 栈上同步重入 coroutine body。

Task handle 不能 detached。普通单任务调用、组合或作用域转移必须让 compiler/runtime 持续知道其结构化 parent；丢弃仍未终结的 Task 是静态错误。异常退出和取消会递归取消并 drain 未完成子任务。

所有可暂停位置必须显式出现后缀关键字 `.await`。它不是可声明或重载的 method，不带括号；旧前缀 `await task` 不再是合法写法。后缀形式按普通 postfix chain 组合：

```loom
let response = fetch(url).await
let document = fetch(url).await.decode()
let parsed = fetch(url).await?
```

`?` 是独立的普通 `Result` 后缀运算符，不属于 `.await`。当 operand 为 `Result[T, E]` 时，`Ok(value)` 产生 `T`，`Err(error)` 从当前 callable 返回 `Err(error)`；当前 callable 必须返回 `Result[_, E]`，且第一版要求错误类型精确相同，不提供 Rust `From` 一类隐式转换。错误传播视作正常的提前 return，离开的每层 block 都执行已登记的 `defer` 与 `scoped` cleanup。`a.await!` 不作为强制成功语法；需要把 `Err` 转为 fault 时应使用显式 `match` 或未来单独定案的 API。

`Task.all(...)` 只构造组合 Task，不隐式等待：

```loom
let combined = Task.all(taskA, taskB)
let a, b = combined.await
```

compiler-known 外部等待构造器包括 `Task.sleep(milliseconds)`、`Task.waitReadable(fd)` 与 `Task.waitWritable(fd)`，都返回可存储的 `Task[Unit]`。sleep 参数为非负 `Int`；解释器按 deadline 挂起，LLVM 把相对毫秒安全换算为绝对 monotonic nanoseconds 并登记 TIMER `WaitSource`。fd 参数必须适配平台 descriptor 范围；runtime 只借用它直到 readiness/cancel，不取得所有权或负责关闭。固定或动态 join 会先建立全部 Task 再暂停 parent，不把等待串行相加。非法 descriptor、负数、换算/deadline 溢出进入 `RuntimeFault`。标准 `Duration` 与更高层 socket/file API 尚未定案。

### 4.2 coroutine ABI

源码不暴露通用 coroutine trait、C++ `coroutine_handle`/`promise_type` 或 Rust `Future`/`Poll`/`Pin`。编译器与运行时之间使用封闭 ABI：

```text
CoroutineObject                 // managed single-pointer object
├── state
├── status/cancellation
├── live locals and cleanup state
├── result or task-local fault
├── wait registrations
└── CoroutineDescriptor
    ├── resume(frame, context) -> Step
    ├── cancel(frame) -> Step
    ├── trace(frame, visitor)
    └── frame/result layout metadata
```

`CoroutineDescriptor` 类似 compiler-generated concept witness/vtable，但不是用户可实现的 `concept`，不形成 `{data, witness}` 源码 fat pointer，不提供 `dyn Coroutine`，也不加入全局 registry。`Task[T]` 的物理值保持单个 managed pointer；descriptor 可以在对象 header 或 GC type metadata 中取得。

未来 generator、stream 或 actor 可以复用 frame/descriptor ABI，但使用各自高层类型。复用 ABI 不代表 Task 是 consumer pull-based generator。

### 4.3 调度

第一版是单线程 cooperative executor，语义固定为“通知 push、执行 pull”：

```text
I/O、timer 或 child completion
  → wake(task_id)
  → task 进入 ready queue

executor
  → 从 ready queue 取 task
  → resume(frame)
```

`resume` 结果概念上是：

```text
Completed(value)
Pending(wait_key)
Faulted(fault)
```

`Pending` 必须注册真实 wait source；executor 不得反复忙轮询 Pending task。事件通知只入队，不直接调用 continuation，因此没有 callback re-entry。

compiler/runtime-private Wait ABI 固定为 versioned C boundary，源码不可见：

```text
WaitSource
├── Timer(deadline: absolute monotonic ns)
├── Fd(handle, readable | writable)
└── Completion

Registration { key, generation }       // one-shot, stale-safe
ReadyNotification {
    registration,
    frame,
    events,
    os_error
}
```

- `register(source, frame)` 只登记等待，不 resume frame；同一 executor 上重复的 fd interest 被拒绝；
- kernel event 或 `notify_completion(registration)` 使 registration 原子地终结并把 notification 推入 ready queue；
- `wait(timeout)` 只负责收集平台事件，`pop_ready()` 才把 frame 交给 executor 的 pull loop；
- `cancel(registration)` 与 notification 都校验 `{key, generation}`，迟到、重复或已取消通知不能再次 enqueue；
- 第一平台层在 macOS 使用 kqueue，在 Linux 使用 epoll；timer 使用同一 reactor wait 的 monotonic deadline timeout。平台 errno 只作为 runtime fault 细节，不成为业务 `Result`。

### 4.4 结构化并发与取消

- parent 拥有 child task；Core 不提供 detached task；
- parent 正常完成前，所有 child 必须终结或被显式 join；
- parent 取消向所有未完成 child 传播；
- child completion 只唤醒等待者；
- 取消在 `await` 和 compiler-known checkpoint 被观察；
- 取消进入 compiler-generated unwind state，按 LIFO 执行 `defer` 与 `scoped` cleanup；
- join 在输家取消后必须等其 cleanup 完成，不能遗留 zombie task；
- 活跃 call-scoped interface/inout 和 `NoSuspend` value 不得跨 `await`；
- async concept requirement、一般 async destructor 和 async `defer` 后置。

## 5. Task join

### 5.1 固定元数：tuple

固定元数的独立参数可以异构：

```loom
let a, b = Task.all(taskA, taskB).await
```

```text
taskA                              Task[A]
taskB                              Task[B]
Task.all(taskA, taskB)             Task[(A, B)]
Task.all(taskA, taskB).await       (A, B)
```

多 binding 是普通 tuple destructuring，不建立第二套多返回值 ABI。函数也可以返回 `(A, B)`，async 调用对应 `Task[(A, B)]`。

规则：

- task 表达式从左到右求值并全部建立后，parent 才在 join 点暂停；
- 结果按输入位置排列，不按完成顺序排列；
- 全部成功后才同时建立 bindings，不出现部分赋值；
- tuple 保留每个 concrete type，不经过 `any` 或隐式 `dyn`；
- LLVM 可以把 tuple slots scalarize，不要求分配 tuple object。

### 5.2 动态元数：list

运行时数量必须使用同构 task list：

```loom
let tasks = List[Task[Report]]()
for i in 0..workerCount {
    tasks.add(runWorker(i))
}

let reports = Task.all(tasks).await
```

类型规则：

```text
Task.all(List[Task[T]])       Task[List[T]]
Task.settled(List[Task[T]])   Task[List[TaskOutcome[T]]]
Task.any(List[Task[T]])       Task[T]
Task.race(List[Task[T]])      Task[TaskOutcome[T]]
```

`Task.all(tasks)` 在调用时快照 task handles；之后加入原 list 的 task 不属于该 join。结果保持加入顺序。动态 task 数量不会产生动态代码：同一个 worker coroutine descriptor/resume function 可以实例化任意多个 frame。

固定数量也可以通过显式 list literal 请求 list 结果：

```loom
let reports = Task.all([taskA, taskB]).await
```

tuple 与 list 不隐式互转。

运行时决定不同 task 类型时不能产生运行时变化的 tuple type，必须显式统一成封闭 enum/tagged union，或在构造点显式转换为共同 `dyn C`。不得自动经过 universal `any`。

### 5.3 join modes

| 操作 | 完成条件 | 未完成 sibling |
|---|---|---|
| `Task.all` | 全部 value-completed | 首个 task-local fault/cancel 后取消并 drain 其余 |
| `Task.settled` | 全部进入终态 | 不因单个失败取消其他 task |
| `Task.any` | 首个 value-completed | 取消并 drain 其余；无成功时产生组合失败 |
| `Task.race` | 首个 completion/fault/cancel | 取消并 drain 其余 |

`TaskOutcome[T]` 概念上包含 `Completed(T)`、`Faulted(fault)`、`Cancelled`。业务 `Result.Err` 是普通 `T` value，不被 join mode 解释为 task failure。OOM 等进程级 fault 绕过 `TaskOutcome`。

`all`/`settled` 对空 list 立即返回空 list；`any`/`race` 要求非空，静态已知为空时编译报错，否则触发合同失败。

`any`/`race` 的第一版只接受相同结果类型。未来异构形式必须返回显式 `Choice[A, B, ...]`，不能擦除为 `any`。

### 5.4 runtime join node

静态 tuple join 和动态 list join 共用 scheduler protocol：

```text
JoinState
├── mode
├── remaining
├── child registrations
├── typed result/outcome slots
└── waiting task
```

child completion 写入对应 slot 并更新计数；满足 mode 条件后只把 waiting task 放入 ready queue。实现不得轮询整个 task 集合。静态 tuple 使用编译期 layout descriptor，动态 list 使用 element descriptor 和动态 slots。

持续增加 task、并发限流或按完成顺序消费可以未来暴露 `TaskGroup[T]`；当前 one-shot list join 已覆盖由环境变量决定数量的场景，内部 `JoinState` 必须为该扩展保留动态容量，但不提前引入 async destructor。

## 6. MIR lowering 与 GC tracing

Loom 自己把 async body 降低成 stackless state machine，不依赖 LLVM coroutine source semantics：

```text
async HIR
  → typed MIR suspension points
  → frame promotion and liveness
  → resume-state dispatch
  → cleanup/cancellation states
  → backend-specific object code
```

每个 suspension point 有稳定 state id 和 live-local set。coroutine frame descriptor 必须让 GC 只跟踪已经初始化的 managed slots；取消状态还必须跟踪已经注册但尚未执行的 cleanup。优化可以收缩 frame、合并状态、内联 child 或消除 Task/JoinState 分配，但不得改变暂停、取消、cleanup、fault 和结果顺序。

closed-world reachability 从 entry/tests 继续遍历 async constructor、resume/cancel/trace descriptor、join mode helper 和 runtime builtin。环境变量改变实例数量不增加代码 roots；只有源码中可达的 task constructors 和显式 witness edges进入 DCE 集合。

## 7. 当前 reference implementation 状态

截至 2026-08-24，Core 0.3 C1 native 门已关闭：

- lexer/parser/HIR/sema/MIR 已实现 `scoped`、`defer`、`async fn`、后缀 `.await`、独立 `?`、`Task[T]`、`List[T]` 与 `TaskOutcome[T]`；旧前缀 await 只产生普通语法错误；
- `Dispose`、`MustScope`、`NoSuspend`、scoped 不可复制/逃逸、未消费 Task 和 interface access across await 均由静态检查器执行；
- lowering 为每个 await 分配稳定 state；线性 chain 按求值顺序抽取，if/match/block 内的 await 由同一 state dispatch 恢复；取消 state 保存挂起时已注册的 cleanup；
- interpreter 与 LLVM 都执行 normal return、早退、fault 和 cancellation 的块级 LIFO cleanup；取消传播到 child，join 在返回 winner/failure 前 drain sibling cleanup；
- Task 是单个 runtime pointer；单 Task、组合 Task 与 `Task.sleep/waitReadable/waitWritable` 都可先存储再等待。静态异构参数返回 tuple，动态同构 list 返回 list，`all/settled/any/race` 共享真实 composite Task/JoinState；
- Rust runtime 实现 version 1 `WaitSource`/generation-checked one-shot `Registration`/`ReadyNotification` ABI，macOS 用 kqueue、Linux 用 epoll。timer、Unix socket、completion、重复通知和取消均有 runtime fixture；源码 fd writable wait 同时通过 interpreter 与 native artifact；
- scheduler 只在 notification 后 enqueue，coroutine `resume` 真正返回 `Pending`，不会在 callback 栈重入或忙轮询；
- native precise moving heap 在 resume 之间以 Task slots/runtime results 为 roots，追踪 `Value` 与 `ValueNode`，回收不可达对象、复制存活对象并重写指针；Task identity 与 immutable witness metadata 非移动。fixture 直接验证旧/新地址不同和垃圾回收计数；
- `examples/core03` 以及专用 stored/dynamic join、nested await、取消 cleanup、fd readiness、moving-GC fixtures 均真实通过 check/build/test/source-run/native-run。runtime 全部使用 Rust 实现，不再保留 C++ wait/float runtime。

这关闭的是 C1 executable reference；package/path dependency、分层 cache、LLVM line-table/dSYM 与 Linux CI 已另行接入。它仍不表示多线程 executor、完整 file/socket 标准库、完整源码级 debugger/value inspection、正式性能门或 C2/C3 证据已经完成。
