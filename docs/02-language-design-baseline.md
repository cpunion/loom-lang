# loom-lang 最小语言核心规范

状态：Core 0.1 / Confirmed Normative Design + C1 Executable Reference

证据等级：C1 executable core（Core 0.1 fixture 已通过真实工具链闭环）

日期：2026-08-24

本文是 Core 0.1 的语言语义基线，只规范已经确认的最小核心。本文明确写出的规则具有规范性；parser/checker 开工所需的精确 token、grammar、数值、failure 与 artifact 选择由 [Core 0.1–0.3 可执行合同](06-executable-contract.md)补齐。下一版已确认的行为抽象由 [Core 0.2 concept 与动态多态规范](05-concepts-and-dynamic-polymorphism.md)单独定义；AOP-like 组合和 desired-state/operator 仍在讨论。

文中的“必须”“不得”是规范要求；表面拼写由 [核心表面与代码风格](03-surface-and-style.md)补充。

## 1. Core 0.1 范围

Core 0.1 包含：

- `module`、显式 import、`pub`；
- `record`、`enum`；
- `fn`、method、只读与可写 receiver；
- `Option[T]`、`Result[T, E]`；
- 穷尽 `match`；
- 显式 rank-1 基本泛型；
- 普通 `test fn`；
- 名义受约束类型 `type T = Base where predicate`；
- record `invariant`；
- `requires`、`ensures`、`assert`。

普通表达式基础包括 `let`、局部 `var`、`if`、block、尾表达式、提前 `return` 和基础运算。表达式及参数从左到右求值。

### 1.1 Core prelude 与 `Float`

Core prelude 自动提供：`Bool`、`Int`、`Float`、`Text`、`Unit`、`Option`、`Result`、`Violation`、`ContractFault`，以及 `Some`、`None`、`Ok`、`Err`、`Unit` 构造。其他类型和函数必须显式 import。

Core 0.1 的 `Float` 固定为 IEEE 754 binary64：

- 基础运算使用 round-to-nearest, ties-to-even；
- 溢出、除以零和无效运算按 IEEE 754 产生 infinity 或 NaN，不隐式产生 ContractFault；
- 与 NaN 的 `<`、`<=`、`>`、`>=`、`==` 都为 false，`!=` 为 true；
- `+0.0 == -0.0` 为 true；
- Core 0.1 不提供 Float 的隐式 total ordering；
- 标准库 `standard.float.is_finite(Float) Bool` 是 compiler-known、pure、total predicate。

文本解析/格式化和跨平台 canonical encoding 由 [可执行合同第 6 节](06-executable-contract.md#6-float-parseformat-和-canonical-encoding)固定，不能改变上述运行语义。

`Int` 固定为 checked signed i64，不随目标平台改变；溢出、除零和最小 Int 值除以 `-1` 产生不可捕获的 `RuntimeFault`，且没有隐式 Int/Float 转换，详见 [可执行合同第 5 节](06-executable-contract.md#5-int-运行合同)。Int 算术仍不得进入要求 total 的 contract predicate。

Core 不同时暴露多组整数大小。未来若 FFI、二进制格式或 SIMD 需要精确宽度，可以显式增加 `I8/I16/I32/I64` 与 `U8/U16/U32/U64`；`ISize/USize` 只允许用于指针和宿主 ABI 边界，不成为默认算术、公共协议或持久化类型。普通源码继续使用跨平台语义固定的 `Int`。

## 2. 名字与 module

```loom
module shop.pricing

import shop.common.Currency
```

规则：

1. 每个源码文件恰有一个 `module` 声明；
2. 一个 module 可以分布在多个文件；
3. 声明的编译身份是 module 限定名，不是文件路径；
4. 默认只有本 module 可见，`pub` 声明进入外部接口；
5. import 必须显式，Core 0.1 没有 wildcard import；
6. import 只影响名字可见性，不执行初始化或激活行为；
7. 文件名、文件遍历顺序和顶层声明排列不影响名字解析；
8. 顶层没有可执行语句或可变全局状态；
9. Core 0.1 拒绝 module import cycle。

## 3. `record`

```loom
record PairOfPrices {
    first  Price
    second Price
}
```

`record` 是封闭、名义、具名字段的值积：

- 相同字段的两个不同 record 名仍是不同类型；
- 字段类型属于 record 的静态合同；
- record **声明**中的字段顺序服务阅读，不参与相等、编码或执行语义；
- record literal 的字段初始化式按 literal 中的源码顺序从左到右、各求值一次；
- Core 0.1 构造时必须提供全部字段，没有默认字段；
- 字段对外只读，只能由该类型的 `mut self` method 修改；
- record 使用值语义；修改一个值不得改变其其他逻辑副本；
- 没有 invariant 的 record literal 构造出 `T`；
- 有 invariant 的 record literal 构造出 `Result[T, Violation]`；
- Core 0.1 每个 record 最多声明一个 invariant clause；多个条件必须在该 clause 中用布尔组合明确写出；
- 增加或删除 invariant 会改变构造 API，属于有意的 breaking change；
- Core 0.1 没有继承、结构子类型、开放字段或对象 identity。

如果所有字段都支持值相等，record 可以派生值相等；否则使用相等是静态错误。

## 4. `enum`

```loom
enum PriceInputError {
    InvalidNumber(ParseFloatError)
    OutOfRange(Violation)
}
```

`enum` 是封闭和类型：

- variant 可以没有载荷或携带一个/多个 typed values；
- 自定义 variant 位于 enum 的命名空间中，构造和 pattern 使用 `EnumName.Variant`；
- Core prelude 的 `Some`、`None`、`Ok`、`Err` 是该限定规则的唯一内建短名；
- variant 源码顺序没有隐式整数或优先级语义；
- Core 0.1 不提供隐式 discriminant、开放 enum 或继承；
- 消费 enum 必须通过穷尽 `match` 或明确处理具体 variant。

## 5. `Option`、`Result` 与失败

`Option` 和 `Result` 是标准封闭泛型 enum：

```loom
enum Option[T] {
    None
    Some(T)
}

enum Result[T, E] {
    Ok(T)
    Err(E)
}
```

规则：

- Core 0.1 没有隐式 `null`；缺失值使用 `Option`；
- 可预期失败使用 `Result`；
- `Err` 是普通值，不是异常；
- 不提供隐式错误转换或 checked exception；
- `?` 等传播糖不属于 Core 0.1，当前使用显式 `match`；
- contract fault 与 `Result` 分轨，见第 11 节。

## 6. `match`

```loom
fn value_or[T](value Option[T], fallback T) T {
    match value {
        Some(found) => found
        None => fallback
    }
}
```

`match` 是表达式：

- 所有 arm 必须产生兼容类型；
- 对封闭 enum、Option 和 Result 必须穷尽；
- 没有 fallthrough；
- 不可到达 arm 是诊断；
- Core 0.1 pattern 只包含可递归嵌套的 variant pattern、payload binding、字面量和 `_`；
- 因此 `Err(LookupError.Unavailable(reason))` 合法；record/collection pattern、pattern guard 和开放 pattern 后置。

## 7. 函数与 method

```loom
fn add_tax(price Price, rate Float) Result[Price, Violation] {
    Price(price * (1.0 + rate))
}
```

规则：

- 参数默认不可变；
- public 函数必须写出参数和返回类型；
- 局部变量允许类型推断；
- 普通函数体按源码顺序执行；
- 尾表达式是返回值，`return` 用于提前返回；
- Core 0.1 没有函数重载、动态派发或隐式 receiver；Core 0.2 的 concept 接口参数在需要擦除时增加 receiver dispatch，编译器可对静态可知的接口调用去虚化。

method 在 `impl T` 中使用独立的 `method` 关键字声明：

```loom
impl Order {
    method total(self) Float {
        self.subtotal - self.discount
    }
}
```

- 只有定义 `T` 的 module 可以声明 `T` 的 inherent methods；
- method 是带显式 receiver 的静态函数；
- `self` 默认深只读，等价于把比 C++ `const` 更严格的语义设为默认；
- `mut self` 才能写字段或调用其他 `mut self` method；
- `mut self` 是对 caller `var` place 的独占 inout receiver，不是 copy-in/copy-out，也不消费该值；
- `mut self` 只能对 `var` place 调用，不能对 `let`、temporary 或 rvalue 调用；
- method 通过 `Ok`、`Err` 或其他正常返回离开时，修改已经写回同一个 caller place；其他逻辑副本不改变；
- ContractFault 终止普通控制流，不承诺回滚，也不存在可继续观察“部分写回”的普通业务分支；
- Core 0.1 没有 interior mutability；只读 method 不得通过字段、容器或别名修改 receiver 可达状态；
- read-only 只约束 receiver，不自动代表未来意义上的 effect purity；
- 所有 method 调用都建立 invariant 边界；method 正常返回时 receiver 的 invariant 必须成立，返回 `Err` 也属于正常返回。

## 8. 基本泛型

```loom
record Pair[A, B] {
    first  A
    second B
}

fn first[A, B](pair Pair[A, B]) A {
    pair.first
}
```

Core 0.1 的泛型只包含：

- 显式 rank-1 类型参数；
- 泛型 record、enum 和 fn；
- 泛型 fn 调用从实参推断类型实参；
- 泛型 record/enum constructor 同时从字段或 payload 以及 expected type context 推断；因此 `None`、`Ok(Unit)`、`Err(problem)` 只有在上下文确定其余类型参数时才合法；
- 推断仍有多个解或没有解时静态失败，不设置默认类型；
- 定义处静态检查；
- 默认不变型。

无约束类型参数只能被存储、传递、返回、构造进其他值或 pattern match。Core 0.1 不能对 `T` 使用相等、排序、算术或任意 method，也不得采用 duck typing。Core 0.2 已确认用显式 `T: Concept` bounds 开放相应能力，见 [concept 与动态多态规范](05-concepts-and-dynamic-polymorphism.md)。

HKT、特化、反射、类型级计算和用户行为抽象不属于 Core 0.1。Core 0.2 只增加一套 `concept`，不会再增加平行的 `trait` 关键字。

## 9. 名义受约束类型

```loom
type Price = Float where self >= 0.0
```

`Price` 是名义类型，不是 `Float` 的别名。即使两个类型拥有相同 base 和 predicate，它们仍不相等。

唯一构造规则是：

```loom
Price(value) : Result[Price, Violation]
```

其规范步骤为：

```text
value 恰好求值一次
→ 对该值求 where predicate
→ true 产生 Ok(Price value)
→ false 产生 Err(Violation)
```

因此：

- `Price(expr)` 的静态类型始终是 `Result[Price, Violation]`；
- 编译器证明 predicate 时可以消除运行期检查，但不得改变表达式类型；
- `Float` 不能隐式缩窄成 `Price`；
- `Price` 可以按其 base value 读取为 `Float`；
- 普通 Float 运算结果仍为 Float，除非未来规则证明闭包，否则必须重新构造 Price；
- record 构造、反序列化、FFI 和泛型代码不得绕过检查；
- `parse_float(text)` 若返回 Result，必须先显式处理，语言不自动展平嵌套失败。

约束只保证声明的 predicate。按第 1.1 节规则，`NaN >= 0.0` 为 false，因此被拒绝；正无穷会通过。若价格还必须有限，必须显式 import 并使用标准库 predicate：

```loom
import standard.float.is_finite

type Price = Float where is_finite(self) && self >= 0.0
```

### 9.1 `Violation`

约束或 invariant 建立失败返回结构化 `Violation`，至少包含：

- 目标类型；
- 失败的 predicate/code；
- 字段路径（若存在）；
- 安全的值摘要；
- 源码中的合同位置。

Violation 是数据建立失败，可以由普通程序通过 Result 处理。

## 10. record invariant

```loom
record Order {
    subtotal Price
    discount Price

    invariant is_finite(self.subtotal) &&
        is_finite(self.discount) &&
        self.discount <= self.subtotal
}
```

规则：

- invariant 是 record 全值合法性；
- 带 invariant 的外部构造返回 `Result[Order, Violation]`；
- 构造时先求值全部字段，再检查唯一的 invariant clause；
- 一个已经建立的 Order 在每个 method 入口都必须满足 invariant；
- `mut self` method 可以在自己的 body 内暂时改变字段，但 invariant 失效期间 receiver 被隔离：不得复制、传参、存储、返回、捕获，也不得作为 receiver 调用任何 method；
- 调用另一个 private 或 public method 前必须先恢复 invariant；
- 每个正常出口，包括返回 `Err` 的出口，都必须重新满足 invariant；
- 实现破坏一个已建立对象的 invariant 是 `InvariantFault`，不是普通 Violation。

## 11. `requires`、`ensures` 与 `assert`

```loom
impl Order {
    method apply_discount(mut self, value Price) Unit
        requires value <= self.subtotal
        ensures self.discount == value
    {
        assert value <= self.subtotal
        self.discount = value
    }
}
```

四种合同的失败语义不同：

| 合同 | 语义 | 失败 |
|---|---|---|
| `where` | 单值合法性 | 构造返回 `Err(Violation)` |
| `invariant` | record 整体合法性 | 外部构造返回 Violation；实现破坏产生 `InvariantFault` |
| `requires` | 调用者义务 | `PreconditionFault`，blame caller |
| `ensures` | 实现正常返回承诺 | `PostconditionFault`，blame callee |
| `assert` | 实现声明当前位置必然成立 | `AssertionFault`，blame 当前实现 |

`PreconditionFault`、`PostconditionFault`、`InvariantFault` 和 `AssertionFault` 都属于结构化 `ContractFault`：

- 普通业务代码不能捕获后继续运行；
- test runner 和宿主边界可以报告；
- 不是 undefined behavior；
- 不能被自动转换成 `Err`。

可预期的业务拒绝必须使用 `Result`，不能滥用 `requires`。

### 11.1 合同表达式

合同 predicate 必须 pure、deterministic、total。Core 0.1 允许参数、字段、`self`、`result`、`old(expr)`、字面量、total 基础运算、比较、布尔组合、穷尽 match，以及 compiler-known total predicates（首个是 `standard.float.is_finite`）；`assert` 还可以引用在该位置之前已经建立的 immutable locals。合同不得执行 I/O、修改状态或返回业务错误。

Core 0.1 的合同中不允许用户函数调用、索引、Int 算术或其他无法静态保证 total 的操作。即使 checked i64 运行合同已经闭合，可能产生 RuntimeFault 的 Int 算术仍不属于合同子集。后续若开放用户 pure/total function，必须先有可检查的效果与终止合同；当前闭合子集见 [可执行合同第 7 节](06-executable-contract.md#7-contract-predicate-与-old)。

- `result` 表示完整返回值；
- `old(expr)` 只允许出现在 ensures，表示当前 fn/method 调用入口时的逻辑值快照；其中表达式只能引用在入口已存在的参数，以及 method 的 `self`/字段；
- 多条合同按逻辑与解释；
- 合同在所有 build mode 中有效；
- 只有静态证明成立时，编译器才能消除相应运行期检查。

### 11.2 fn 与 method 的检查顺序

普通 fn 也可以声明 `requires` 和 `ensures`。它的观察顺序固定为：

```text
requires
→ 捕获 old(...)
→ body
→ ensures
```

method 在同一规则上增加 receiver invariant 边界，所有 public/private method 都采用：

```text
入口 invariant
→ requires
→ 捕获 old(...)
→ body
→ 出口 invariant
→ ensures
```

若 body 通过任一正常路径返回，包括 `Ok` 或 `Err`，出口 invariant 与 ensures 都必须执行。出口 invariant 先检查；若它与 ensures 同时不成立，报告 `InvariantFault`，不会被较后的 PostconditionFault 遮蔽。

## 12. 普通 test

```loom
test fn negative_price_is_rejected() {
    let rejected = match Price(-0.01) {
        Err(_) => true
        Ok(_) => false
    }

    assert rejected
}
```

`test fn` 是只进入测试构建的普通顶层函数：

- 无参数；
- 返回 Unit 或 `Result[Unit, E]`；
- 使用与普通代码相同的 parser、类型系统和合同；
- 正常返回 Unit/Ok(Unit) 即通过；
- 返回 Err、ContractFault、RuntimeFault 或未处理程序缺陷即失败；
- 对预期业务 Err 必须在 test 内显式 match；
- test 遵守正常 module/import/可见性；
- 没有专用响应式执行、fixture 生命周期、mock DSL 或隐式依赖注入。

Core 0.1 不包含 `example`、`scenario`、`property`。

## 13. 明确开放的问题

以下不属于本规范，也没有保留关键字：

- AOP-like 静态组合和注入；
- desired-state/operator/reconcile；
- capability/provider/effect；
- Core 0.3 已单独定义的 GC、scoped/defer 与结构化 async/Task 不属于 Core 0.1；持久化 coroutine 和分布式执行仍开放；
- 继承、concept conformance 之外的自由 extension declaration、开放/多重派发和第二套 trait/interface 抽象；
- registry package、lockfile、feature/bundle；基础 manifest/path dependency/bin-test target 属于工具链层，不改变 Core 0.1 表达式语义；
- `?`、pattern guard、默认字段和复杂解构；
- 一般所有权、借用与公开底层内存布局；Core 0.2 接口参数由编译器管理为 call-scoped value/inout，不增加 borrow、lifetime 或 owning-carrier 源码语法。

`concept`/`dyn concept` 已经通过独立裁决进入 Core 0.2；GC、scoped/defer 与结构化 Task 已通过 [独立裁决](08-memory-cleanup-and-async.md)进入 Core 0.3，不再属于本节开放问题。其余方向只有在新的最小例子闭合后，才能修改 Core 版本。
