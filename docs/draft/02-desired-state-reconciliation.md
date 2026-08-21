# Desired-state 与持续调和

状态：**Archived Design Draft / Non-Normative / Paused**

来源：Design Baseline 0.2 与 Capability Map 0.2（Git 快照 `572f9ef`）

整理日期：2026-08-21

本文保存此前类似 Kubernetes operator 的 desired-state/reconciliation 方向。它不是当前语言规范，也不表示 `loom-lang` 已承诺提供 `operator` 关键字、专用调度器或持久 runtime。

这条路线与 [AOP-like 静态组合](01-declarative-composition-aop-like.md)彼此独立：前者讨论一次构建中的实现如何组合，本文讨论一个长时间运行的系统如何在故障、重试和状态变化中趋近目标。任一条路线失败都不应拖累另一条。

## 1. 要验证的命题

核心命题是：

> 对需要跨重启、重复观察和多次外部动作才能完成的过程，能否把“最终应该是什么”声明为 durable desired state，把差距计算保持为 pure typed plan，再由具有幂等键和持久回执的显式 action 驱动收敛，从而比一次性命令式调用链更容易恢复、解释和治理。

它不等于把 Kubernetes 或部署控制面内建进语言。订单履约、账单结算、数据复制、权限授予和基础设施资源都可能是场景，只要当前状态能够可靠观察、动作能够显式执行、结果能够持久记录。

## 2. 历史语义分解

旧方案保留的最小分解是：

```text
desired state
  + durable observations -> current state
  + pure gap plan
  + capability actions
  + idempotency key and durable receipt
  -> repeated reconciliation
```

各组成部分的责任是：

| 概念 | 责任 | 不应该承担的责任 |
|---|---|---|
| desired | 持久表达目标和约束 | 不包含隐藏 I/O 或重试循环 |
| observation | 记录外部世界的可验证事实 | 不因“看起来合理”直接冒充 current |
| current | 由已接受 observation/receipt 确定折叠出的模型 | 不依赖进程内临时对象 |
| plan | 纯地比较 desired/current，产出 Converged 或 typed actions | 不直接调用外部系统 |
| action | 通过显式外部能力改变世界 | 不绕过资源作用域和幂等合同 |
| idempotency key | 标识一次逻辑动作，允许安全重放 | 不保证外部系统天然 exactly-once |
| receipt | 持久记录尝试与结果，供恢复和 current 折叠 | 不是只写内存的 debug log |
| program basis | 固定产生 pending action 的程序和合同版本 | 不允许升级后静默换一种解释 |
| managed domain | 声明 controller 能改变的资源集合 | 不允许多个 controller 无协调重叠写入 |

## 3. 调和循环

旧方向隐含的运行循环可以写成以下协议草图：

```text
1. 读取 desired、program basis、durable observations 和 receipts
2. 在 typed boundary 验证新 observation
3. 纯、确定地折叠 current
4. 运行 pure plan(desired, current)
5. 若已满足目标，记录/报告 Converged
6. 若需要动作，先持久化 Pending(action, key, basis, contract)
7. 通过显式 action provider 执行或重放同一个 key
8. 持久化成功、失败或未知结果的 receipt
9. 重新观察并再次 plan，直到收敛、阻塞或升格
```

第 6 步必须先于外部动作，否则进程在动作完成后、写回执前崩溃时无法知道应该重放什么。即使如此也只能得到 **at-least-once delivery**；安全性来自 provider 对同一 idempotency key 的幂等合同，而不是把不可靠网络包装成 exactly-once。

一次 action 成功并不自动等于 desired 已实现。runtime 必须重新观察，或消费足以构成 current 的 durable receipt，再由下一轮 pure plan 判断是否收敛。

## 4. 纯计算与外部效果分轨

旧设计要求以下部分 pure、deterministic：

- desired 的规范化与校验；
- observation 到 current 的折叠；
- `plan(desired, current)`；
- action key 的逻辑派生；
- 收敛、无进展和震荡检测所使用的可复现状态摘要。

以下部分是显式效果：

- 从外部世界取得 observation；
- 执行 action；
- 持久化 desired、pending、receipt 和 controller status；
- 租约、时钟或通知等调度设施。

外部 observation 在经过 typed boundary 验证前不得进入 current。action 只能通过显式 provider/capability 执行，并声明它触及的资源或管理域。这里的 capability 模型并未进入当前 Core；未来也可以用常规接口达到同一分轨，不能预设必须先发明 effect system。

## 5. 类型化计划的示意

下面只展示语义形状，不是 Loom 表面语法：

```loom
record DesiredReplicaSet {
    service  ServiceId
    replicas Int
}

record CurrentReplicaSet {
    service  ServiceId
    healthy  Int
    pending  Int
}

enum ReplicaAction {
    StartReplica(ServiceId, ReplicaId)
    StopReplica(ServiceId, ReplicaId)
}

enum ReconcileDecision[A] {
    Converged
    Actions(List[A])
    Blocked(BlockReason)
    Escalated(EscalationReason)
}

fn plan(desired DesiredReplicaSet, current CurrentReplicaSet)
    ReconcileDecision[ReplicaAction]
{
    // 纯计算；不启动进程，也不读取网络。
    ...
}
```

runtime 为每个 `ReplicaAction` 产生稳定 key，将 action、key、程序 basis 和 provider contract 一起写入 pending，然后才调用外部 provider。若进程在调用后崩溃，恢复时重放相同 key；provider 必须返回既有结果或保持等价效果。

这个示例不确认 `record` 之外的任何 API、`List` 类型、decision variants、action batching 或调度顺序。它只说明 desired/current/plan/action 的责任边界。

## 6. Pending、receipt 与程序 basis

为防止长过程在升级后漂移，旧方案要求每个 pending action 固定：

- action 的规范 typed payload；
- idempotency key；
- 产生它的 desired revision；
- 程序/规则 basis；
- provider 和 action contract version；
- managed resource/domain；
- 尝试次数和 durable result/unknown 状态。

如果部署了新程序，runtime 不能用新逻辑静默重解释旧 pending。至少需要在下列策略中显式选择一种：继续用旧 basis 完成、证明安全后迁移、补偿/取消，或进入人工处理。具体选择尚未确定。

receipt 需要区分“明确失败”“明确成功”和“结果未知”。结果未知时不能随意生成新 key；应重放相同逻辑动作或先通过 observation 确认世界状态。

## 7. 管理域与多 controller 冲突

持续调和会反复写外部世界，因此 ownership 不是普通函数调用冲突。旧方案要求每个 controller 声明可管理的资源域；如果两个 active controller 的写域可证明重叠，则必须：

- 静态或启动时拒绝；或
- 通过显式协议划分字段/资源 ownership；或
- 使用一个可解释的协调 controller/仲裁合同。

“最后一次写获胜”不能成为默认组合规则。只读 observation 可以共享，但 observation 的时效、一致性和 source of truth 必须进入合同。

## 8. 收敛与升格

安全重试不等于一定收敛。旧方向要求 runtime 至少识别并报告：

- 多轮 plan 得到同一 action，但 current 没有可观察进展；
- desired/current 在有限状态之间反复震荡；
- action 永久失败或 contract 已不可用；
- observation 长期缺失、冲突或过期；
- controller 的管理域/版本发生不可自动解决的变化。

无进展或震荡不能无限静默重试，应进入结构化 `Escalated` 状态并保留 witness：相关 desired revision、current summaries、planned actions、receipts 和 basis。是否另设 `Blocked`、阈值如何配置、哪些错误允许自动恢复，均未冻结。

长期若要宣称“最终收敛”，还需要为具体 operator 给出可审查的 measure、单调性或其他领域证明；通用 runtime 本身不能证明任意 action plan 会收敛。

## 9. 可解释性与审计

对任一调和实例，工具或 runtime 应能够回答：

1. 当前 desired 是哪个 revision，谁提交的；
2. current 由哪些 observation 和 receipts 折叠而来；
3. 为什么计划产生这些 actions；
4. 每个 action 的资源域、idempotency key、provider contract 和 program basis；
5. 哪些尝试成功、失败或结果未知；
6. 为什么系统认为已经 Converged、正在 Retry、Blocked 或 Escalated；
7. 升级、desired 变更或 controller 竞争如何影响 pending 工作。

这些信息必须来自调和实际使用的 durable state，而不是另写一套可能漂移的日志解释器。敏感 action payload 和 observation 需要结构化脱敏，不能为了 explain 泄露密钥或业务隐私。

## 10. 为什么暂停

这条路线具有独立价值，但它远大于一项语法功能：

- 它需要 durable store、action protocol、幂等 provider、崩溃恢复和版本迁移，主要风险在 runtime/分布式正确性而非 parser；
- 当前尚未证明专用语言声明优于普通 typed state machine、workflow engine 或 operator SDK；
- observation consistency、unknown outcome、deletion/finalization 和多 controller ownership 都没有冻结；
- capability/effect、resource scope、provider lifecycle 尚未进入最小语言核心；
- 没有一个带故障注入和跨重启 oracle 的真实 fixture，无法用 happy-path demo 证明安全；
- 如果与静态组合同时实现，失败时无法判断是组合模型、runtime 协议还是领域 action 不幂等；
- 把通用部署控制面、调度和存储直接内建进语言会显著扩大产品边界。

因此 desired-state 先作为独立研究轨归档。未来可以首先以普通库/runtime 原型验证协议，不预设需要语言关键字，也不预设 runtime 的产品名称；`loom-machine`、`operator`、`reconcile` 都不是当前已确认概念。

## 11. 尚未解决的问题

1. desired 的身份、revision、删除和 ownership 如何表示；
2. observation 是事件、快照还是两者，如何处理迟到、重复、乱序和过期；
3. current 的 fold 如何版本化和迁移，能否从 durable facts 完整重建；
4. action key 由哪些稳定字段派生，provider 如何证明同 key 幂等；
5. action 的明确失败、未知结果和部分成功如何编码；
6. pending action 遇到新 desired revision 时继续、取消、补偿还是重规划；
7. program/provider contract 升级时如何保留或迁移旧 basis；
8. 多 action plan 是逐项提交、批量提交还是允许并行，部分完成后如何重新 plan；
9. controller 管理域如何表达，动态资源 key 如何判定重叠；
10. convergence/no-progress/oscillation 的领域 witness 如何定义；
11. retry、deadline、backpressure、fairness 和人工审批属于语言、runtime 还是部署配置；
12. secrets、PII、receipt 保留期和审计访问如何治理；
13. 什么场景确实不能由常规队列 worker、数据库状态机或成熟 workflow engine 清楚完成；
14. 哪些部分值得成为语言语义，哪些只应是一个库、SDK 或独立服务。

## 12. 重新开启的最小证据门

未来重启时，先选一个动作少、状态可完全观察、provider 真正支持幂等 key 的真实过程。不要同时引入静态 contribution、动态插件或多 controller 并行。原型至少需要：

- durable desired、observation、pending 和 receipt schema；
- pure plan 的确定性 golden；
- 正常收敛、明确失败、结果未知、重复 delivery 和 observation 延迟；
- 在“外部动作后、receipt 前”崩溃以及 receipt 后崩溃的恢复测试；
- 同一 idempotency key 重放不产生重复业务效果；
- 程序升级期间存在旧 pending 的 basis 测试；
- desired 在 pending 期间变化的固定策略；
- no-progress 和 oscillation 的 Escalated witness；
- 与一个惯用 state machine/workflow engine 基线的正确性、恢复复杂度和可理解性对照。

只有协议在跨重启和故障注入下成立，并且声明式 desired/pure plan 对真实维护有清楚净收益，才讨论专用语法、调度器、语言级 capability 或更复杂的 controller composition。

## 13. 本草案明确不决定

- 不确认 desired-state 必须属于 `loom-lang` 语言本体；
- 不确认 `operator`、`reconcile`、`machine` 或任何旧关键字；
- 不承诺 exactly-once、自动事务或任意 controller 都能收敛；
- 不把部署、集群租约、灾备或通用 workflow engine 全部纳入语言；
- 不要求 AOP-like 静态组合先存在；
- 不把这条路线与 live programming、AST 编辑或专用 workbench 绑定。
