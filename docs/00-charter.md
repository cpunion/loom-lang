# loom-lang 项目章程

状态：Active / Core 0.1 Confirmed Draft + Core 0.2 Static/Borrowed Dyn Confirmed

日期：2026-08-21

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
- `T: C` 是定义处检查的静态约束；
- `dyn concept C` 在同一接口上额外承诺可擦除的运行时投影；
- Core 0.2 动态值必须显式选择 `view[dyn C]` 或 `view[mut dyn C]`，不得使用所有权不明的裸 `dyn C`；
- `box[dyn C]` / `shared[dyn C]` 是已选择但尚未规范闭合的 owning carrier 方向。

完整语义见 [concept 与动态多态规范](05-concepts-and-dynamic-polymorphism.md)。实现必须拆成 C1e static concept、C1f borrowed dyn 两道门；owned/shared 必须先另行闭合 C0 所有权规范，不能反向阻塞 Core 0.1/0.2。

## 3. 明确不在当前规范中

下列方向仍可继续讨论，但当前没有权威语法或语义：

- AOP-like 静态组合、注入和贡献；
- desired-state、operator 和 reconciliation runtime；
- capability/provider 与 effect system；
- async、并发、持久化和分布式执行；
- package、target、feature/bundle；
- `example`、`scenario`、`property` 专用声明；
- entity/ORM、第二套 `trait` 抽象、继承、开放/多重派发、运行期实现发现和宏。

Core 只有一套行为抽象：`concept`。`T: C` 是定义处检查的静态约束；只有显式动态载体才执行运行期 receiver dispatch。它不允许 registry、类路径扫描或 import 激活实现。

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

Core 0.1 区分两类失败：

- 不可信数据无法建立受约束值或带 invariant 的 record，是可处理的 `Violation`，通过 `Result` 返回；
- `requires`、`ensures`、已建立对象的 invariant 或 `assert` 被程序实现破坏，是不可作为业务分支捕获的结构化 `ContractFault`。

预期业务拒绝使用 `Result`。不得用 contract fault 代替普通业务错误，也不得把程序缺陷伪装成 `Err` 后继续运行。

## 6. 证据等级

| 等级 | 能证明什么 | 不能证明什么 |
|---|---|---|
| C0 规范 | 语义边界可唯一说明 | 语法可实现、体验优秀 |
| C1 executable core | parser/checker/runtime 能执行规范 | 开发效率更高 |
| C2 controlled tasks | 固定任务上正确性与效率差异 | 大型工程长期收益 |
| C3 real repository | 一个真实项目中能持续使用 | 对所有领域普适 |

当前处于 C0 草案：范围和本文列出的主语义已经确认，但 [实现分期](04-capability-stages.md#6-实现前仍需冻结的可执行细节) 中的 executable contract 尚未全部闭合。

## 7. 裁决原则

出现设计冲突时依次遵守：

1. 一个概念只保留一种主要失败语义；
2. 编译器保证不得依赖 release/debug 模式；
3. 不通过隐式转换、异常或运行期实现搜索隐藏控制流；显式 `dyn C` receiver dispatch 不属于搜索；
4. 先用最小例子闭合，再增加语法；
5. 尚未确认的能力不预留关键字和运行时；
6. 常规文本、CLI、测试和 LSP 必须能独立完成工作。

权威规则见 [Core 0.1 最小语言核心规范](02-language-design-baseline.md)和 [Core 0.2 concept 与动态多态规范](05-concepts-and-dynamic-polymorphism.md)。
