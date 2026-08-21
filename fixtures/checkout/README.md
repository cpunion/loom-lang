# Checkout fixture 设计

状态：Design only / 尚无实现

对应实验：[第一项 checkout 对照实验](../../docs/01-first-experiment.md)

边界依据：[loom-lang 项目章程](../../docs/00-charter.md)

本目录当前只冻结 fixture 的公平性与可观察契约。E0 不放置无法运行的 `.loom`、TypeScript 空壳或伪生成产物。

## 1. 领域模型

两种实现必须表达同一组概念：

| 概念 | 最低语义 |
|---|---|
| `Cart` / `CartLine` | 至少一行；数量为正；商品单价非负 |
| `Money` | 十进制定点或等价精确表示；最终金额非负 |
| `Customer` | 普通/VIP；带 region 与可选 VAT identity |
| `CheckoutRequest` | cart、customer、operation id |
| `Order` | 价格明细、授权结果、持久化标识 |
| `CheckoutError` | Validation、Tax、Risk、Authorization、Persistence 的可区分结果 |

初始行为：

```text
validate cart
-> calculate base price and product promotions
-> calculate tax
-> authorize final amount
-> persist once by operation id
-> return order
```

AuditSink 存在但只记录协议要求的事件，不能用它作为任意 debug log。

## 2. 能力 stub

所有外部作用均由确定性内存 stub 提供：

- `Auth.authorize(request)`：按冻结表返回允许或拒绝，并记录调用；
- `Risk.assess(request)`：按金额阈值和固定客户表返回结果；
- `OrderStore.save(order, operation_id)`：相同 operation id 幂等，记录首次调用；
- `AuditSink.append(event)`：保留有序事件列表；
- `Tax.rate(region)`：从固定表读取，无网络和时间依赖。

stub 的输入、输出和 trace 使用语言无关的 canonical JSON 表示。两种实现必须通过同一 trace oracle；运行时性能不在本实验范围。

## 3. Oracle 分层

### 3.1 行为 oracle

至少覆盖：

- 空购物车、零/负数量和非法金额被拒绝；
- 普通与 VIP、US 与 EU 的价格明细；
- VAT identity 缺失/非法；
- 风险通过/拒绝；
- 授权通过/拒绝；
- persistence failure；
- 相同 operation id 的重复请求不会重复创建订单。

### 3.2 顺序 oracle

trace 必须验证，而非只看最终值：

```text
validation failure -> no risk, auth, audit, persist
risk rejection -> risk before auth; no auth or persist
authorization rejection -> auth before rejection audit; no persist
success -> price/tax before risk/auth; persist after auth
VIP discount -> after product promotion, before tax
```

### 3.3 解释 oracle

对于每个最终步骤、约束和价格规则，保存：

- 稳定的逻辑名称；
- 源贡献名称；
- 显式目标；
- 前置数据依赖；
- 效果或业务顺序边；
- 删除该来源后的预期影响集合。

候选 `explain` 输出可有自己的呈现格式，但必须无损映射到这份 oracle。传统基线由静态源码/wiring 回答同一问题。

### 3.4 Mutation oracle

至少引入以下 mutation，确认测试不会只覆盖 happy path：

- 把 VAT 检查移到授权之后；
- 把 VIP 折扣移到税后；
- 授权拒绝时漏写 audit；
- validation failure 仍写 audit；
- persistence 位于 authorization 之前；
- 同 operation id 重复持久化；
- 倒置显式顺序边；
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

基线可以重构。任务时间和修改位置包含为安全完成重构所需的工作。

## 5. 候选设计约束

以下只是能力草图，不冻结表面语法：

```loom
flow Checkout(request CheckoutRequest) Order {
    step validate = validate_cart(request.cart)?
    step price after validate = price_cart(validate)
    step authorize after price = authorize(price)?
    step persist after authorize = persist(authorize, request.operation_id)?
    result persist
}

contribution VipPricing to Checkout.pricing {
    step vip_discount after product_promotions before tax = ...
}
```

无论最终语法如何，语义必须满足：

1. `VipPricing` 命名唯一目标 `Checkout.pricing`，不能使用名称模式寻找多个目标；
2. `vip_discount` 的输入输出和顺序边进入编译计划；
3. 目标未开放该贡献类型时静态拒绝；
4. 贡献与基础声明在解释输出中地位对等、来源可定位；
5. 文件排列、导入遍历和注册时机不改变计划；
6. 删除贡献后，检查器能给出其直接影响集合；
7. 无法闭合的歧义不通过“最后一个获胜”解决。

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
- A/B 任务难度经 pilot 无明显偏斜；
- 行为、trace、mutation 与解释四层评分均可重复；
- 评分只读取冻结源码、构建/测试结果、执行 trace 与参与者答案；
- 在一个全新的普通 Git clone 中可以执行完整实验。
