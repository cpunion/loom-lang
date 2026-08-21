# loom-lang 表面与代码风格

状态：Surface Direction 0.2

证据等级：E0

日期：2026-08-21

本文确认代码阅读风格和已经稳定的表面规则。组合 slot、typed error lane 等仍标为示意的部分，必须在 E1 parser 开工前另行冻结 grammar。

## 1. 风格目标

loom-lang 的日常代码应当像一门克制的现代静态语言：

- 文本优先，适合顺序阅读和普通 diff；
- 大括号表达结构，缩进只表达排版；
- 低标点、少关键字、相同概念只有一种常用写法；
- 普通函数保持熟悉的命令式/表达式混合风格；
- 声明式只用于真实的约束、依赖、组合和目标，不把所有代码改写成 DSL；
- 公共合同显式，局部细节可推断；
- formatter 统一风格，但不以字母排序破坏作者的阅读组织。

## 2. 文件与恢复边界

- 源文件使用 UTF-8；
- 每个文件有一个 `module` 声明，同一 module 可由多个文件共同组成；
- 文件名和目录用于组织与项目约定，不参与声明语义；
- 顶层声明以具名关键字开始，声明之间无需分号；
- `{}`、`()`、`[]` 是结构边界；缩进不参与 parse；
- 普通字符串不能跨未转义换行，未闭合字符串在行末形成局部词法错误；多行文本以后使用显式多行分隔符；
- `module`、`import`、`fn`、`type`、`record`、`entity`、`enum`、`capability`、`flow`、`policy`、`contribution`、`boundary`、`example`、`scenario`、`property` 是保留的顶层起始词；
- nested declaration 不合法，因此 parser 在一个声明已经出错后，只要在字符串/注释之外、换行后的 statement boundary 遇到顶层起始词，就结束当前 error island 并尝试新声明；该同步不依赖缩进，也不要求错误声明的 `{`、`(`、`[` 已经闭合；
- 恢复只让后续声明继续获得诊断与类型信息，不会把损坏程序标成可构建；一个损坏声明不得吞掉后续能够独立识别的顶层声明；
- formatter 使用 4 空格缩进，不输出分号。

注释使用 `//`，文档注释使用 `///` 并附着下一条声明。

## 3. 命名与可见性

| 类别 | 风格 | 示例 |
|---|---|---|
| package / module / feature | 小写点分或 snake_case | `shop.checkout`、`vip_pricing` |
| 类型 / capability / enum 变体 | PascalCase | `Money`、`OrderStore`、`Rejected` |
| 函数 / 字段 / 局部变量 / slot | snake_case | `calculate_total`、`authorization_rejected` |
| 常量 | snake_case | `default_timeout` |

可见性前缀为 `pub`；package 内可见和 private 的最终拼写尚未冻结。引用其他 module 的名字需要显式 import，禁止依赖目录扫描产生未命名绑定。

## 4. 类型与函数拼写

确认采用：

- 类型后置且无冒号：`order Order`；
- 返回类型写在参数列表之后且无箭头；
- 泛型使用方括号：`Result[Money, Violation]`；
- optional 使用后缀：`Money?`；
- 构造和更新字段使用 `=`；
- 尾表达式可作为返回值，`return` 只用于提前返回；
- `let` 不可变，`var` 局部可变；
- `?` 传播 `Result` 错误。

```loom
module shop.checkout

pub type Money = Decimal where self >= 0

pub record CartLine {
    sku       Sku
    quantity  Quantity
    unit_price Money
}

pub fn line_total(line CartLine) Result[Money, Violation] {
    Money.from(line.unit_price * line.quantity)
}
```

连续字段可以为阅读对齐；字段次序不是相等、编码或执行语义。

## 5. 数据与约束风格

优先把领域不变量写在类型附近：

```loom
type Quantity = Int where self > 0
type Title = Text where len(self) >= 1 && len(self) <= 120

record Order {
    id       OrderId
    subtotal Money
    discount Money?

    invariant match discount {
        None => true
        Some(value) => value <= subtotal
    }
}
```

产生新值时不隐藏可能失败的重建：

```loom
fn apply_discount(subtotal Money, discount Decimal) Result[Money, Violation] {
    Money.from(subtotal - discount)
}
```

候选 v1 的受约束更新表达式方向暂定为：

```loom
let authorized = order with { status = Authorized }?
```

E1 中除合法字面量外，构造和重建都返回 `Result`；更强的保持证明后置。具体诊断与类型显示在 E1 语料中校准。领域 key 的 `entity` 语义仍属于语言范围，但 E1 Checkout 先用带 `id` 字段的 record，避免把持久化身份一起塞进首个组合实验。

## 6. capability 与错误风格

外部依赖出现在函数合同旁：

```loom
enum AuthorizationFailure {
    Rejected(AuthorizationReason)
    Unavailable(AuthorizationCallError)
}

capability Authorization {
    fn authorize(request AuthRequest)
        Result[Permit, AuthorizationFailure]
}

capability OrderStore {
    fn save(order Order, operation_id OperationId) Result[StoredOrder, SaveError]
}

fn store_order(order Order, operation_id OperationId)
    Result[Order, CheckoutError]
    uses orders OrderStore
{
    match orders.save(order, operation_id) {
        Ok(stored) => Ok(stored.value)
        Err(error) => Err(PersistenceFailed(error))
    }
}
```

错误使用 `Result`、`match` 和 `?`；`?` 只传播与当前返回类型相同的错误，不触发隐式 `From`。跨错误类型必须用显式 `match` 映射。候选 v1 有纯函数值后可增加标准库 `map_err`，但不增加第二套 `throw`/checked-exception 写法。缺陷诊断与业务错误在类型和 UI 中保持不同类别。

provider 实现方向为 `provider Name implements Capability`；E1 先由 host test harness 提供具名内存 provider，不解析 provider body。provider declaration 是实现，target/scenario 创建的 instance 才是状态。

候选 v1 的资源使用采用词法作用域；`use` 不进入 E1：

```loom
fn export_report(path Path) Result[Unit, ExportError] uses files Files {
    use output = files.create(path)?
    output.write(render_report())?
    Ok(Unit)
}
```

## 7. 普通函数与 flow

普通函数体按书写顺序执行：

```loom
fn normalize(cart Cart) Result[Cart, CheckoutError] {
    let non_empty = require_non_empty(cart)?
    let priced = normalize_prices(non_empty)?
    Ok(priced)
}
```

只有需要开放的具名业务图才使用 flow。下面是**语义草图，不是冻结 grammar**；[Checkout fixture](../fixtures/checkout/README.md#5-候选设计约束) 是这张图唯一的 normative 版本：

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
```

这段草图确认的是：slot 在基础 flow 中明确出现；Ok/Err 是 typed lane；slot 调用位置可见。关键字 `slot`、`on`、`run` 和泛型形状仍待 fixture 展开后冻结。

贡献也必须具名和定向：

```loom
contribution VipDiscount to Checkout.pricing {
    transform vip_discount after product_promotions
        (input PricedCart) Result[PricedCart, PricingError]
    {
        apply_vip_discount(input)
    }
}

contribution ProductPromotions to Checkout.pricing {
    transform product_promotions
        (input PricedCart) Result[PricedCart, PricingError]
    {
        apply_product_promotions(input)
    }
}

contribution HighValueRisk to Checkout.pre_authorization {
    transform high_value_risk
        (input AuthorizationContext)
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

contribution AuditAuthorizationRejection to Checkout.authorization_rejected {
    transform rejection_audit(input AuthorizationRejectedContext)
        Result[AuthorizationRejectedContext, RejectionHandlingError]
        uses audit AuditSink
    {
        match audit.append(make_authorization_rejected_event(input)) {
            Ok(_) => Ok(input)
            Err(error) => Err(AuditFailed(error))
        }
    }
}
```

空 pipeline 返回 `Ok(input)`。每个 contribution 恰好提供一个 slot-local transform key。checker 先在 target-visible、同一 slot 的全部 declared transforms（active 或 inactive）中无条件检查 key 唯一并解析 anchor，再只把两端 active 的 `before` / `after` 边放进执行计划；因此 VipDiscount 单独启用合法，真正未声明的 key 才是 unknown anchor。同一 slot 的 active transforms 必须形成全序，首个 `Err` 停止；未定序、duplicate key、unknown anchor 和 cycle 都拒绝。slot 在基础 flow 中的调用位置决定它相对外层步骤的位置；贡献不能引用 slot 外节点。

## 8. policy 与 keyed slot 风格

这两类不进入 E1 parser，示例只固定方向。

规则 slot 使用内建、可证明交换的结果代数：

```loom
policy Eligibility(order Order) Decision[Reason] {
    slot checks rules using deny_reasons
}

contribution RiskLimit to Eligibility.checks {
    rule risk_limit {
        deny RiskExceeded when risk_score(input) > max_risk
    }
}
```

keyed slot 用于注册项或开放成员，key 冲突是编译错误。数据字段扩展、keyed slot 与 policy 是否进入后续版本，分别由 Checkout 之后的 fixture 裁决；E1 不实现，也不因 pipeline 成立而自动进入候选 v1。

## 9. module 与 build target

```loom
module shop.checkout

import shop.domain.Order
import shop.pricing.Money
```

构建目标的具体文件格式尚未冻结，语义上必须明确列出：

```text
entry point
direct contributions
capability slot -> provider bindings
```

E1 只有一个 repo-local build target，直接列 fully-qualified contribution；import 和依赖不激活行为，profile、feature、bundle 与 package activation 均后置。

配置表面的方向可以表达为：

```text
target checkout_test
  entry shop.checkout.checkout
  compose shop.checkout.ProductPromotions
  compose shop.checkout.VipDiscount
  compose shop.checkout.HighValueRisk
  compose shop.checkout.AuditAuthorizationRejection
  bind authorization = shop.test.RejectingAuthorization
  bind risk = shop.test.FixedRisk
  bind tax = shop.test.FixedTax
  bind orders = shop.test.MemoryOrderStore
  bind audit = shop.test.RecordingAuditSink
```

这是 E1 构建合同示意，不冻结 manifest 语法。每个生效 contribution 的路径都是 `target -> contribution -> slot`，每个 capability 槽恰有一个 binding。候选 v1 的大型工程阶段再验证 package feature/bundle：target 显式启用 composition domain，bundle 成员由贡献方声明并自动参与，避免消费方逐项登记。

## 10. example、scenario 与 property

```loom
example regular_total = calculate_total(sample_cart)

scenario authorization_rejection_is_audited for checkout_test {
    with authorization = fresh RejectingAuthorization
    with risk = fresh FixedRisk
    with tax = fresh FixedTax
    with orders = fresh MemoryOrderStore
    with audit = fresh RecordingAuditSink as recorded

    expect checkout(sample_input) == Err(AuthorizationFailed(Declined))
    expect recorded.trace == [AuthorizationRejected(sample_operation_id)]
}

property money_never_negative {
    for_all cart Cart
    expect calculate_total(cart).map(value => value >= Money.zero) == Ok(true)
}
```

`example` 必须纯。E1 scenario 必须显式选择一个 target，继承该 target 的 entry 与 direct contribution set，同时**全量替换**它的 provider instances：目标预声明的每个 capability 槽都必须恰好列一次 `with ... = fresh ...`，缺失、重复或只覆盖部分槽都失败。fixture alias/trace 只指向本次运行的 fresh instance。property 的最终生成器语法后置。已实现的验证声明都只由测试命令运行，源文件保存本身不产生外部效果。

## 11. 格式化原则

- formatter 保留作者的顶层声明分组和 record 字段顺序；
- import 可以按固定组排序；
- contribution、rule 和 flow step 的规范计划顺序不反写源码排列；
- 一行过长时优先按参数、链式调用或表达式边界换行；
- formatter 不用重排来表达语义；
- 错误恢复节点也必须保留原始文本，格式化器跳过不能可靠理解的区域。

## 12. E1 前必须补齐的 grammar

1. `pipeline[S, E]`、`transform` 与 contribution `before` / `after` 的最终拼写；
2. flow typed lane、`on` 与 `result` 的精确 grammar；
3. contribution 访问 slot input/output 的名字绑定；
4. `with` provider 的声明与实例化方式；
5. E1 所需的 Decimal/Text/Vec 字面量与最小 pattern；Text interpolation、range 后置到候选 v1；
6. module 与 import 的文件级 grammar；
7. top-level recovery 的 golden fixtures，包括未闭合字符串、括号和函数体。
