# 声明式静态组合与 AOP-like 扩展

状态：**Archived Design Draft / Non-Normative / Paused**

来源：Design Baseline 0.2 与 Checkout Protocol 0.2（Git 快照 `572f9ef`）

整理日期：2026-08-21

本文保存此前“声明、约束、显式贡献”路线中尚值得继续验证的静态组合假设。它不是当前语言规范，不保留任何关键字，也不要求 Core 0.1 实现这里的机制。

“AOP-like”只用于指出它和横切关注点组合的相似性，不是语言中的正式概念。旧讨论中的 `weave` 不再作为产品或语义术语；这里也不采用传统 AOP 的 pointcut、调用栈拦截或按名称模式注入。

## 1. 要验证的命题

这条路线的核心命题是：

> 当一个声明的 owner 显式开放少量有类型的扩展位置，其他模块能否以具名、定向的贡献扩展它，并由编译器生成确定、可检查、可解释的组合计划，从而让跨模块规则比普通 composition root、middleware 或策略数组更局部、更不易遗漏。

“声明式”在这里不是把所有算法改写成 DSL，而是把稳定合同和模块间关系从命令式 wiring 中提出来：

| 层 | 回答的问题 | 旧方案中的例子 |
|---|---|---|
| 声明/合同 | 什么形状、输入、输出、失败和扩展是合法的？ | 类型、函数签名、具名 slot、closed error |
| 实现 | 具体如何计算？ | 普通函数体、typed transform、具名 contribution |
| 运行状态 | 本次执行和外部世界是什么？ | record 值、provider instance、外部资源 |
| 静态组合 | 哪些实现进入哪个合同，为什么以当前顺序出现？ | target 选择 contribution 后得到的 typed plan |

普通 `fn` 仍按源码顺序表达单一 owner 的局部算法。只有确有多个独立来源需要向同一个稳定合同贡献行为时，才考虑开放组合。

## 2. 它和传统 AOP 的边界

| 传统 AOP 概念 | 旧静态组合方案 | 约束目的 |
|---|---|---|
| join point | 目标 owner 明确声明的具名 typed slot | 未开放的位置绝不能扩展 |
| pointcut | contribution 对唯一限定 slot 的精确引用 | 不按函数名、annotation、调用栈或源码模式匹配 |
| advice | 有名字和源码位置的 typed contribution | 输入、输出、错误和依赖均静态可见 |
| precedence | 数据依赖或显式 `before` / `after` 边 | 文件顺序和注册时机没有语义 |
| weaving | build 时闭合出 typed composition plan | 不在运行期扫描或猴子补丁 |
| enablement | build target 的显式选择 | import 或普通 dependency 不激活行为 |

因此，更准确的描述是 **owner-controlled static composition**。AOP-like 的价值只在于它能表达审计、授权前检查、价格规则等横向贡献；它不获得任意包围函数、改写返回值或截获异常的权限。

## 3. 统一 slot 模型

旧方案要求目标先公开一个 slot。一个完整 slot 合同至少包含：

- 唯一限定名及 owner；
- 接受的 contribution 形状；
- typed input、output 和封闭错误类型；
- 使用的组合代数；
- contribution 允许消费的外部能力或效果上界；
- 哪些 package/module 可以贡献；
- duplicate、missing、unordered 和 cycle 等失败的确定诊断。

每项 contribution 则必须包含：

- 自己的限定名和源码位置；
- 唯一、精确的目标 slot；
- 它提供的 member/transform key；
- typed 输入、输出和错误；
- 它消费的数据或外部能力；
- 需要业务顺序时的显式边；
- 进入最终计划时可被工具追溯的来源信息。

目标未公开 slot 时必须静态失败。依赖中存在 contribution、import 了它、或它的名字恰好匹配某个函数，都不能使它生效。

## 4. 分开验证的组合代数

旧方案只考虑少量内建代数，不允许用户任意定义“如何组合”，因为任意组合函数会使来源、冲突和顺序再次变得不可解释。

| 候选 slot | 用途 | 闭合规则 | 历史成熟度 |
|---|---|---|---|
| ordered pipeline | 定价、审批、导入、检查链 | 同型 transform，显式全序，首个错误停止 | Checkout 草案最完整 |
| keyed members | 注册项、命名 handler/provider | key 唯一，重复即错误 | 仅方向草案 |
| rules | eligibility、allow/deny、集合约束 | 只能用语言内建且可证明交换的 fold | 仅方向草案 |

字段扩展、开放 method、隐式 provider 选择、用户自定义代数和开放多重/模式 dispatch 都没有得到旧方案授权。即使 ordered pipeline 实验成功，也不能自动证明其他代数合理。

当前已经确认的 `concept`/`dyn concept` 与本方案不同：

- `impl C for T` 只证明名义类型 `T` 满足接口 `C`，每个 `(T, C)` 只有一个 conformance；
- dyn carrier 只调用显式 construction 时封装的 concrete witness；
- import 不激活 impl，运行时也不扫描候选；
- concept 不允许多个来源向同一个执行点贡献、排序或编织 member。

因此 concept/dyn 可以作为未来组合实验的普通 typed interface 和显式依赖传递基线，但不能替代 slot/contribution 假设本身，也不提供 capability、effect 或 provider 生命周期。

## 5. Ordered pipeline 的历史语义草图

历史上最具体的切片是：

```text
pipeline[S, E]
empty                          = Ok(input)
one active contribution       = S -> Result[S, E]
multiple active contributions = explicit total order; output feeds next input
```

其规则是：

1. `E` 由 slot owner 定义并封闭；contribution 只能返回精确的 `Result[S, E]`，不能私自扩展错误联合；
2. 每项 contribution 恰好提供一个 slot-local transform key；
3. checker 先在同一 target 可见的 declared transforms 中建立 key catalog，key 必须无条件唯一，anchor 必须能解析到唯一 catalog entry；
4. `before` / `after` 只有在两端都 active 时进入执行计划，因此一个可选 contribution 单独启用仍可合法；引用 catalog 中不存在的 key 是 unknown anchor；
5. 所有 active transforms 必须形成唯一全序；未定序 pair、duplicate key、unknown anchor 和 cycle 都 fail closed；
6. pipeline 按顺序传递 context，在首个 `Err` 停止；empty slot 是 `Ok(input)`；
7. slot 在 owner 算法中只有一个明确调用位置，contribution 不能引用 slot 外节点、跳转到其他结果 lane 或包围任意函数。

这里要求全序是首个实验的保守选择，不代表长期认为一切都应串行。要允许部分序、并行或自动交换，必须先有单独的 purity、effect footprint、totality 与 commutativity 证据；不能使用文件名或稳定排序 key 冒充业务顺序。

## 6. `fn`、开放流程和 typed outcome

旧方案区分：

- 普通函数：单一 owner 的局部有序算法；
- 开放流程：owner 显式暴露具名阶段，多个来源可以贡献；
- typed outcome lane：成功、业务拒绝和基础设施错误都以封闭类型出现在计划中，贡献不能截获未声明的路径。

以下只保存旧表面示意，**不是 grammar**：

```loom
flow Checkout(request CheckoutRequest) Result[Order, CheckoutError]
    uses authorization Authorization,
         risk Risk,
         audit AuditSink
{
    slot pricing pipeline[PricedCart, PricingError]
    slot pre_authorization
        pipeline[AuthorizationContext, PreAuthorizationError]
        allows risk
    slot authorization_rejected
        pipeline[AuthorizationRejectedContext, RejectionHandlingError]
        allows audit

    step base_price = calculate_base_price(request)
    step priced = match pricing.run(base_price) {
        Ok(value) => value
        Err(error) => result Err(PricingFailed(error))
    }

    // 其余基础步骤仍由 owner 明确书写。
}
```

与之对应的定向贡献示意为：

```loom
contribution VipDiscount to Checkout.pricing {
    transform vip_discount after product_promotions
        (input PricedCart) Result[PricedCart, PricingError]
    {
        apply_vip_discount(input)
    }
}

contribution HighValueRisk to Checkout.pre_authorization {
    transform high_value_risk(input AuthorizationContext)
        Result[AuthorizationContext, PreAuthorizationError]
        uses risk Risk
    {
        match risk.assess(make_risk_request(input)) {
            Ok(Allowed) => Ok(input)
            Ok(Rejected(reason)) => Err(RiskRejected(reason))
            Err(error) => Err(RiskUnavailable(error))
        }
    }
}
```

这个例子只表达三项语义意图：扩展位置由 `Checkout` owner 预先开放；贡献指向唯一 slot；贡献的顺序、错误和外部依赖进入静态计划。`flow`、`slot`、`contribution`、`transform`、`allows`、`uses` 和 `result` 均未被当前语言保留。

## 7. 激活与大型工程组织

旧方案的最小激活方式是由 build target 直接列出完全限定的 contribution：

```text
target checkout_test
  entry shop.checkout.checkout
  compose shop.checkout.ProductPromotions
  compose shop.checkout.VipDiscount
  compose shop.checkout.HighValueRisk
  bind authorization = shop.test.FixedAuthorization
  bind risk = shop.test.FixedRisk
  bind audit = shop.test.RecordingAuditSink
```

这同样不是 manifest 语法。它保存的规则是：

- import 只影响名字可见性，不激活行为；
- dependency 只使声明可用，不激活行为；
- 每条生效路径都能解释为 `target -> contribution -> slot`；
- target 中重复列出、目标不存在或 provider binding 不闭合都静态失败；
- 添加和删除 contribution 应产生可审阅的 composition-plan diff。

旧方案还讨论过 package feature/bundle：target 只启用一个具名 composition domain，成员关系由贡献方声明，再沿 `target -> enabled bundle -> contribution -> slot` 解释，以减少大型工程中央清单。它从未经过真实大型仓库验证，尤其没有解决 bundle membership 升级、跨 package ownership、兼容性和供应链审阅，因此只能保留为开放方向，不能成为首个实现的默认激活模型。

## 8. 外部能力和效果边界

旧组合方案依赖另一个尚未进入 Core 的假设：外部效果通过具名 capability slot 暴露，target 为每个 slot 绑定唯一 provider instance。为了避免 contribution 在组合时偷偷扩大权限，曾设想：

- owner 预声明开放流程可能使用的 capability 集；
- 每个 slot 再声明 contribution 的 `allows` 上界；
- contribution 的实际 `uses` 必须是这两个上界的子集；
- inactive contribution 不调用相应 capability，但也不能使 target 的合同发生不可见变化；
- 时钟、随机、网络、文件和数据库都不得来自 ambient global。

这套 capability/provider 模型与静态组合有关，但不是同一个问题。未来组合实验应先判断在只有普通静态 concept、显式 dyn carrier、函数参数和 composition root 时，专用 slot 是否仍有独立价值；不能为了验证 slot 而一次实现完整 effect system、provider runtime 和 FFI。

## 9. 可解释性合同

旧方案把 explain 视为组合语义的一部分，而不是额外生成的近似文档。工具应从实际执行所用的同一 typed plan 回答：

1. 最终有哪些成员、规则或步骤；
2. 每一项来自哪个限定声明和源码位置；
3. 为什么它适用于当前目标；
4. 为什么它位于当前顺序；
5. 哪条激活路径让它生效；
6. 组合失败时，哪个稳定 witness 证明 duplicate、missing、unordered 或 cycle；
7. 删除某项贡献会使静态组合图中的哪些声明、slot 和 step 失效或改变。

解释结果必须不受文件排列、目录遍历、import 顺序和注册时机影响。若执行计划无法唯一回答来源和顺序，该程序就不应被接受。

## 10. 为什么暂停

这条路线不是因为已经被证伪而暂停，而是因为旧实验同时要求太多未经验证的层：

- Core 语言本身尚无 parser/checker，无法区分基础工具成本和组合机制收益；
- `flow`、typed lane、slot、contribution、target、capability/provider 和 explain schema 高度耦合；
- Checkout 只证明“owner 已预见并开放 extension point”的场景，不能证明任意横切变化都更局部；
- 传统语言的函数组合、middleware、策略数组和 typed composition root 是很强的公平基线；尚无数据证明专用语言机制更高效；
- 同一能力可能通过普通库、编译期声明处理器或较小的静态链接机制实现，没有理由先冻结一组大语法；
- package bundle、影响查询和 effect footprint 属于大型工程与效果系统问题，不能由一个 checkout demo 顺带确认；
- 若把所有潜在变化都预先开成 slot，owner 合同会膨胀；若 slot 太少，又无法兑现局部扩展价值。

因此当前先验证最小静态语言与契约核心。组合路线以后应作为可独立失败的实验，不再作为语言成立的前提。

## 11. 尚未解决的问题

重新讨论前至少要回答：

1. 哪一个真实变化必须跨多个 owner，普通函数/模块/composition root 为什么不够；
2. join point 的最小形状是 pipeline slot，还是普通函数合同上的更小静态扩展点；
3. slot 是 public API 吗，增加、删除、改类型如何版本化；
4. contribution 的激活应在源码、package manifest 还是 build target，谁拥有最终决定权；
5. 可选 contribution 的 anchor 如何在不同 feature subset 下保持稳定；
6. closed error 是否会迫使 owner 为所有未来 contribution 预留错误；开放错误又如何保持穷尽和兼容；
7. 顺序缺失何时是错误，何时可以用可证明的无序/交换；
8. contribution 是否能引入新的依赖或 effect；如果不能，owner 要预见到什么程度；
9. package/bundle 如何避免依赖升级静默激活行为；
10. explain 和 plan diff 的稳定 machine schema 是什么；
11. 同一机制是否真的比显式 typed list 更容易导航、重构、测试和审阅；
12. 哪些能力应是语言语义，哪些应只是构建工具或库约定。

## 12. 重新开启的最小实验门槛

未来若重启，建议只选择一个 owner、一个 ordered pipeline slot 和两个独立贡献，不同时引入 provider runtime、bundle、policy fold 或并行 DAG。实验至少应具有：

- 一个惯用且经过外部审阅、可以使用静态 concept 与显式 dyn carrier 的常规语言 composition-root 基线；
- 相同的行为和 mutation oracle；
- empty/A/B/A+B 四个组合子集；
- duplicate key、unknown anchor、unordered pair、cycle 和 missing target 的稳定诊断；
- 来源、激活路径、顺序理由和删除影响的 explain golden；
- 固定的小型变更任务，测量修改散点、遗漏、完成时间和理解正确率；
- 一个“owner 没有开放 slot”的 held-out 需求，验证系统会要求修改 owner，而不是退化成隐式注入。

只有这项最小实验显示专用静态机制相对普通 typed composition 有清楚净收益，才讨论语法、更多组合代数、capability/effect closure 或大型工程激活。

## 13. 本草案明确不决定

- 不决定当前语言需要 AOP、`flow`、slot 或 contribution；
- 不决定使用旧示例中的任何关键字；
- 不允许 pointcut、名称模式、annotation 扫描、调用栈匹配或任意函数包围；
- 不确认 package bundle、effect footprint、field extension 或 policy fold；
- 不确认“静态组合一定提高开发效率”；
- 不把这条路线与 desired-state runtime、live programming 或 AST 编辑绑定。
