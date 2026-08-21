# loom-lang 语言设计基线

状态：Design Baseline 0.2

证据等级：E0（设计已确认，尚未由编译器和真实任务验证）

日期：2026-08-21

本文回答“loom-lang 是一门什么语言”。它是语言范围与核心语义的当前权威；[表面与代码风格](03-surface-and-style.md)回答代码应当如何书写，[能力分期](04-capability-stages.md)回答哪些能力进入首个实现、候选 v1 与未来研究。

文中使用三种状态：

- **确认**：除非实验给出反例，否则实现必须遵守；
- **暂定**：方向已选，但具体语法、算法或运行时合同仍可调整；
- **后置**：不进入当前实现，需由新的真实场景解锁。

## 1. 定位与目标领域

loom-lang 是一门面向应用系统和大型工程组织的静态类型语言，优先服务：

- 领域模型、业务服务、CLI 与数据处理程序；
- 规则持续增加、多个 feature 向同一业务能力贡献行为的系统；
- 需要把外部依赖、失败、约束和执行顺序说清楚的代码；
- 需要可重复构建、可解释组合和明确模块边界的大型代码库；
- 未来需要用目标状态和持续调和表达长时间业务过程的系统。

首要目标不是语法更短，而是减少四种工程歧义：约束写在哪里、行为来自哪里、为什么存在当前顺序、某个外部效果由谁提供。

候选 v1 不是系统编程语言，不以手工内存管理、内核/嵌入式开发、极致数值计算或元编程 DSL 为首要场景。部署、切流、集群租约和灾备属于外部基础设施；语言只产生可执行程序及其明确的依赖合同。

## 2. 四条核心原则

### 2.1 声明、实现和状态分离

编译器必须区分：

| 层 | 回答的问题 | 例子 |
|---|---|---|
| 声明/合同 | 什么值或行为才合法？ | 类型、约束、函数签名、能力操作、开放组合槽 |
| 实现 | 如何计算或提供合同？ | 函数体、能力 provider、具名贡献 |
| 状态 | 这次运行或外部世界现在是什么？ | record/entity 值、数据库内容、未来调和观测 |

三者可以在相邻源码中书写，但不能在语义上混为一物。公共合同变化与实现体变化是不同类别；运行状态不能反向决定编译语义。

### 2.2 声明即约束

每个声明都是由编译器执行的合同，而不是注释：类型限制值域，函数签名限制调用，能力需求限制外部依赖，开放 slot 限制可贡献形状。本文专门使用“数据约束”时，只指 `where` / `invariant` 定义的值合法性，不把业务 eligibility、执行顺序或任意实现性质都塞进同一种约束机制。

领域数据约束只有一个声明位置；值进入领域类型后应保持有效，不要求调用方反复手写校验。

### 2.3 顺序即依赖

不同上下文采用不同顺序语义：

- 普通函数体是有序算法，语句先后有语义；
- 顶层声明、record 字段、独立规则和 flow 节点的文本排列没有执行语义；
- flow 中方向明确的数据依赖会产生顺序；
- 真实业务先后由显式 `before` / `after` 边表达；
- 效果足迹重叠只能产生“必须定序”的义务，不能替程序员猜测方向；
- 长期只有被独立组合规则证明可交换且不可观察顺序的节点才可无序；规范计划可用稳定 key 序列化它们，但展示顺序不是程序语义。E1 不实现 totality/commutativity 证明，每条 outcome path 和 pipeline 都要求唯一顺序，绝不用任意 tie-break 掩盖缺失关系。

### 2.4 组合必须显式且可解释

任何开放组合都包含两个显式对象：

1. 目标声明公开一个**具名、具类型、具组合规则的槽**；
2. 贡献声明以限定名指向这个槽。

不存在对任意函数、调用点或名字模式的隐式匹配。构建器必须从用于执行的同一组合计划回答：成员来自哪里、为何适用、为何有当前顺序，以及一个确定、可定位的冲突 witness。E1 不承诺计算全局最小冲突集合。

## 3. 程序与名字

### 3.1 编译输入

程序语义只由以下输入决定：

```text
源码 + 显式依赖及 lockfile + 构建目标配置 + 编译器/标准库版本
```

同一输入必须产生同一诊断、typed program 与组合计划。具体后端一旦锁定，还必须另行规定 artifact 的可重复性；E0 不用“组合计划确定”冒充“所有平台产物逐字节相同”。环境变量、目录遍历顺序、文件修改时间和依赖下载时机不得成为隐藏语义输入。

### 3.2 源码名字就是编译身份

声明以 package、module 与声明名组成的限定名解析。公开 rename 是普通 API 变化；包版本与迁移工具负责兼容性，不引入用户不可见的历史身份。

文件是阅读和组织单位，不是声明身份：

- 一个 module 可以分布在多个文件；
- 文件移动不改变 module 内限定名；
- 同一 module 内顶层声明排列不影响程序；
- module 声明与实际目录布局的约束由项目配置统一规定。

顶层只允许声明和编译期常量，不运行 effectful 初始化代码。程序世界效果必须从显式入口经 capability 发生。

## 4. 声明模型

候选 v1 的语言声明分为六组：

| 组 | 声明 | 作用 |
|---|---|---|
| 数据 | `type`、`record`、`entity`、`enum` | 表达领域值、领域键、封闭变体与约束 |
| 计算 | `fn` | 有序的局部算法与可复用计算 |
| 外部依赖 | `capability`、provider、resource | 分离外部操作合同、实现和实例状态 |
| 组合 | slot、contribution、policy、flow | 表达开放成员、规则集合和依赖图 |
| 边界 | boundary | 解码不可信输入、闭合错误并隔离缺陷 |
| 验证 | example、scenario、property | 表达示例、场景与生成式性质 |

build target 属于构建配置：选择入口、依赖、组合成员与 capability provider，关闭所有开放槽，得到确定程序。E1 只有一个 repo-local target，直接列 fully-qualified contribution 和 provider binding；profile、feature 与 bundle activation 后置，避免首个实现同时承担四层配置语义。

候选 v1 的大型工程方向仍确认：target 显式把 package feature/bundle 放进 composition domain，成员关系在贡献方声明；bundle 启用后其定向 contribution 自动参与，消费方无需逐项登记。普通 dependency 或 import 永远不激活行为，完整路径必须能解释为 `target -> enabled feature/bundle -> contribution -> slot`。

### 4.1 数据声明

- `type T = Base where predicate` 定义名义约束类型；同一基类型和同一谓词的两个不同名字仍是不同类型；
- `record` 是不可变值积；
- `entity` 也是值快照，但额外声明领域 key，用于寻址和持久化合同；它不是隐式 ORM、identity map 或可变远程对象；
- `enum` 是封闭和类型；变体顺序没有隐式整数含义；
- `T?` 是 `Option[T]` 的表面糖；
- record 字段顺序只服务阅读和格式化，结构相等与编码不得依赖文件排列；
- record 可按字段名和值派生结构相等；entity 不自动定义 `==`，领域身份使用 `same_key(a, b)`，快照内容比较必须显式；
- 公共类型、函数与能力的签名必须显式，局部实现允许推断。

### 4.2 值与内存

确认采用：不可变默认、局部 `var` 可变、值语义、自动内存管理。资源生命周期不依赖对象回收时机；文件、连接等资源使用词法 `use` 作用域保证释放。

候选 v1 的基础类型方向：

- `Int`：有符号 64 位，普通溢出属于缺陷；
- `Decimal`：十进制精确表示，除法必须给出舍入策略；
- `Bool`；
- `Text`：UTF-8 的 Unicode 标量序列，保留源码/输入的标量内容；NFC 等正规化必须由显式类型或 API 请求，不能在不同边界悄悄改变；
- `Option`、`Result`、`Vec`、`Map`、`Set`；
- `Instant`、`Monotonic`、`Duration` 和显式时区的 civil time 分型。

精确 wire 编码和时间 API 在实现相应边界前冻结，不作为 E1 parser 的前置条件。

## 5. 约束与信任边界

### 5.1 约束类型恒有效

约束值只有三个建立入口：

1. 编译器可以证明的合法字面量；
2. 显式构造，例如 `Money.from(value) Result[Money, Violation]`；
3. boundary 对外部表示执行派生解码和校验。

约束类型可以隐式上转为基类型；基类型不能隐式进入约束类型。不能证明保持约束的运算返回基类型，调用方必须重新建立。容器保持不变型，例如 `Vec[Money]` 不是 `Vec[Decimal]`。

record/entity 的 `invariant` 在构造和产生新值的更新处检查。E1 不实现通用证明器：除合法字面量外，`from` 和重建一律在运行时返回 `Result`。

E1 约束 AST 只允许字段投影、字面量、比较、布尔组合、穷尽 Option match 和少量 compiler-known total primitive；普通用户函数、递归、循环、`?`、capability 与 partial unwrap 一律拒绝。Violation 表示输入不满足合法谓词，不把实现缺陷伪装成普通业务违约。

### 5.2 boundary 的职责

boundary 同时承担三项语言职责：

- 从 HTTP、CLI、queue、存储或 FFI 表示派生领域值；
- 要求所有预期错误在入口处映射闭合；
- 把 panic 限制在一次边界调用中，并生成结构化 incident。

Violation 至少包含字段路径、约束、值摘要和来源边界。适配器是库，boundary 的类型/错误/隔离合同属于语言。

E1 Checkout 使用确定性内存 provider，不要求先实现通用 boundary 派生器。非法数量、金额和 VAT 输入以无约束 `CheckoutInput` / `Raw*` record 表示，再经唯一 `CheckoutRequest.from(raw)` 路径建立 `Quantity`、`Money`、`Customer` 与 `CheckoutRequest`。共享 oracle 只能向 raw 类型注入非法值；构造失败固定映射为 `CheckoutError.Validation`，且不会进入 flow 或产生 capability trace。

## 6. 函数、控制流与失败

普通函数是有序、表达式导向的静态函数：

- `let` 不可变，`var` 局部可变；
- `if`、`match`、`for`、`while`、`return`、`break`、`continue`；
- 尾表达式作为返回值；
- 预期业务失败用 `Result[T, E]` 与 `?`；
- `?` 只传播相同的错误类型；不同封闭错误之间必须显式 `match` 并构造目标 variant，不提供隐式 `From`；
- `panic` 只表示除零、越界、断言等缺陷，用户代码不能捕获后继续业务分支；
- 函数值与闭包进入候选 v1，默认按值捕获；首版只允许纯函数值，效果多态回调后置。

泛型方向确认为 rank-1 显式类型参数，不提供 HKT 或特化。候选 v1 先使用封闭内建 concept 集（`Eq`、`Ord`、`Hash`、`Show`、`Decode`、`Encode`、`Num`）；用户定义 trait 需真实库场景解锁。

## 7. capability 与效果

### 7.1 外部依赖

capability 是一组具名操作合同；函数以 `uses slot_name CapabilityType` 声明所需的词法槽。provider 不按类型搜索：只有 target/scenario 的 `slot_name -> qualified provider instance` 绑定有效，缺失绑定、同一槽重复绑定或类型不符都报错；依赖图中存在多个 provider implementation 本身不构成歧义。

capability 是词法作用域中的依赖句柄，不是普通一等值：它不能存进普通数据、从函数返回或被闭包捕获。这样的限制保证依赖范围和测试替换仍然可见。

时钟、随机数、文件、网络和数据库都必须通过 capability；没有 ambient 全局实例。FFI 只能实现 capability 操作，必须声明效果并把返回表示经过 boundary 检查。

provider 完成声明/实现/状态分离：provider declaration 明确 `implements Capability`，完整实现相同操作签名和错误类型，也可以显式 `uses` 其他 capability；provider declaration 是实现，provider instance 才是状态。build target 绑定具有明确 program lifetime 的构造，scenario 每次运行创建 fresh instance，不存在通过类型名访问的全局 provider 状态。

E1 不实现 loom 源码中的 provider body，只绑定测试 harness 提供的具名内存 provider。每个 host provider 必须由随源码/构建配置版本化的 descriptor 声明 qualified name、implemented capability、operation signature、fresh constructor、adapter version/digest 与 trace schema；checker 和 test runner 不得从进程环境猜测。scenario 的调用 trace 由该锁定 adapter 返回；若需要检查 stateful stub，scenario 使用本次运行的 fixture alias，不读取 `RecordingAudit.events` 一类静态全局。

### 7.2 效果足迹

效果足迹是候选 v1 方向，不进入 E1 最小实现：

- capability 可以声明 resource key family；
- 操作用 `reads` / `touches` 描述有限资源集合；
- 调用方足迹由编译器保守推断；
- 两个读取不冲突，读写或写写重叠产生排序义务；
- 编译器无法证明不相交时必须保守要求显式边；
- 足迹不会自动决定先后方向；
- provider 对声明足迹的遵守是可测试/可审查合同。

E1 中所有无数据依赖的效果步骤都必须用显式业务边定序；只有当 footprint 模型通过独立实验后，才允许省略可证明无关步骤之间的边。

并发和取消的用户表面尚未冻结。确认的是：不会用隐式共享可变状态掩盖并发，能力依赖和资源作用域在并发下仍须保持可见。

## 8. 显式组合

### 8.1 统一 slot 模型

目标声明必须先公开命名 slot；slot 合同包含：

- 唯一限定名；
- 接受的 contribution 形状；
- 输入/输出类型和闭合错误类型；
- 使用的组合规则；
- contribution 允许使用的 capability 槽上界；
- 允许贡献的 package 范围；
- 组合失败的确定诊断。

contribution 有自己的名字和源码位置，明确指向一个 slot。未公开的目标不可扩展；不存在“对所有同名函数生效”或“在某调用前后插入”的语义。

### 8.2 组合代数分别解锁

长期只考虑少量内建组合，不允许用户任意定义组合代数：

| slot 类型 | 用途 | 冲突/顺序规则 |
|---|---|---|
| keyed members | 字段、注册项、命名 provider | key 唯一，重复即错误 |
| rules | eligibility、集合约束、allow/deny | 仅使用语言内建且满足交换律的 fold |
| ordered pipeline | checkout、导入、审批等流程 | 同型 transform；显式全序；首个错误停止 |

E1 只实现 ordered pipeline。keyed members、rules、field/provider contribution 和 footprint 必须各自由后续 fixture 单独解锁，不因 Checkout 通过自动进入候选 v1。用户自定义组合代数、开放模式 dispatch 和隐式 provider choice 后置。

### 8.3 flow 与普通函数的边界

- `fn` 用于单一所有者、局部有序算法；
- `flow` 用于明确需要开放步骤、具名阶段和组合解释的顺序过程；
- E1 flow 节点拥有 typed input/output，错误结果也是显式 typed lane；每条可执行 outcome path 必须形成唯一顺序；
- contribution 只能进入 flow 预先公开的 slot，不能包围任意函数；
- flow 的实际执行计划由数据/outcome 边和显式业务边确定，源文件中的节点排列不决定顺序；并行/无序 DAG 后置。

### 8.4 E1 唯一的 step slot

E1 不实现任意形状的 DAG 注入，只实现目标拥有的 typed pipeline slot：

```text
pipeline[S, E]
empty                         = Ok(input)
one contribution             = S -> Result[S, E]
multiple active contributions = explicit total order; output feeds next input
```

`E` 是 slot owner 定义的封闭错误类型。contribution 必须返回精确 `Result[S, E]`，不能扩展错误联合；pipeline 依序传递 context，首个 `Err` 停止。基础 flow 在 slot 调用点显式把 `E` 映射到自己的封闭错误类型。

slot 在基础 flow 中有唯一、可见的调用位置，因此它位于哪些基础步骤之间由正常数据/typed outcome 边决定。每个 contribution 恰好提供一个带显式 slot-local key 的同型 transform，不能跳转 lane、包围函数、截获未声明错误或引用 slot 外节点。

同一 slot 有多个 active contribution 时：

- checker 先为 target-visible、同一 slot 的全部 declared transforms（active 或 inactive）建立 key catalog；key 在该 catalog 中必须无条件唯一，anchor 只能解析到唯一 catalog entry；
- `before` / `after` 只有两端都 active 时才进入执行计划，因此可选 contribution 单独启用仍合法；引用 catalog 中不存在的 key 是 unknown anchor；
- 所有 active members 必须形成显式全序，不尝试证明 pure、total 或 commutative；
- 不同来源使用相同 key、未知 anchor、未定序 pair 和 cycle 都 fail closed；explain 同时保留 key 与 qualified source；
- empty slot 返回 `Ok(input)`，启用/移除 contribution 不要求修改目标 flow 实现。

E1 的 open flow 只作为 build-local entry，不导出为稳定库 API。它在声明中预列 Authorization、Risk、Tax、OrderStore、AuditSink 等全部 capability 槽；每个 contribution 的 `uses` 必须是该集合及目标 slot `allows` 上界的子集，禁止组合时引入新 capability。target 为所有预声明槽绑定唯一 provider，inactive contribution 只是不调用相应槽。composition-derived public effect row 后置。

Checkout E1 需要三个明确 slot：`pricing`、`pre_authorization` 和 `authorization_rejected`。最后一个只在 Authorization 的 typed rejection lane 调用，保证“授权拒绝要审计、验证失败不审计”由显式路径表达，而不是由全局拦截实现。

### 8.5 激活与可发现性

import 只控制源码名字可见性，不激活 contribution。E1 只有 direct composition：单一 build target 逐项列 fully-qualified contribution；重复列同一 contribution 直接失败。`loomc explain` 的激活路径固定为 `target -> contribution -> target slot`。

候选 v1 的大型工程阶段再引入 package feature/bundle composition domain：成员关系由贡献方显式声明，target 启用 bundle 后成员自动参与而无需中央逐项登记；普通 dependency/import 仍不激活。届时 explain 路径扩展为 `target -> enabled feature/bundle -> contribution -> slot`，锁定依赖升级和 membership 变化必须产生可审阅的 plan diff。

## 9. package、module 与构建

- package 是版本、分发、可见性和贡献政策边界；
- module 是 package 内命名空间；
- package 依赖和 E1 module import 必须无环；若后续真实场景需要 type-only cycle，须以独立规则解锁；
- 可见性为 `pub`、`package`、`private`；
- public 声明签名、约束、能力需求与组合 slot 必须显式；
- E1 build target 选择入口、direct contributions 与 provider bindings；候选 v1 再加入 package feature/bundle composition domain；
- 所有 slot、能力和错误必须闭合后才能构建可执行产物；
- lockfile 与工具链版本进入可重复构建输入；
- `loomc explain` 与检查器消费同一个 typed composition plan。删除影响仅指静态组合/依赖图中直接和传递受影响的声明、slot 与 step，不承诺预测业务输出值的语义反事实。

公共接口兼容性、增量编译和 artifact manifest 属于编译器/包管理能力，但不能改变上述语言语义。

### 9.1 大型工程组织目标

大型工程能力不是“自动发现更多代码”，而是把所有权、激活与影响面做成可治理合同：

- package owner 决定 public slot、允许的 contributor 范围与兼容性政策；
- target/profile 的变化产生可审阅的 composition-plan diff，依赖升级不能静默激活行为；
- 工具可按 package、feature/bundle、contribution、slot、capability 与 public contract 查询直接/传递影响；
- public contract、lockfile、provider binding 与构建输入可重复，CI 与本地 checker 使用同一 typed plan；
- 增量编译和并行构建只能优化该计划，不能另造语义；
- feature/bundle membership 必须先通过独立大型工程 fixture，direct target 始终是可工作的保守退路。

## 10. example、scenario 与 property

这些是语言级验证声明，不绑定特定编辑器体验：

- `example name = expr`：纯、确定、可作可执行文档；
- `scenario name { ... }`：显式 provider 与输入下执行一条行为场景；
- `property name { ... }`：在可重复生成器下检查性质。

它们由 `loomc test` 执行。E1 实现 pure example 与 explicit-provider scenario；property 在候选 v1 加入可重复生成器后实现。effectful scenario 必须显式选择 provider，并产生可报告的调用 trace；不会因保存源码自动执行世界效果。

## 11. 已确认的长期能力：目标状态调和

目标状态调和是 loom-lang 已确认的长期语言/runtime 范围，但不进入 Checkout E1；独立证据门只决定何时实现，不决定它是否属于本项目。它可以调和任何由 capability 暴露并能可靠观测的领域或系统资源，不等于把部署控制面内建进语言。

语义分解确认保留：

```text
desired state
  + durable observations -> current state
  + pure gap plan
  + capability actions
  + idempotency key and durable receipt
  -> repeated reconciliation
```

它必须满足：`desired` 与 gap `plan` 是 pure；外部 observation 经 typed boundary 验证后才能折叠为 current；action 只经 capability 执行并声明资源作用范围；执行采用 at-least-once + idempotency key + durable receipt；pending action 固定程序版本和 provider/action contract；崩溃后由持久观测继续；可证明重叠的 controller 管理域必须拒绝或显式协调；无进展或震荡进入 `Escalated`。具体声明名、表面语法、存储协议和调度算法均后置到 loom-lang 的独立实验阶段。

## 12. 明确后置的语言能力

以下能力不进入 E1，且只有真实 fixture 才能解锁：

- 用户宏、comptime 和任意语法扩展；
- 用户自定义组合代数与开放 dispatch；
- 用户自定义 trait、HKT、特化与效果多态函数值；
- algebraic effect handler 和可恢复 continuation；
- 所有权/借用或无 GC 核心；
- 跨 capability 原子事务或分布式 exactly-once；
- 运行期 footprint 锁调度；
- 通用持久化命令式调用栈；
- 目标状态调和的生产协议；
- 部署控制面。

## 13. 当前仍需在 E1 前冻结的决定

以下不改变语言定位，但会阻塞首个 parser/checker：

1. slot/contribution 的最终关键字与 typed slot 语法；
2. flow 的 Ok/Err typed lane 与 slot 调用形式；
3. E1 所需的最小类型和表达式全集；
4. package manifest 与单 package fixture 的目录约定；
5. E1 host provider、fresh scenario instance、fixture alias 与 target/scenario 绑定表示；
6. `explain` 的 canonical machine-readable schema；
7. Checkout Raw/Input -> constrained domain -> `CheckoutError.Validation` 的唯一建立路径；
8. direct target composition、slot-local transform key 和 capability `allows` 语法。
9. E1 Decimal 的字面量、scale/舍入，以及 Int/Decimal 溢出、除零和越界的 defect 诊断。
10. E1 内建结构 equality、Vec 字面量/遍历、最小 pattern 与穷尽 `match` 的 type rules。
11. Raw schema、Violation code/path/value summary、确定首错顺序和 canonical JSON。
12. host-provider descriptor manifest、adapter digest、fresh constructor、fixture alias 与 trace schema。

这些决定应先用 fixture 纸面展开，再写 parser；不能用 parser 当前最好实现的形状倒推语言。
