# 归档设计草案

状态：**Archived / Non-Normative / Paused**

整理日期：2026-08-21

本目录保存 `loom-lang` 在收缩到最小 Core 0.1 前讨论过、但尚未验证的设计路线。保留它们是为了以后能够从具体假设和反例继续讨论，而不是让旧方案继续暗中约束当前语言。

## 权威边界

- 本目录中的任何关键字、类型、运行时协议或实现分期都**不是**当前语言规范；
- 当前范围和语义以根 [README 权威表](../../README.md#文档权威关系)、[Core 0.1 规范](../02-language-design-baseline.md)和 [Core 0.2 concept/dyn 规范](../05-concepts-and-dynamic-polymorphism.md)为准；
- 当前 Core 不保留 `flow`、`slot`、`contribution`、`compose`、`capability`、`operator`、`reconcile` 等关键字；草案中的拼写都只是历史语义示意；
- 归档不等于否决。某条路线只有经过新的小实验、独立决策并写回规范文档，才会重新成为实现范围；
- 本目录不能作为 parser、checker、标准库或 runtime 自行补全语义的依据。

这些草案主要从 Git 快照 `572f9ef` 中的 Design Baseline 0.2、Surface Direction 0.2、Capability Map 0.2 和 Checkout Protocol 0.2 提炼而来。提炼保留的是设计意图、约束和开放问题，不保证旧文中的每个表面拼写原样保留。

## 草案索引

| 草案 | 保存的假设 | 当前状态 |
|---|---|---|
| [声明式静态组合与 AOP-like 扩展](01-declarative-composition-aop-like.md) | owner 开放具名 typed slot，外部 contribution 精确附着，编译期闭合顺序、错误和来源 | 暂停，等待 Core 之后的独立最小实验 |
| [Desired-state 与持续调和](02-desired-state-reconciliation.md) | desired 与 current 分离，由 observation、pure plan、幂等 action 和 durable receipt 驱动收敛 | 暂停，等待独立 runtime 正确性模型与真实场景 |
| [Checkout 组合实验协议](03-checkout-composition-experiment.md) | 与惯用 TypeScript composition root 公平对照的任务、指标和停止条件 | 暂停，仅作未来实验材料 |
| [Checkout 组合 fixture](04-checkout-composition-fixture.md) | ordered pipeline、typed error lane、provider 和 oracle 的历史具体化 | 暂停，仅作未来实验材料 |

## 为什么先归档

旧路线一次绑定了常规语言核、约束、静态组合、效果能力、build target、解释计划和持久调和 runtime。即使整体方向有价值，也无法判断一次实验的成功或失败究竟来自哪一层。

当前策略是先把 `record`、`enum`、`fn`/method、`Result`/`Option`、`match`、module、基本泛型、普通 test 和契约编程闭合为可执行语言核，再按独立证据门实现已经定稿的 `concept` 与显式 dyn receiver dispatch。静态组合与持续调和随后分别接受独立验证；一条路线失败时，不影响另一条，也不否定语言核心本身。

`concept`/`dyn concept` 不会使本目录中的组合方案自动复活：conformance 只说明一个类型满足一个接口，dyn carrier 只调用构造时封装的唯一 witness；二者都不会扫描、激活、排序或编织 contribution。
