# loom-lang

状态：**Active Language Design / Core 0.1 + Confirmed Core 0.2 Static/Borrowed Dyn**

阶段：Core 0.1 正在闭合 executable contract；Core 0.2 的 static concept 与 borrowed dyn 已定稿；owned/shared dyn 仅确认方向，尚无编译器实现

日期：2026-08-21

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
- `invariant`、`requires`、`ensures`、`assert`；
- 默认只读 receiver 与显式 `mut self`。

紧随 Core 0.1 的 **Core 0.2 已确认扩展**是：

- 唯一的行为抽象 `concept`、显式 conformance 和 associated type；
- `T: Concept` 的定义处检查与静态派发；
- `dyn concept` 的运行时投影；
- 不允许裸 `dyn C`，Core 0.2 动态值必须显式选择 `view[dyn C]` 或 `view[mut dyn C]`；
- owning carrier 方向确定为 `box[dyn C]` / `shared[dyn C]`，但在 affine/shared 所有权合同闭合前不是可接受源码。

Core 0.1 权威基线见 [最小语言核心规范](docs/02-language-design-baseline.md)，具体书写见 [核心表面与代码风格](docs/03-surface-and-style.md)；Core 0.2 的行为抽象见 [concept 与动态多态规范](docs/05-concepts-and-dynamic-polymorphism.md)。尚待闭合的可执行细节与实现边界见 [核心能力分期](docs/04-capability-stages.md)。

## 尚未确认

以下仍需单独讨论和小实验，不属于 Core 0.1：

- AOP-like 静态组合、注入点、贡献与排序；
- desired-state、operator 与持续调和；
- capability/provider、effect、异步与并发；
- `example`、`scenario`、`property` 等专用验证声明；
- package、target、feature/bundle 与大型工程组合治理。

普通 `test` 已足够验证当前核心；不会为了未来能力提前保留关键字或运行时模型。

`concept` 描述一个类型提供哪些显式操作；`dyn concept` 只把其中可擦除的 receiver methods 投影为动态调用。它不扫描实现、不按名字注入行为，也不等同于 AOP contribution、capability/provider 或运行期插件发现。

## 文档权威关系

| 主题 | 权威文档 |
|---|---|
| 项目边界和裁决原则 | [项目章程](docs/00-charter.md) |
| Core 0.1 语义 | [最小语言核心规范](docs/02-language-design-baseline.md) |
| Core 0.1 表面写法 | [核心表面与代码风格](docs/03-surface-and-style.md) |
| Core 0.2 concept/dyn 语义与表面 | [concept 与动态多态规范](docs/05-concepts-and-dynamic-polymorphism.md) |
| 实现顺序与开放问题 | [核心能力分期](docs/04-capability-stages.md) |

[历史设计草案](docs/draft/README.md)保存此前的声明式组合、AOP-like 与 desired-state/operator 方案；其中 [Checkout 对照实验](docs/draft/03-checkout-composition-experiment.md)及其 [fixture](docs/draft/04-checkout-composition-fixture.md) 均不是当前语言规范。

## 目标交付形态

Core 0.1 采用普通、静态的工具链：

```text
.loom 普通文本
  -> lexer / parser / type checker / contract checker
  -> diagnostics / executable program / ordinary tests
  -> standard LSP

Git add / commit / branch / merge 仍是普通 Git
```

计划命令只描述产品边界，并不表示已有实现：

```text
loomc check
loomc build
loomc test
```

下一步是在不引入组合或 operator 能力的前提下，先为 Core 0.1 补齐 executable grammar、诊断 golden 和最小解释器/编译器切片；随后按 C1e static concept、C1f borrowed dyn 两个独立证据门实现 Core 0.2。owned/shared dyn 必须先另行闭合 C0 所有权规范。
