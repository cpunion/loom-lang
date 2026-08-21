# loom-lang Core 0.1 实现分期

状态：Core Delivery Plan 0.1 / Active

证据等级：C0 草案（核心基线已确认，executable contract 尚未完成）

日期：2026-08-21

本文只安排 [Core 0.1](02-language-design-baseline.md) 的实现和验收。过去的 Checkout 组合实验已暂停；AOP-like 组合和 desired-state/operator 不在当前实现路线中。

## 1. C0：规范闭合中

当前阶段要产出唯一、互不矛盾的语义和表面合同：

- [项目章程](00-charter.md) 定义范围；
- [最小语言核心规范](02-language-design-baseline.md) 定义语义；
- [核心表面与代码风格](03-surface-and-style.md) 定义常用写法；
- 本文定义实现顺序和关门条件。

C0 当前尚未关门。关门不代表语言已经可运行，只代表实现者不需要自行发明 `Price` 构造、record invariant、method receiver 或合同失败的第二种解释。

## 2. C1a：常规静态语言骨架

先实现不依赖合同运行时的基础闭环：

| 子系统 | 最小范围 |
|---|---|
| source | UTF-8、注释、大括号、完整 declaration-start sequence 与 impl-member 同步恢复 |
| module | module、显式 import、pub、跨文件同 module、拒绝 cycle |
| data | record、enum、Option、Result、字面量与名义类型 |
| code | fn、let、局部 var、if、block、return、基础运算 |
| match | variant/payload/literal/`_`，封闭类型穷尽检查 |
| generics | rank-1 record/enum/fn、调用点推断、定义处检查 |
| tests | `test fn` 发现、执行与结构化失败报告 |
| tools | `loomc check`、`loomc test`、machine-readable diagnostics |

### C1a 必过门

1. 一个损坏声明不能吞掉后续合法顶层声明；
2. 恢复后的 `pub`、`test fn` 和 `method` 不得被错误降级或重新分类；
3. 文件名、文件遍历和声明排列不改变类型检查结果；
4. module cycle、重复声明、不可见名字和 import 错误稳定诊断；
5. enum/Option/Result 的遗漏 match arm 静态失败；
6. 泛型函数只依据声明签名检查，不按每个调用点重新猜实现；
7. `test fn` 的 Unit、Ok、Err 三条结果路径可区分；
8. CLI 与未来 LSP 使用同一个 parser/checker。

## 3. C1b：受约束数据

在普通类型系统稳定后实现：

- `type T = Base where predicate`；
- checked construction `T(expr) -> Result[T, Violation]`；
- 带 invariant record 的 checked literal；
- 结构化 Violation；
- 约束值向 base 的读取与运算后重新建立；
- 编译器可证明检查成立时的安全消除。

### C1b 必过门

1. `Price(-0.01)` 返回 Err，`Price(0.0)` 和 `Price(1.0)` 返回 Ok；
2. `Price(expr)` 无论能否静态证明，静态类型始终是 `Result[Price, Violation]`；
3. 解析、record literal、反序列化和泛型路径都不能伪造 Price；
4. 普通 Float 运算不会静默获得 Price 类型；
5. 有 invariant 的 Order literal 返回 Result，非法 discount 被拒绝；
6. Violation 至少稳定提供 type、predicate/code、path、safe value summary 和 contract location；
7. debug/release 的接受与拒绝结果一致；
8. NaN、正负无穷、负零和边界值的 Float 规则有 golden tests。

## 4. C1c：method 与契约

随后实现：

- `impl T` 和 inherent method；
- 默认深只读 `self`；
- 独占 inout `mut self`、caller `var` place 与正常返回写回；
- record invariant 的入口/出口检查；
- `requires`、`ensures`、`assert`；
- `result` 和 `old(expr)`；
- 结构化、不可由普通业务代码捕获的 ContractFault。

### C1c 必过门

1. 只读 method 写直接字段、容器成员或 receiver 可达状态都静态失败；
2. 非 `var` receiver 不能调用 `mut self` method；
3. `mut self` 从 Ok 或 Err 正常返回都写回 caller place，其他值副本不改变；
4. invariant 暂时失效的 receiver 不能逃逸、传参、存储或用于嵌套 method call；
5. free fn 和 method 的 `requires` 失败都报告 PreconditionFault 并定位 caller；
6. free fn 和 method 的 `ensures` 失败都报告 PostconditionFault 并定位 callee；
7. `assert` 失败报告 AssertionFault；
8. test runner 把 AssertionFault 归为测试失败，并保留与普通 Err 不同的类别；
9. 所有 private/public method 从 Ok 或 Err 正常返回时都重新检查 invariant；
10. 出口 invariant 先于 ensures；二者同时失败时稳定报告 InvariantFault；
11. 合同检查顺序与 Core 规范一致，`old` 读取入口快照；
12. ContractFault 不能被普通 `match` 或 catch 风格业务分支消费；
13. release build 不得整体关闭合同；只有静态证明成立的单项检查可以消除；
14. `try_apply_discount` 的 Err 路径保持 receiver 不变，并由 ensures 验证；
15. 编译器不得替普通 `Result` mut method 提供虚假的自动回滚。

## 5. C1d：可使用的最小工具链

Core 语义闭合后补齐日常使用所需工具：

- 稳定 formatter；
- diagnostics 的 JSON schema 与 golden；
- go-to-definition、find references、rename、hover 和 diagnostics LSP；
- 普通多文件 build artifact 或可替代的确定解释器；
- 增量实现可以后置，但增量与冷构建结果必须一致；
- 最小标准库：基础数值/Text、Option、Result、常用比较与 parsing。

C1 关门要求同一组 fixture 通过冷 CLI、测试 runner 和 LSP 三条入口，且诊断 code、span、blame 与 failure category 一致。

## 6. 实现前仍需冻结的可执行细节

以下是 Core 内部的工程合同，不是新语言能力。相应子系统开工前必须冻结：

1. 完整 lexical grammar、字符串转义和 error-island diagnostic code；
2. Float 文本解析/格式化与跨平台 canonical encoding（运行与比较语义已由 Core 规范固定）；
3. Int 位宽或 arbitrary-precision 裁决，以及溢出、除零、`MIN / -1`、转换和可进入合同的 total 运算；
4. `Violation` 与 `ContractFault` 的规范字段、code、序列化和隐私摘要；
5. 已确认 inout/exclusive 语义的静态 place/alias 分析算法与 diagnostic；
6. record/enum 值布局不参与源码语义时的 ABI 策略；
7. 合同 predicate 的可判定 pure/terminating 子集；
8. `old(expr)` 的快照边界和复制成本；
9. 标准库 builtin 与普通源码声明的边界；
10. compiler/runtime defect 的终止和宿主报告协议。

这些细节未闭合时可以做 lexer 或 checker spike，但不能把 spike 的偶然实现升级成规范。

## 7. 后续讨论轨，不进入当前排期

Core 0.1 之后再分别用小例子讨论：

### 7.1 AOP-like 静态组合

要回答的最小问题是：一个 owner 如何显式开放扩展位置，外部贡献如何静态附着、排序、类型检查和解释，同时不依赖调用栈匹配、全局扫描或隐藏控制流。

在该问题闭合前，不定义 `flow`、`slot`、`contribution`、pointcut/advice 或任何同类关键字。

### 7.2 desired-state / operator

要回答的最小问题是：如何先声明 desired/current/observation，再由纯 plan 和显式 action 驱动收敛；如何处理幂等、重试、崩溃恢复、版本 basis 和不收敛。

在该问题闭合前，不定义 `operator`、`machine`、`reconcile` 或专用 runtime。

这两条研究轨彼此独立，也不得作为 Core 0.1 实现的前置条件。
