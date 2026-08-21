# loom-lang

状态：**Active Language Experiment**

阶段：E0——语言设计基线与对照 fixture 设计（尚无编译器实现）

日期：2026-08-21

`loom-lang` 是一项编程语言实验：验证在普通文本文件、普通 Git 和常规 compiler/LSP 工作流中，**声明、约束和显式组合/贡献**能否比常规语言更清楚、更局部地组织真实系统。

如果实验成立，`loom-lang` 应能作为一门具有完整常规工具链的语言独立交付。

## 当前结论

- 源码首先是普通、可阅读、可 diff 的文本；用户可以用常规编辑器和 Git。
- 编译器从源码、显式依赖和构建配置构造并检查程序；同一组输入必须产生同一语义结果。
- 用户模型由声明、约束、依赖和显式贡献组成。
- 每个贡献必须显式指向目标；组合必须确定、可检查、可解释。工具必须回答“最终行为由哪些来源组成、为什么按这个顺序执行”。
- desired-state reconciliation 已确认属于长期语言/runtime 范围；当前后置实现，并为 observation、plan、action、receipt 与收敛安全另设证据门。
- E1 有意收缩为 raw-to-domain 约束建立、顺序 flow path、三个 closed-error ordered pipeline、direct target composition 和具名 host provider；不同时引入 bundle、footprint、开放错误或通用 DAG。
- 当前先冻结问题、fixture 与测量方法，不创建没有验证用途的编译器空壳。

## 文档入口

以下文档按主题构成当前权威基线：

| 主题 | 权威文档 |
|---|---|
| 项目命题、产品边界、证据等级 | [项目章程与边界](docs/00-charter.md) |
| 语言范围与核心语义 | [语言设计基线](docs/02-language-design-baseline.md) |
| 表面语法与代码风格 | [表面语法与代码风格](docs/03-surface-and-style.md) |
| 能力分期与进入条件 | [能力分期与决策状态](docs/04-capability-stages.md) |
| Checkout 对照方法与计分 | [第一项 checkout 对照实验](docs/01-first-experiment.md) |
| Checkout typed graph、错误映射与 oracle | [checkout fixture 设计](fixtures/checkout/README.md) |

跨主题引用必须与对应权威文档一致；出现冲突视为文档缺陷，不能由实现任选一份解释。

## 目标交付形态

未来最小产品即使只有下列传统形态，也必须有价值：

```text
.loom 普通文本
  -> parser / checker / composition compiler
  -> diagnostics / explain plan / executable artifact
  -> standard LSP

Git add / commit / branch / merge 仍是普通 Git
```

计划中的命令名仅用于界定产品边界，并不表示现在已有实现：

```text
loomc check
loomc build
loomc test
loomc explain <declaration>
```

`loomc explain` 与 `check` 同等重要；若编译器能够组合行为，却不能完整解释贡献来源、排序依据和冲突原因，则语言假设失败。

## 当前工作

E0 只产出可审查的语言与实验资产：

- 一份语言章程和可逐项确认的设计基线；
- 一份表面风格与能力分期；
- 一个 checkout 领域的等价基线设计；
- 固定任务、正确性 oracle、解释性问题与预注册指标；
- 明确的继续、重设计和停止条件。

下一道门不是“创建更多 crate”，而是先冻结 [语言设计基线](docs/02-language-design-baseline.md) 中标记为 E1 阻塞项，再审定 [第一项实验](docs/01-first-experiment.md) 与 [fixture 契约](fixtures/checkout/README.md)，确认语言切片和公平对照能够同时闭合。
