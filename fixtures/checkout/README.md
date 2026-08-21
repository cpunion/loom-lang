# Checkout fixture 设计

状态：Design Protocol 0.2 / 尚无实现

对应实验：[第一项 checkout 对照实验](../../docs/01-first-experiment.md)

边界依据：[loom-lang 项目章程](../../docs/00-charter.md)

本目录当前只冻结 fixture 的公平性与可观察契约。E0 不放置无法运行的 `.loom`、TypeScript 空壳或伪生成产物。

## 1. 领域模型

两种实现必须表达同一组概念：

| 概念 | 最低语义 |
|---|---|
| `CheckoutInput` / `RawCartLine` | 无约束 transport 数据，允许 oracle 表达空 cart、负数量和非法金额 |
| `Cart` / `CartLine` | 至少一行；数量为正；商品单价非负 |
| `Money` | 十进制定点或等价精确表示；最终金额非负 |
| `Customer` | 普通/VIP；带 region 与可选 VAT identity |
| `CheckoutRequest` | 只由 `CheckoutRequest.from(raw)` 建立的恒有效 cart、customer、operation id |
| `Order` | 价格明细、授权结果、持久化标识 |
| `CheckoutError` | Validation、Pricing、Tax、PreAuthorization、Authorization、RejectionHandling、Persistence 的封闭结果 |

raw schema 固定为：`CheckoutInput { cart RawCart, customer RawCustomer, operation_id Text }`、`RawCart { lines Vec[RawCartLine] }`、`RawCartLine { sku Text, quantity Int, unit_price Decimal }`、`RawCustomer { tier CustomerTier, region Region, vat_identity Text? }`。领域侧 `VatIdentity = Text where len(self) >= 8 && len(self) <= 12`，`OperationId = Text where len(self) >= 1 && len(self) <= 64`，`Customer.vat_identity` 为 `VatIdentity?`。

非法输入只进入 `CheckoutRequest.from(raw)`；任一 Violation 固定变成 `CheckoutError.Validation(violation)`，随后 flow 不启动且 capability trace 为空。多项同时非法时返回确定的第一项：empty cart，随后按 line index 检查 quantity、unit price，再检查 VAT identity、EU presence invariant、operation id；该顺序属于 fixture oracle，不依赖 record 字段排列。

T1 只新增 `CheckoutRequest` 的领域 invariant：EU customer 必须拥有 `Some(VatIdentity)`。`CheckoutRequest.from` 统一触发既有 `VatIdentity` constraint 与新增 invariant 并映射 Violation；参与者不得在 flow 或调用点另写一份 VAT 条件。

初始行为：

```text
validate raw input into CheckoutRequest
-> calculate base price
-> run pricing pipeline (initial ProductPromotions; optional VipDiscount)
-> calculate tax
-> run pre-authorization slot
-> authorize final amount
-> on authorization rejection, run authorization-rejected slot
-> persist once by operation id
-> return order
```

三个 slot 都由基础 checkout 预先声明 typed 合同：

| slot | 输入 / 输出 | 作用 |
|---|---|---|
| `pricing` | `PricedCart -> Result[PricedCart, PricingError]` | base price 之后、税费之前追加定价变换；不允许 capability |
| `pre_authorization` | `AuthorizationContext -> Result[AuthorizationContext, PreAuthorizationError]` | 授权之前执行检查；只允许 `risk Risk` |
| `authorization_rejected` | `AuthorizationRejectedContext -> Result[AuthorizationRejectedContext, RejectionHandlingError]` | 只消费 Authorization rejection；只允许 `audit AuditSink` |

三个 slot 都采用 E1 唯一组合规则 `pipeline[S, E]`：空槽返回 `Ok(input)`；每项 contribution 恰好提供一个带 slot-local key 的 `S -> Result[S, E]` transform；多个 active transforms 必须由 key 上的 `before` / `after` 形成全序，依次传递 context，并在首个 `Err` 停止。checker 先在 target-visible、同一 slot 的全部 declared transforms（active 或 inactive）中无条件检查 key 唯一并解析 anchor；顺序边只有两端 active 时才进入计划，从未声明的 key 才是 unknown anchor。contribution 不能引用 slot 外节点、扩展 `E` 或使用 `allows` 之外的 capability。

slot errors 在任务发布前封闭：`PricingError = InvalidAmount(Violation)`；`PreAuthorizationError = RiskRejected(RiskReason) | RiskUnavailable(RiskError)`；`RejectionHandlingError = AuditFailed(AuditError)`。新增 contribution 只能返回这些既有 variants；若真实需求需要新错误，必须由 slot owner 修改合同，并作为中央 API 变化计量。

初始 target 只启用 `ProductPromotions`。机制门另外执行 pricing 的 empty、ProductPromotions(A)、VipDiscount(B)、A+B 四个 subset；A+B 必须明确 `vip_discount after product_promotions`。依赖/import 不激活 contribution，E1 target 直接 compose fully-qualified contribution。

AuditSink 存在但只记录协议要求的事件，不能用它作为任意 debug log。

## 2. 能力 stub

base flow 预声明 `authorization Authorization`、`risk Risk`、`tax Tax`、`orders OrderStore`、`audit AuditSink` 五个 capability 槽。E1 禁止 contribution 新增槽；target/scenario 必须按槽名绑定一个具名 host provider，缺失、重复或类型错误都拒绝。

所有外部作用均由确定性内存 provider 提供，并共享一个 per-scenario recorder。recorder 在每次 capability 调用开始/结束时分配单调 event index，记录 provider slot、operation、canonical argument/result summary 与 outcome；这条全局 canonical event stream 是跨 provider 顺序 oracle，不能由五份局部日志事后拼接：

- `Authorization.authorize(request)`：按冻结表返回允许或拒绝，并记录调用；
- `Risk.assess(request)`：按金额阈值和固定客户表返回结果；
- `OrderStore.save(order, operation_id)`：相同 operation id 幂等，记录首次调用；
- `AuditSink.append(event)`：按冻结表成功或返回 `AuditError`，保留有序尝试/成功事件；
- `Tax.rate(region)`：从固定表读取，无网络和时间依赖。

五个 capability 的 E1 操作合同固定如下：

| capability | 操作 |
|---|---|
| `Authorization` | `authorize(AuthRequest) Result[Permit, AuthorizationFailure]`；`AuthorizationFailure = Rejected(AuthorizationReason) | Unavailable(AuthorizationCallError)` |
| `Risk` | `assess(RiskRequest) Result[RiskDecision, RiskError]`；`RiskDecision = Allowed | Rejected(RiskReason)` |
| `Tax` | `rate(Region) Result[TaxRate, TaxError]` |
| `OrderStore` | `save(Order, OperationId) Result[StoredOrder, SaveError]`，`StoredOrder.value` 为 `Order` |
| `AuditSink` | `append(AuditEvent) Result[Unit, AuditError]` |

host provider 不是测试进程里的隐藏约定。fixture 随源码版本化一个 provider descriptor manifest；每项至少包含 qualified provider name、implemented capability、operation signature、fresh constructor、adapter version/digest 与 trace schema version。checker 依据 descriptor 校验 target/scenario binding，test runner 只加载该 descriptor 锁定的 adapter。manifest 与 adapter digest 都属于显式构建/测试输入。

每个 scenario 创建 fresh provider instances 和 fresh recorder；没有类型级全局状态。provider 的输入、输出和 trace 使用语言无关的 canonical JSON 表示。两种实现必须通过同一 trace oracle；运行时性能不在本实验范围。

错误映射固定如下，E1 不提供隐式 `From` 或开放 error union：

| 来源 | CheckoutError |
|---|---|
| `CheckoutRequest.from` Violation | `Validation(violation)` |
| pricing pipeline | `PricingFailed(error)` |
| Tax | `TaxFailed(error)` |
| pre-authorization pipeline | `PreAuthorizationFailed(error)` |
| Authorization provider call failure | `AuthorizationUnavailable(error)` |
| Authorization business rejection | `AuthorizationFailed(reason)` |
| rejection handler failure | `RejectionHandlingFailed { authorization, handling }`，保留原 Authorization reason |
| OrderStore | `PersistenceFailed(error)` |

因此顶层联合固定为 `CheckoutError = Validation(Violation) | PricingFailed(PricingError) | TaxFailed(TaxError) | PreAuthorizationFailed(PreAuthorizationError) | AuthorizationUnavailable(AuthorizationCallError) | AuthorizationFailed(AuthorizationReason) | RejectionHandlingFailed { authorization AuthorizationReason, handling RejectionHandlingError } | PersistenceFailed(SaveError)`。底层 provider error/reason 的具体 variants 由下一份 E1 executable contract 与 host descriptor 一起冻结，贡献不得扩展它们。

## 3. Oracle 分层

### 3.1 行为 oracle

至少覆盖：

- 空购物车、零/负数量和非法金额被拒绝；
- 普通与 VIP、US 与 EU 的价格明细；
- VAT identity 缺失/非法；
- 风险通过/拒绝；
- 授权通过、业务拒绝与 provider call failure；
- rejection audit failure，最终同时保留 Authorization 与 handling error；
- persistence failure；
- 相同 operation id 的重复请求不会重复创建订单。

### 3.2 顺序 oracle

capability trace 必须验证，而非只看最终值：

```text
validation failure -> no risk, auth, audit, persist
risk rejection -> risk before auth; no auth or persist
authorization call failure -> auth attempt; no rejection audit or persist
authorization rejection -> auth before rejection audit; no persist
rejection audit failure -> auth, audit attempt; RejectionHandlingFailed; no persist
success -> tax before risk/auth; persist after auth
```

ProductPromotions/VipDiscount 是 pure transforms，不为测试污染生产语义；它们的“promotion -> VIP -> tax”顺序由 typed composition plan、结果 golden 与 mutation oracle 共同验证，不伪造成 capability trace event。

### 3.3 解释 oracle

对于每个最终步骤、约束和价格规则，保存：

- 稳定的逻辑名称；
- 源贡献名称；
- 显式目标；
- 前置数据依赖；
- 效果或业务顺序边；
- 删除该来源后的预期影响集合。

静态 plan 同时含 Ok/Err branches；一次 trace 必须是其中唯一 typed path 的 refinement，而不是与整张 plan 逐项相等。已走 path 上的 capability 调用顺序必须一致，未走 branch 不得出现在 trace。

候选 `explain` 输出可有自己的呈现格式，但必须无损映射到这份 oracle。传统基线由静态源码/wiring 回答同一问题。

“删除影响”仅指静态依赖/组合图中直接和传递受影响的声明、slot 与 step，不要求工具预测业务输出的反事实值。

### 3.4 Mutation oracle

至少引入以下 mutation，确认测试不会只覆盖 happy path：

- 把 VAT 检查移到授权之后；
- 把 VIP 折扣移到税后；
- 授权拒绝时漏写 audit；
- 把 Authorization provider call failure 错当业务拒绝并写 audit；
- audit failure 丢失原 authorization error 或错误地继续 persist；
- validation failure 仍写 audit；
- persistence 位于 authorization 之前；
- 同 operation id 重复持久化；
- 倒置显式顺序边；
- 删除 ProductPromotions/VipDiscount 的排序边；
- 忽略一项贡献。

两组必须杀死相同 mutation 集。

## 4. 传统基线设计约束

正式基线预计采用如下结构，但可由外部审阅者调整：

```text
baseline-typescript/
  domain/
  pricing/
  policies/
  checkout/
  capabilities/
  tests/
```

允许集中 composition root、typed pipeline、策略数组或 middleware，只要：

- 这是 TypeScript 工程中的合理常规方案；
- 所有行为仍显式可查；
- 没有为增加 Loom 的优势而制造重复或脆弱结构；
- 不预先实现一个与候选语言完全相同的定制 compiler/DSL。

基线必须为 pricing、pre-authorization 与 authorization-rejected 提供与候选方案等价的 typed extension points，通过一个明确 composition root 激活策略/handler，并为五个 capability 槽使用 fresh 内存 fixtures。这样任务不会因为候选提前知道扩展点而获得不公平优势。

另有一个 held-out boundary probe 落在没有 extension point 的位置；两组正确做法都是修改 owner contract。该 probe 不计效率，只验证候选不能越过未开放目标。

基线可以重构。任务时间和修改位置包含为安全完成重构所需的工作。

## 5. 候选设计约束

以下是 E1 的 **canonical typed graph**。它冻结类型与控制语义，但不冻结最终关键字。辅助函数合同也属于该图：

| helper | signature / guarantee |
|---|---|
| `CheckoutRequest.from` | `CheckoutInput -> Result[CheckoutRequest, Violation]` |
| `calculate_base_price` | `CheckoutRequest -> PricedCart`；结果保留完整 constrained request facts，并增加价格明细 |
| `apply_tax` | `(PricedCart, TaxRate) -> Result[PricedCart, PricingError]` |
| `make_authorization_context` | `PricedCart -> AuthorizationContext` |
| `make_auth_request` | `AuthorizationContext -> AuthRequest` |
| `make_rejection_context` | `(OperationId, AuthorizationReason) -> AuthorizationRejectedContext` |
| `build_order` | `(PricedCart, Permit) -> Order`；在已约束输入上 total、infallible |

`PricedCart` 至少包含原 `CheckoutRequest`、逐行价格明细、subtotal、discount 与可选 tax；因此 pricing 可读 customer tier、Tax 可读 region、后续步骤可读 operation id。`AuthorizationContext` 保留产生授权请求所需的 request/price facts；`AuthorizationRejectedContext` 保留 operation id 与原 `AuthorizationReason`。`Order.id` 固定等于 request operation id，`OrderStore` 只幂等持久化，不分配新领域 ID。具体 record 字段将在 E1 executable contract 中逐项冻结，但不得改变下列数据依赖和错误映射：

```loom
fn checkout(raw CheckoutInput) Result[Order, CheckoutError] {
    match CheckoutRequest.from(raw) {
        Ok(request) => Checkout(request)
        Err(violation) => Err(Validation(violation))
    }
}

flow Checkout(request CheckoutRequest) Result[Order, CheckoutError]
    uses authorization Authorization,
         risk Risk,
         tax Tax,
         orders OrderStore,
         audit AuditSink
{
    slot pricing pipeline[PricedCart, PricingError]
    slot pre_authorization pipeline[AuthorizationContext, PreAuthorizationError]
        allows risk
    slot authorization_rejected
        pipeline[AuthorizationRejectedContext, RejectionHandlingError]
        allows audit

    step base_price = calculate_base_price(request)
    step priced = match pricing.run(base_price) {
        Ok(value) => value
        Err(error) => result Err(PricingFailed(error))
    }
    step tax_rate = match tax.rate(priced.request.customer.region) {
        Ok(value) => value
        Err(error) => result Err(TaxFailed(error))
    }
    step taxed = match apply_tax(priced, tax_rate) {
        Ok(value) => value
        Err(error) => result Err(PricingFailed(error))
    }
    step checked = match pre_authorization.run(make_authorization_context(taxed)) {
        Ok(value) => value
        Err(error) => result Err(PreAuthorizationFailed(error))
    }
    step authorization_attempt = authorization.authorize(make_auth_request(checked))

    on authorization_attempt.Err(failure) {
        match failure {
            Unavailable(error) => result Err(AuthorizationUnavailable(error))
            Rejected(reason) => {
                let context = make_rejection_context(request.operation_id, reason)
                match authorization_rejected.run(context) {
                    Ok(_) => result Err(AuthorizationFailed(reason))
                    Err(handling) => result Err(RejectionHandlingFailed {
                        authorization = reason
                        handling = handling
                    })
                }
            }
        }
    }

    on authorization_attempt.Ok(permit) {
        step order = build_order(taxed, permit)
        match orders.save(order, request.operation_id) {
            Ok(stored) => result Ok(stored.value)
            Err(error) => result Err(PersistenceFailed(error))
        }
    }
}

contribution ProductPromotions to Checkout.pricing {
    transform product_promotions
        (input PricedCart) Result[PricedCart, PricingError] = ...
}

contribution VipDiscount to Checkout.pricing {
    transform vip_discount after product_promotions
        (input PricedCart) Result[PricedCart, PricingError] = ...
}

contribution HighValueRisk to Checkout.pre_authorization {
    transform high_value_risk(input AuthorizationContext)
        Result[AuthorizationContext, PreAuthorizationError]
        uses risk Risk = ...
}

contribution AuditAuthorizationRejection to Checkout.authorization_rejected {
    transform rejection_audit(input AuthorizationRejectedContext)
        Result[AuthorizationRejectedContext, RejectionHandlingError]
        uses audit AuditSink = ...
}
```

完整任务态 build target 直接激活 contribution，并绑定全部预声明 capability 槽；初始态只 compose `ProductPromotions`：

```text
target checkout_experiment {
    entry shop.checkout.checkout
    compose shop.checkout.ProductPromotions
    compose shop.checkout.VipDiscount
    compose shop.checkout.HighValueRisk
    compose shop.checkout.AuditAuthorizationRejection

    bind authorization = shop.test.FixedAuthorization
    bind risk = shop.test.FixedRisk
    bind tax = shop.test.FixedTax
    bind orders = shop.test.MemoryOrderStore
    bind audit = shop.test.RecordingAuditSink
}
```

无论最终语法如何，语义必须满足：

1. `VipDiscount` 命名唯一目标 `Checkout.pricing`，不能使用名称模式寻找多个目标；
2. 每个 transform key、输入输出、closed error、capability uses 和顺序边进入编译计划；
3. 目标未开放该贡献类型时静态拒绝；
4. 贡献与基础声明在解释输出中地位对等、来源可定位；
5. 文件排列、导入遍历和注册时机不改变计划；
6. contribution 只有被 direct target 选择才生效，依赖/import 不会隐式激活；
7. 删除 contribution 后，检查器能给出其静态直接和传递影响集合；
8. pricing empty/A/B/A+B、首错停止和 error mapping 均有 golden；
9. 无法闭合的歧义不通过“最后一个获胜”解决。

如果真实 fixture 迫使我们退回隐藏注入、全局中央列表或无法解释的自动顺序，应按实验 kill criteria 停止，而不是给语法继续加特殊规则。

## 6. 计划中的目录（E1 才创建）

```text
fixtures/checkout/
  oracle/                 # canonical cases、trace、mutation 与 explanation oracle
  baseline-typescript/    # 冻结的惯用基线
  candidate-loom/         # 只含实验所需的最小语言用例
  tasks/a/                # A 版任务包和起始 patch
  tasks/b/                # 等价 B 版
  scoring/                # 自动正确性与人工理解评分定义
```

创建顺序必须是 oracle -> external baseline review -> candidate slice。不能先按已实现语法降低 oracle 要求。

## 7. Fixture 接受条件

进入实现前必须满足：

- 领域专家能从本文和 oracle 判断每个结果、错误和顺序；
- TypeScript 审阅者确认基线不是稻草人；
- 两组拥有等价的 typed extension points 和显式 composition root；
- raw input、closed CheckoutError/slot errors、provider lifecycle 与 trace schema 均已冻结；
- pricing empty/A/B/A+B subset matrix 和 handler failure case 全绿；
- A/B 任务难度经 pilot 无明显偏斜；
- 行为、trace、mutation 与解释四层评分均可重复；
- 评分只读取冻结源码、构建/测试结果、执行 trace 与参与者答案；
- 在一个全新的普通 Git clone 中可以执行完整实验。
