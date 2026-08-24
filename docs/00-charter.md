# loom-lang 项目章程

状态：Active / Core 0.1–0.3 C1 Executable Reference + C2 Implementation-Controlled Evidence

日期：2026-08-24

## 1. 当前命题

当前阶段只验证：

> 在普通文本、普通 Git 和常规 compiler/LSP 中，把值约束、对象不变量、函数契约与可复用行为接口做成不可绕过的语言语义，能否得到一门清楚、安全且适合日常编程的静态语言核心。

这份命题不依赖 live coding、结构化编辑、语义版本管理或专用 IDE。

## 2. 已确认的当前产品边界

Core 0.1 包含：

- `module`、显式 import 和可见性；
- `record`、`enum`；
- `fn`、method 与 receiver 读写区分；
- `Option`、`Result` 与穷尽 `match`；
- rank-1 基本泛型；
- 普通 `test`；
- `type T = Base where predicate`；
- `invariant`、`requires`、`ensures`、`assert`。

支撑这些能力的普通表达式基础——`let`、局部 `var`、`if`、block、尾表达式、提前 `return` 和基础运算——也属于 Core 0.1。

## 2.1 已确认的 Core 0.2 扩展

Core 0.1 闭合后，下一项语言扩展已经定稿：

- `concept` 是唯一行为抽象，不再增加平行的 `trait`；
- conformance 必须显式声明并满足 coherence；
- `T: C` 是泛型参数中的定义处静态约束；
- `dyn concept C` 在同一接口上额外承诺可擦除的运行时投影；
- 参数写作 `value C`；具体值自动适配，参数位置的 `value dyn C` 仅用于显式强调类型擦除，二者具有相同可观察语义；字段、返回与嵌套类型显式写 `dyn C`；
- `dyn C` 的语义是可复制的 concrete logical value 与显式 `T: C` proof 一起流动，不规定 fat-pointer 或其他物理布局；静态可知的调用允许完全去虚化，当前 LLVM C1 只在间接派发仍存在时物化 compiler-private data/witness 表示；
- 源码不引入 `view[...]`、borrow、lifetime、`box/shared` 或其他所有权 carrier。

完整语义见 [concept 与动态多态规范](05-concepts-and-dynamic-polymorphism.md)。实现分成 C1e static concept、C1f erased interface 两道门，但二者共用 conformance、witness 和合同语义。

## 2.2 已确认的 Core 0.3 扩展

普通语言闭环继续增加：

- 自动 tracing GC，不增加 ownership/borrow/lifetime 语法，对象地址和移动不可观察；
- 块级 `scoped` 与 `defer`，外部资源不依赖 GC/finalizer；
- compiler-known `Dispose`、`MustScope`、`NoSuspend`；
- `async fn`、显式后缀 `.await`、单指针 `Task[T]`；
- Loom MIR 自行降低的 stackless coroutine 和 ready-queue executor；
- 结构化并发、取消，以及 `all/settled/any/race`；
- 静态异构 tuple join 与动态同构 list join。

它不引入 Rust `Future`/`Poll`/`Pin` 表面、C++ coroutine customization、Promise callback chain、detached task、GC finalizer、weak reference 或 universal `any`。完整规则见 [GC、词法清理与异步任务定案](08-memory-cleanup-and-async.md)。

## 3. 明确不在当前规范中

下列方向仍可继续讨论，但当前没有权威语法或语义：

- AOP-like 静态组合、注入和贡献；
- desired-state、operator 和 reconciliation runtime；
- capability/provider 与 effect system；
- 多线程共享内存、持久化 coroutine 和分布式执行；
- 网络 registry 发布/认证、composition bundle 和 dynamic target；基础 `loom.toml`、path/文件 registry dependency、lockfile、optional-dependency feature、bin/test/lib target 与持久缓存已进入工具链；
- `example`、`scenario`、`property` 专用声明；
- entity/ORM、第二套 `trait` 抽象、继承、开放/多重派发、运行期实现发现和宏。

Core 只有一套行为抽象：`concept`。`T: C` 是定义处检查的静态约束；接口参数在确实需要类型擦除时才执行运行期 receiver dispatch。它不允许 registry、类路径扫描、import 激活实现，或 `A -> any -> dyn C` 的运行时 conformance 发现。

这些方向不能反向改变 Core 0.1 已确认的类型、契约和失败语义；需要扩展时必须通过新的小例子单独裁决。

## 4. 硬依赖规则

语言工具的输入是：

```text
当前源码 + 显式 module/import + 编译器与标准库版本
```

输出是：

```text
诊断 + typed program + 普通测试/构建结果
```

因此：

1. 普通 Git clone 可以直接执行 `loomc check/build/test`；
2. 相同输入必须得到相同类型检查和契约检查结果；
3. 文件遍历、修改时间、编辑器状态和隐藏注册表不参与语义；
4. LSP 与 CLI 必须调用同一 parser/checker；
5. 所有契约在所有构建模式中有效，只能因静态证明成立而消除。

## 5. 失败分轨原则

Core 0.1 区分三条失败轨：

- 不可信数据无法建立受约束值或带 invariant 的 record，是可处理的 `Violation`，通过 `Result` 返回；
- `requires`、`ensures`、已建立对象的 invariant 或 `assert` 被程序实现破坏，是不可作为业务分支捕获的结构化 `ContractFault`。
- checked Int 算术的溢出或非法除法是不可捕获的结构化 `RuntimeFault`，不伪装成业务 `Err` 或合同失败。

预期业务拒绝使用 `Result`。不得用 ContractFault 或 RuntimeFault 代替普通业务错误，也不得把程序缺陷伪装成 `Err` 后继续运行。

## 6. 证据等级

| 等级 | 能证明什么 | 不能证明什么 |
|---|---|---|
| C0 规范 | 语义边界可唯一说明 | 语法可实现、体验优秀 |
| C1 executable core | parser/checker/runtime 能执行规范 | 开发效率更高 |
| C2 controlled tasks | 固定任务上正确性与效率差异 | 大型工程长期收益 |
| C3 real repository | 一个真实项目中能持续使用 | 对所有领域普适 |

Core 0.1–0.3 的 C0 规范已经闭合；C1 reference implementation 已把 parser、checker、typed MIR、合同运行时、concept witness、erased interface、moving GC、词法 cleanup、结构化 Task、LLVM object/native artifact、CLI、formatter 和 LSP 接入同一 source/analysis pipeline。`examples/core01`、`examples/core02` 与 `examples/core03` 均真实通过 check/build/test/source-run/artifact-run，并以冻结 SHA-256、解释器/native 双 oracle、root 统计、性能上界和增量结构门形成 [C2 implementation-controlled evidence](09-quality-and-controlled-evidence.md)。这仍不等于人类开发效率对照或 C3 real repository。

## 7. 裁决原则

出现设计冲突时依次遵守：

1. 一个概念只保留一种主要失败语义；
2. 编译器保证不得依赖 release/debug 模式；
3. 不通过隐式转换、异常或运行期实现搜索隐藏控制流；显式 `dyn C` receiver dispatch 不属于搜索；
4. 先用最小例子闭合，再增加语法；
5. 尚未确认的能力不预留关键字和运行时；
6. 常规文本、CLI、测试和 LSP 必须能独立完成工作。

权威规则见 [Core 0.1 最小语言核心规范](02-language-design-baseline.md)、[Core 0.2 concept 与动态多态规范](05-concepts-and-dynamic-polymorphism.md)、三阶段共享的 [Core 0.1–0.3 可执行合同](06-executable-contract.md)、[编译过程与后端定案](07-compiler-pipeline-and-backends.md)以及 [Core 0.3 GC、词法清理与异步任务定案](08-memory-cleanup-and-async.md)。
