# 第一项实验：checkout 的声明、约束与显式贡献

状态：Paused / Non-normative composition research

日期：2026-08-21

> 本文早于当前 Core，保留为未来讨论静态组合时的研究输入。文中的 `flow`、slot、contribution、capability、target、example 和 scenario 都不是当前语言决定，不得据此实现 parser、checker 或 runtime。当前权威范围见根 [README 权威表](../../README.md#文档权威关系)。

关联记录：[fixture 设计](04-checkout-composition-fixture.md)

## 1. 研究问题

第一项实验只回答：

> 面对一个规则持续增加的 checkout 系统，`loom-lang` 的声明、约束和显式贡献，是否能在不牺牲正确性与可理解性的前提下，减少修改散点并提高完成变更的效率？

两组都使用普通文本编辑器、普通 Git checkout 和命令行测试。正式 E2 前，候选必须具备 diagnostics、go-to-definition 和 rename 的最低 LSP 能力；基线组使用其生态中的同类功能。工具不对等时只做机制测试，不采集开发效率数据。

## 2. 为什么选择 checkout

checkout 同时包含局部业务逻辑和典型横向规则，但不需要分布式 runtime 才能观察差异：

- 金额、数量与订单状态约束；
- 定价、折扣、税费和授权之间的数据依赖；
- 地区合规、风险检查、审计和幂等要求；
- 不同 feature 对同一流程的贡献；
- 顺序错误会产生可观察的业务缺陷；
- 可以用纯内存能力和确定性输入建立完整 oracle。

它足以暴露“贡献是否真的局部、组合是否真的可解释”，又不会让网络、数据库或部署噪声主导结果。

## 3. 对照方案

### 3.1 传统基线

首选基线为 TypeScript（固定版本）上的惯用模块化实现。它可以使用：

- 类型、判别联合、纯函数和模块；
- 显式 pipeline/middleware 或策略对象；
- 依赖注入、集中 wiring 和成熟测试框架；
- formatter、language server 和普通 Git；
- 参与者认为合理的重构，只要不改变公开行为。

不得故意把基线写成单个巨型函数、复制粘贴规则或禁用成熟抽象。正式实验前由至少一名熟悉 TypeScript 的外部审阅者确认基线是可维护的惯用实现。

为保证扩展任务公平，基线必须拥有与候选 flow slot 对等的 typed extension points 和一个显式 composition root。E1 两组都直接在 root 启用具体策略/handler；修改 root 是允许且单独计数的正常动作，实验要测的是 typed composition/check/explain 是否优于惯用 plugin wiring，而不是冒充“无需 wiring”。

因此 Checkout 只能证明“预先声明 extension point 后”的收益，不能证明 loom-lang 能自动适应任意未预见变化。另设一个不计效率分的 held-out boundary probe：需求落在没有 slot 的位置时，两组都必须修改 owner contract；候选若能偷注入则判失败。

### 3.2 loom-lang 候选

候选实现必须只依靠语言假设本身：

- 领域值和不变量由声明/约束表达；
- checkout 的基础步骤与 typed outcome 显式声明输入和输出；
- 基础 flow 预先声明 `pricing`、`pre_authorization` 和 `authorization_rejected` 三个 typed slot；
- 每个 contribution 提供一个 slot-local keyed transform，由单一 target 直接启用；
- 每条 flow outcome path 与同一 pipeline 的 active transforms 都由数据/typed outcome 依赖和显式 `before/after` 闭合为唯一顺序；
- base flow 预声明 Authorization、Risk、Tax、OrderStore、AuditSink capability slots，contribution `uses` 不得超出其 slot 上界；
- duplicate transform key、missing target、missing/duplicate provider binding、unknown anchor、unordered pair 和 order cycle 静态失败；
- `explain` 从实际组合计划列出每个规则/步骤的来源和排序理由。

候选方案不得以隐藏注册表、命名约定扫描、调用栈匹配或手写中央 switch 冒充语言组合。
仅仅增加依赖或 import 不得激活 contribution；E1 所有生效行为必须沿 `target -> contribution -> slot` 追溯。feature/bundle activation 属于后续大型工程实验。

### 3.3 等价性

两组共享一份与语言无关的输入/输出 oracle：

- 相同的初始领域数据；
- 相同的 capability stub 行为与调用日志；
- 相同的成功结果、业务错误和拒绝原因；
- 相同的动作顺序约束；
- 相同的测试向量与 mutation tests。

只有两组都通过 oracle，才把任务计为成功。候选语言能更早报错是诊断收益，不能降低最终行为要求。

## 4. 初始系统

初始 fixture 包含：

1. 可容纳非法值的 `CheckoutInput` / `RawCartLine`，以及恒有效的 `Cart`、`CartLine`、`Money`、`Order`、`Region` 等领域声明；
2. 唯一 `CheckoutRequest.from(raw)` 建立路径，把数量、金额、空购物车等 ConstraintError 映射为 `CheckoutError.Validation`；
3. `validate -> price -> tax -> authorize -> persist` 的基础 checkout，以及定价、授权前检查、授权拒绝处理三个 closed-error pipeline；
4. 可控且每 scenario fresh 的 Authorization、Risk、Tax、OrderStore、AuditSink host provider；
5. 初始 active `ProductPromotions` pricing contribution，供 T2 形成真实 A+B 组合；
6. US/EU、普通/VIP、授权通过/拒绝、重复 operation id 的 oracle cases；
7. 能回答最终计划来源和顺序的基准问题。

完整设计见 [fixture 文档](04-checkout-composition-fixture.md)。E0 不提交伪实现源码。

## 5. 实验任务

正式任务包使用等价但不同命名/数值的 A、B 两套变体，避免第二次执行只是在复述答案。

### W0：热身，不计分

修正一个局部折扣常量并运行测试，用于确认环境、命令和测试反馈均可用。

### T1：增加地区约束

要求：新增 `CheckoutRequest` invariant：EU customer 必须持有 `Some(VatIdentity)`；既有 `VatIdentity` constraint 只负责长度 8 至 12（含边界）。`CheckoutRequest.from(raw)` 只负责统一触发这些声明并把 ConstraintError 映射为 `CheckoutError.Validation`，不得在 constructor、flow 或调用点再手写一份 VAT 条件。失败不进入 flow，capability trace 为空。TypeScript 基线使用同一 raw/schema constructor 边界和单一 schema 规则来源。

观察重点：约束写几处、边界是否遗漏、失败顺序是否正确。

### T2：增加 VIP 定价贡献

要求：VIP 折扣作为第二个 `pricing` contribution，以 transform key `vip_discount after product_promotions` 加入 target；折扣后金额仍须满足 Money 约束，解释输出必须指出全序和激活路径。

观察重点：新增 contribution 是否只修改自身与 E1 target、是否需要修改目标 pipeline 实现、组合顺序是否清晰。

### T3：增加风险检查与审计

要求：高价值订单通过 `pre_authorization` slot 在授权前执行风险检查；所有授权拒绝通过 `authorization_rejected` typed outcome lane 审计，但 validation/risk failure 不得进入该 lane；审计事件包含稳定 operation id。target 显式启用两项 contribution；Risk/AuditSink 与其余预声明 capability 槽从初始 fixture 起已经完整绑定。

观察重点：一个规则跨成功/失败路径时的散点、遗漏率和行为来源可追溯性。

### T4：发现并修复组合错误

提供一项会与现有贡献形成 duplicate key 或 order cycle 的变更。参与者须根据检查器给出的确定性 witness 定位完整冲突成员并修复，不得通过删除测试或硬编码总顺序绕过。

T4 是候选语言的机制门，不进入 Loom/TypeScript 完成时间或 diff 效率比较；惯用 TypeScript ordered array 并不天然存在同一种 cycle，不能为了“对等”强迫基线先实现一套定制图 DSL。观察重点是 checker witness 是否稳定、完整并直接指向贡献。

### B1：无扩展槽边界探针，不计效率分

提供一个落在基础 flow 未开放位置的 held-out 需求。正确行为是 missing target，随后由 owner 显式新增/修改 typed slot；任一方案若能通过调用栈拦截、名称匹配或任意位置注入绕过 owner contract，即判语言机制失败。

### C1：理解性问答

在不运行 debugger 的前提下回答：

1. 哪些来源共同决定最终价格？
2. 风险检查为什么位于授权之前？
3. 哪些错误会产生审计事件，哪些不会？
4. 从 target 移除指定 contribution 后，静态组合图中的哪些步骤、slot 与约束会受影响？

答案按预先冻结的来源/顺序 oracle 评分。

## 6. 实验设计

### 6.1 阶段门

1. **E1 机制门**：两种实现通过相同 oracle，候选 ordered-pipeline subset matrix、closed errors、capability closure、`check/explain` golden 全部稳定；
2. **E2 pilot**：至少 4 名非作者参与，发现任务歧义和学习材料问题，只修 protocol，不报告生产力结论；
3. **E2 正式实验**：目标至少 12 名具有 TypeScript 经验的参与者，采用组内 crossover 与反平衡顺序；
4. 在采集前冻结源码、任务、时间上限、评分脚本和分析表。

若招募规模不足，数据只能标为 exploratory，不进行“效率更高”的结论升级。

### 6.2 反平衡

- 一半参与者先做 baseline-A，再做 Loom-B；另一半先做 Loom-A，再做 baseline-B；
- A/B 的结构难度、测试数、变更类型一致，领域名称和常量不同；
- 两种方案分别提供等时长教程和一次不计分热身；
- 作者不得在计时中解释语言或提示代码位置。

### 6.3 记录

记录终端命令、测试结果、Git diff、任务开始/结束时间和最终问卷。不要求专用编辑器遥测，也不记录与任务无关的个人内容。

## 7. 预注册指标

### 7.1 主要指标

| 指标 | 定义 |
|---|---|
| 正确性调整后的完成时间 | 从读题到共享 oracle 全绿；超时或 oracle 未通过按上限计并另记失败 |
| 任务成功率 | 行为 oracle、顺序 oracle和禁止绕过检查全部通过 |
| 理解正确率 | C1 的来源、适用条件、顺序理由和删除影响回答得分 |

主要比较使用参与者内的配对差值，并同时报告原始分布、中位数和置信区间；不只报告最快案例。

### 7.2 次要指标

- 修改的语义位置数：需要独立理解并修改的声明/函数/配置点；
- 触碰文件数与非格式化 diff 行数；
- 同一约束或顺序事实的重复表达次数；
- 首次得到正确诊断、首次测试通过和最终完成的时间；
- 测试失败次数与 escaped mutation 数；
- 查明一项行为全部来源所需时间；
- 主观负担（7 点量表）及自由反馈。

“行数更少”本身不是成功标准；生成文件、formatter churn 和测试 fixture 不计入语义位置数，计数规则在 pilot 前冻结。

### 7.3 语言健全性门禁

下列项目要求 100% 通过，不以平均效率抵消：

- duplicate transform key、missing target、unknown anchor、unordered pair、order cycle、missing/duplicate provider binding 均 fail closed；
- target 未声明 typed slot 时 contribution 必须失败；依赖/import 本身不得激活 contribution；
- pricing empty/A/B/A+B 全 subset 通过，A+B 有显式全序；slot 的 error/capability 上界不可被 contribution 扩展；
- 每个接受的步骤和约束都有唯一、可定位来源；
- 每次执行 trace 是静态 plan 中唯一 typed path 的 refinement，且 capability 调用顺序一致；
- 相同源码和依赖在重复构建时得到相同计划；
- 没有依赖文件排列或遍历顺序的隐藏控制流。

## 8. 继续、重设计与停止条件

阈值在正式采集前锁定，采集后不得移动。

### 8.1 继续进入第二领域

同时满足：

1. 候选正确性不劣于基线，且所有语言健全性门禁通过；
2. 可对照计时的 T1–T3 中至少 2 项配对中位完成时间改善达到 20%；
3. 可对照计时的 T1–T3 中至少 2 项语义修改位置中位数不高于基线的 60%；
4. 约束任务 T1、核心组合任务 T2 和 typed outcome/effect 任务 T3 各自的正确性、时间与局部性都不得劣于基线；
5. C1 理解正确率至少 90%，且不低于基线；
6. 优势不能主要来自候选测试更弱、基线写得不惯用或参与者接受了额外现场提示。

这些阈值是路线选择门，不等于统计显著性声明；样本、区间和失败案例仍须完整报告。

### 8.2 重设计后最多再试一次

出现以下任一项，先修语言/任务而不是扩展功能：

- 正确性合格但时间或局部性未过门；
- 参与者能完成任务，却普遍不能解释组合来源或顺序；
- 超过 25% 的参与者除了修改允许的 composition root，还必须修改目标实现或中央条件 switch 才能完成局部贡献；
- 诊断可修复但稳定地把错误指向错误来源；
- 学习成本主导结果，且一次限定范围的语法/教程修订有明确修复假设。

同一 checkout 假设最多做一次实质重设计复验，避免无止境调整直到指标好看。

### 8.3 停止或收缩该语言假设

出现以下任一项即停止扩大 compiler/runtime，保留实验报告：

- 需要隐藏目标匹配、不可见控制流或全局手工注册表才能写完 fixture；
- contribution 能进入目标未声明的 slot，或能拦截任意调用/错误；
- 接受的程序无法从同一组合计划完整解释来源和顺序；
- checker/explain 计划与执行 trace 在来源、typed outcome 或可观察顺序上不一致；
- 为完成 Checkout E1 被迫加入 field extension、开放 error union、resource footprint、用户自定义组合代数、feature/bundle activation 或目标状态 runtime 中的任一项；
- ordered pipeline 的 empty identity、context threading、closed error mapping 或 capability closure 在 parser 开工前仍不能唯一说明；
- 相同源码和 compose 配置会因文件布局、遍历或链接顺序产生不同计划；
- 两轮均未达到正确性、局部性和效率继续门；
- 相比惯用基线，显式贡献只是换名的 middleware/wiring，且没有可测的局部性或理解收益；
- 语言机制复杂度使简单局部修改持续慢于基线，或产生更多逃逸缺陷。

停止 checkout 方案不自动否定所有声明式语言研究，但它否定以当前贡献模型继续建设通用 Loom 语言的依据。

## 9. E0 完成清单

- [ ] 章程与项目边界经审阅冻结；
- [ ] baseline 技术栈、版本和惯用性审阅者确定；
- [ ] A/B fixture 的行为与顺序 oracle 冻结；
- [ ] raw input 建立、closed errors、capability/provider 和 pipeline subset matrix 冻结；
- [x] E2 所需最低 LSP parity 有可执行门禁（协议测试覆盖 diagnostics、definition、references、prepare rename/rename、completion 与 document/workspace symbols）；
- [ ] T1–T3 对照任务、T4 候选机制门和 B1 边界探针可由未参与设计者独立理解；
- [ ] 指标计数脚本/表格设计完成；
- [ ] kill criteria 获得项目负责人确认；
- [ ] 之后才批准最小 executable slice。
