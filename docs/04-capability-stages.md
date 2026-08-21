# loom-lang 能力分期

状态：Capability Map 0.2

证据等级：E0

日期：2026-08-21

本文把语言设想分成三层：E1 首个可执行实验、候选 v1、未来独立研究。分期的目的不是缩小长期目标，而是防止编译器先实现大量互相耦合的机制，却还没有证明显式组合比惯用语言更有价值。

## 1. E1：Checkout 最小可执行纵切

E1 只实现完成 [Checkout 实验](01-first-experiment.md) 所需的语言闭环。

### 1.1 必须实现

| 能力 | E1 合同 |
|---|---|
| 文本与 module | UTF-8、大括号、顶层恢复、module/import、普通多文件项目 |
| 基础类型 | Int、Bool、Text、Decimal、Unit、Option、Result、Vec；冻结 Checkout 所需运算、舍入、溢出/除零类别 |
| 领域数据 | raw record、type where、record、enum、显式构造与 invariant；entity/update 后置 |
| 表达式与控制流 | fn、let/局部 var、if、`for item in Vec`、封闭 enum/Option/Result 的最小穷尽 match、尾表达式、return、`?`、基础运算 |
| 错误 | typed Result；panic 与业务错误分开诊断 |
| capability | Authorization/Risk/Tax/OrderStore/AuditSink 预声明词法槽、`uses`、具名 host provider、target/scenario 精确 binding |
| flow | build-local 顺序 flow paths、数据/outcome 边、显式业务边、Ok/Err typed lane |
| 显式 slot | pricing、pre-authorization、authorization-rejected 三个 closed-error ordered pipeline |
| contribution | 一个 slot-local keyed transform、唯一目标、`uses` 不超过 slot/base 上界、来源可定位 |
| composition root | 单一 target 直接列 fully-qualified contribution 和全部 provider bindings |
| 组合检查 | missing target、duplicate transform key、unknown anchor、unordered pair、cycle、capability/error 越界 fail closed |
| 工具 | `loomc check`、`loomc test`、`loomc explain` |
| 验证声明 | pure example、每次 fresh host provider 的 scenario；property 后置 |
| 确定性 | canonical JSON diagnostic/explain 对相同输入逐字节一致；人类文本保证 code/span/participants/reason 一致 |

### 1.2 实现顺序

E1 是一个关门目标，不是一次性实现。按四个可独立验收的子门推进：

1. **E1a 常规语言核**：module、raw/domain record、fn、Result、最小 match、诊断和 pure example；
2. **E1b 数据约束**：`type where`、record invariant、Raw/Input 建立路径和结构化 Violation；
3. **E1c 显式组合**：flow、三个 typed pipeline slot、contribution、composition root、check/explain；
4. **E1d 外部效果**：capability、显式 provider、scenario、调用 trace 与完整 Checkout oracle。

每一门只扩展前一门的 typed program，不允许为赶任务同时引入第二种组合代数。

### 1.3 E1 的保守简化

- 不实现 resource footprint、并行或无序 flow；每条 outcome path 和每个 pipeline 都必须有唯一顺序；
- 不实现 field extension、policy fold 或运行期 registry；
- 不实现用户泛型；Result/Option/Vec 可先由标准库/编译器提供；
- 不实现通用 boundary adapter、FFI、并发或持久化；
- 不实现闭包、HOF 或通用 while；只实现 Checkout 汇总必需的确定性 `for item in Vec`，不在循环中开放 effectful contribution；
- 不实现 package registry；fixture 可用单 package、多 module；
- 不实现 provider body、resource 或 `use`；provider 由 host harness 提供，scenario 每次 fresh instance；
- 不实现 profile、feature 或 bundle；target 直接 compose contribution；
- 不生成生产 artifact；解释器或简单后端只需通过共享 oracle；
- 不实现目标状态调和。

### 1.4 E1 关门条件

E1 不是“parser 能跑”即完成，必须同时满足：

1. Checkout 两种实现通过同一行为、顺序、mutation 与 explanation oracle；
2. contribution 只能进入预先声明的 slot；
3. pipeline 的 empty identity、context threading、首错停止、closed error mapping 和 capability closure 有 executable tests；
4. pricing 至少覆盖 empty、A、B、A+B 全 subset，A+B 由显式边全序；
5. 授权拒绝审计通过 typed error lane 表达，验证失败不会误触发，handler failure 的错误优先级固定；
6. `explain` 与实际 trace 满足 typed path refinement：一次 trace 只能走计划中的唯一分支，且 capability 调用顺序相同；
7. 文件顺序、声明顺序和依赖遍历顺序不改变计划，所有组合歧义 fail closed；
8. TypeScript 基线通过外部惯用性审阅；进入 E2 计时前，候选具备 diagnostics/go-to/rename 的最低 LSP parity。

## 2. 候选 v1：可写常规应用

只有 E1 机制正确且 Checkout 对照值得继续，才扩展到候选 v1。

### 2.1 通用语言层

- rank-1 用户泛型；
- 内建 concept 与结构派生；
- 纯函数值/闭包与常用 HOF；
- `for` / `while`、Range、Map、Set；
- 完整 pattern matching 与封闭 enum 穷尽检查；
- Decimal、Text、time 的稳定标准语义；
- `use` 资源作用域；
- entity key 与受约束 update；
- package、版本、lockfile 和标准 LSP。

### 2.2 约束与边界

- 递归约束类型和 record/entity invariant；
- boundary 解码、错误闭合、Violation blame 和 panic 隔离；
- HTTP/CLI/queue/storage adapter 库；
- 约束感知 property 生成器；
- schema 版本与显式纯迁移函数。

### 2.3 已验证组合的工程化

- ordered pipeline 与 typed lane 的库级 public contract；
- package contribution policy；
- 经独立大型工程 fixture 解锁后，target 可显式启用 package feature/bundle composition domain，bundle membership 由贡献方声明并自动参与；普通 dependency/import 不激活；未过该门时 v1 继续使用 direct target；
- composition plan diff 与 contribution policy 报告；
- machine-readable `explain` schema；
- provider declaration/body、构造、实例 lifetime 与 binding closure。

keyed members、policy rule fold、field/provider contribution 都是独立候选实验，不因 pipeline 通过而自动进入 v1。

### 2.4 效果与构建

- 经独立 fixture 解锁的 resource key family 与 `reads`/`touches` footprint；
- 经独立 fixture 解锁的 public effect upper bound 与 composition-derived effect row；
- capability provider 的并发/幂等等显式合同；
- 确定 artifact、dependency lock 与构建 manifest；
- package 级精确增量编译和影响查询。

### 2.5 验证

- pure example；
- explicit-provider scenario；
- property；
- composition plan 与执行 trace 对齐；
- 第二个领域 fixture，证明组合模型不是 Checkout 特例。

## 3. 后续能力阶段（独立证据门）

以下能力各自需要新的场景、正确性模型和退出条件，不能随候选 v1 自动进入。

### 3.1 已确认长期范围：目标状态调和

这仍是 loom-lang 的语言/runtime 目标，不是第三个项目。研究内容：desired/current 分离、typed observation、pure plan、capability action 与 resource scope、at-least-once、幂等 key、durable receipt、程序 basis、管理域重叠、崩溃恢复、换版与收敛/`Escalated`。

解锁条件：Checkout 与第二个领域已经证明语言核不是单一 fixture 特例；同时至少一个真实的跨重启业务过程无法用普通 flow 清楚表达，并且团队已有可验证的 observation、幂等 provider 与 durable receipt 合同。

### 3.2 结构化并发

研究内容：task scope、取消传播、deadline、并行 flow 调度、失败聚合与 resource cleanup。

解锁条件：顺序执行成为已测瓶颈，且 footprint 模型在真实 provider 上足够准确。

### 3.3 高级抽象

可能包括用户 trait、效果多态函数值、开放 dispatch、受限 compile-time 派生。每项必须由标准库或第二领域的具体缺口解锁。

### 3.4 性能与后端

可能包括原生代码、WASM、服务 runtime 和更可控的内存布局。它们不改变声明/约束/组合语义，并以真实 workload 而不是语言展示解锁。

## 4. 暂不进入路线的能力

- 任意用户宏和语法改写；
- 用户自定义组合代数；
- 隐式全局目标匹配；
- algebraic effect handler 与可恢复 continuation；
- 所有权/借用作为默认内存模型；
- 依赖任意对象回收时机的终结器；
- 跨 capability 原子事务；
- 通用分布式 exactly-once；
- 持久化任意命令式调用栈；
- 内建部署控制面。

## 5. 决策状态矩阵

| 主题 | 当前状态 | 备注 |
|---|---|---|
| 静态类型、值语义、自动内存管理 | 确认 | E1 起遵守 |
| 大括号文本语法、缩进非语义 | 确认 | E1 grammar |
| 声明/实现/状态分离 | 确认 | 核心原则 |
| 约束类型与 invariant | 确认 | E1 最小实现 |
| Result + panic 分轨 | 确认 | E1 |
| capability + `uses` | 确认 | E1 |
| slot + targeted contribution | 确认语义 | 精确 grammar 待冻结 |
| fn 有序、flow 依赖图 | 确认 | E1 |
| typed Ok/Err flow lane | 确认需求 | 精确语义需纸面执行 |
| ordered pipeline | 确认语义 | E1 唯一组合代数 |
| keyed/rules/field/provider contribution | 开放 | 各自独立 fixture，不自动进入 v1 |
| import 与 contribution 激活分离 | 确认 | 激活由 build target |
| resource footprint | 开放 | 独立 fixture 通过前一律使用显式边 |
| package feature/bundle activation | 开放 | 需大型工程 fixture；E1/direct target 可独立成立 |
| rank-1 泛型、纯闭包 | 暂定 v1 | E1 可后置 |
| boundary 三职责 | 确认方向 | E1 不实现通用 adapter |
| 并发/取消语法 | 开放 | 另行实验 |
| 目标状态调和能力 | 确认长期范围 | loom-lang 后续阶段 |
| 目标状态语法、存储和调度协议 | 开放 | 独立证据门 |
| 编译后端与包注册表 | 开放 | 不阻塞 E1 |

## 6. 下一份必须产出的设计资产

在创建编译器工程前，按顺序完成：

1. Checkout base flow 的 typed graph；
2. pricing、pre-authorization 与 authorization-rejected 三个 pipeline slot 的精确输入/输出；
3. Raw/Input -> constrained domain -> CheckoutError 的唯一建立和映射；
4. closed CheckoutError、每个 slot error、handler failure priority；
5. Authorization/Risk/Tax/OrderStore/AuditSink host provider contract、fresh lifecycle 与 bindings；
6. pricing empty/A/B/A+B 计划及 T1–T4 纸面 composition plan；
7. 成功、验证失败、风险拒绝、授权拒绝、handler 失败、持久化失败执行 trace；
8. slot/lane grammar 候选、`explain` JSON schema/golden 与 E1 type rules；
9. Decimal 字面量、scale/舍入，以及 Int/Decimal 溢出、除零和越界的 defect oracle；
10. 内建结构 equality、Vec 字面量/遍历、最小 pattern、穷尽 match 和 `result`/`return` 控制类型；
11. Raw schema、Violation code/path/value summary、确定首错顺序和 canonical JSON；
12. host-provider descriptor manifest、adapter digest、fresh constructor、fixture alias 与 trace schema。

这十二项能闭合，才说明 E1 executable contract 足够清楚，可以冻结 parser AST/checker API。在此之前只能做不承诺 AST 形状的一次性 lexer/error-recovery spike。
