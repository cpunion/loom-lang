# loom-lang

状态：**Active Language Design / Core 0.1 Confirmed Draft**

阶段：已确认最小核心的范围与主语义，正在闭合 executable contract；尚无编译器实现

日期：2026-08-21

`loom-lang` 当前只确认一组小而完整的常规语言能力：代数数据类型、函数与方法、显式失败、穷尽匹配、模块、基本泛型、普通测试，以及受约束值和契约编程。

目标是先回答一个更小的问题：

> 一门普通静态语言能否让“合法值、合法对象状态和函数契约”成为编译器持续执行的语言事实，同时保持熟悉、可阅读的文本编程体验？

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

权威基线见 [最小语言核心规范](docs/02-language-design-baseline.md)，具体书写见 [核心表面与代码风格](docs/03-surface-and-style.md)，尚待闭合的可执行细节与实现边界见 [核心能力分期](docs/04-capability-stages.md)。

## 尚未确认

以下仍需单独讨论和小实验，不属于 Core 0.1：

- AOP-like 静态组合、注入点、贡献与排序；
- desired-state、operator 与持续调和；
- capability/provider、effect、异步与并发；
- `example`、`scenario`、`property` 等专用验证声明；
- package、target、feature/bundle 与大型工程组合治理。

普通 `test` 已足够验证当前核心；不会为了未来能力提前保留关键字或运行时模型。

## 文档权威关系

| 主题 | 权威文档 |
|---|---|
| 项目边界和裁决原则 | [项目章程](docs/00-charter.md) |
| Core 0.1 语义 | [最小语言核心规范](docs/02-language-design-baseline.md) |
| Core 0.1 表面写法 | [核心表面与代码风格](docs/03-surface-and-style.md) |
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

下一步是在不引入组合或 operator 能力的前提下，为 Core 0.1 补齐 executable grammar、静态规则、诊断 golden 和最小解释器/编译器切片。
