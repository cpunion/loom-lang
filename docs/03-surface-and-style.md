# loom-lang 核心表面与代码风格

状态：Core Surface / Core 0.1–0.3 C1 Executable Reference

证据等级：C1 executable core（parser、formatter 与 checker 已共享该表面合同）

日期：2026-08-25

本文规定 [Core 0.1](02-language-design-baseline.md) 已确认能力的常用写法。Core 0.2 的 `concept`、显式 conformance、Go 风格参数书写与 `dyn C` 由 [独立规范](05-concepts-and-dynamic-polymorphism.md)定稿；Go-like 仅指 `name Type` 表面，不包含 structural conformance 或运行时接口发现。Core 0.3 的 GC、词法清理和 Task/coroutine 由 [独立规范](08-memory-cleanup-and-async.md)定稿。精确 token、换行、precedence、statement 和 Core 0.2 parser 形状以 [可执行合同](06-executable-contract.md)为准。所有权/借用表面、AOP-like 组合、desired-state/operator、capability 和专用 `example`/`scenario` 不在本文中，也不预留关键字。

## 1. 总体风格

loom-lang 使用普通 UTF-8 文本，目标是像一门克制的现代静态语言：

- 大括号表达结构，缩进只负责排版；
- 相同概念只有一种主要写法；
- 普通函数按源码顺序阅读和执行；
- 类型、失败与公共合同显式；
- formatter 保留作者组织声明的顺序，不按字母重排；
- 文件路径用于工程组织，不参与语言语义。

Core 0.1 不使用分号。注释使用 `//`，文档注释使用 `///` 并附着下一条声明。formatter 使用 4 个空格缩进。

## 2. 文件、顶层声明与错误恢复

每个文件以一个 `module` 声明开始，随后可以有显式 `import` 和顶层声明：

```loom
module shop.price

import standard.float.ParseFloatError
import standard.float.parse_float
```

Core 0.1 的顶层 declaration-start token sequence 只有：

```text
module
import
(pub)? type
(pub)? record
(pub)? enum
(pub)? fn
impl
test fn
```

`pub` 必须与其后的声明关键字作为整体识别，恢复时不能跳过 `pub`。`impl` 内唯一的 member-start sequence 是 `(pub)? method`；method 不使用 `fn`，因此不会与顶层 `fn` 同步点混淆。

parser 的恢复规则同样属于表面合同：

1. 除 `impl` 中的 method 外，nested declaration 不合法；
2. 缩进不参与 parse，也不参与恢复；
3. 普通字符串不能跨越未转义换行，未闭合字符串在该行形成局部词法错误；
4. 一个顶层声明损坏后，parser 只在字符串/注释之外、换行后的 statement boundary 匹配完整 top-level start sequence；
5. 一个 method 损坏但 `impl` 外壳仍可识别时，以完整 `(pub)? method` sequence 同步下一个 member；遇到 top-level start sequence 则结束损坏的 impl error island；
6. 恢复不表示损坏程序可构建，只保证一处半写代码不会吞掉其后的独立声明，也不会改变恢复后声明的可见性。

精确 token grammar 和 diagnostic code 已由 [可执行合同](06-executable-contract.md)补齐，不得改变这条顶层同步原则。

## 3. 命名与可见性

| 类别 | 风格 | 示例 |
|---|---|---|
| module | 小写点分；每段 snake_case | `shop.checkout` |
| 类型、enum variant | PascalCase | `Price`、`InvalidNumber` |
| 函数、method、字段、局部变量 | snake_case | `parse_price`、`unit_price` |
| 类型参数 | 简短 PascalCase | `T`、`Value` |

声明默认只在本 module 内可见；`pub` 进入 module 的外部接口。跨 module 名字必须显式 import。Core 0.1 没有 wildcard import，也不根据目录扫描产生名字。

## 4. 类型、参数与 block

Core 0.1 的常用拼写为：

- 参数和字段采用 `name Type`；
- 非 `Unit` 返回类型写在参数列表之后，不使用 `->`；
- 省略返回类型固定表示 `Unit`，不触发返回类型推断；惯用源码省略，显式 `Unit` 仍合法；
- 泛型实参使用方括号，如 `Result[Price, ConstraintError]`；
- 缺失值写作 `Option[T]`，Core 0.1 不引入 `T?` 糖；
- `let` 声明不可变局部，`var` 声明可变局部；
- block 的尾表达式是返回值，`return` 只用于提前返回；无 operand 的 `return` 等价于 `return Unit`；
- 非 `Unit` 结果不能作为裸 expression statement；需要忽略时写 `discard expression`，不在 callee 上标注“可丢弃”属性；
- record literal 的字段使用 `=`；
- Core 0.1 不提供 `?` 错误传播糖，失败分支使用显式 `match`。

```loom
pub fn value_or[T](value Option[T], fallback T) T {
    match value {
        Some(found) => found
        None => fallback
    }
}
```

`discard` 可接受任意表达式，包括 call、block、`if` 或 `match`；它自身是 statement，不能出现在 initializer、argument 或 block tail 的 expression 位置。`Unit` 表达式可直接作为 statement；对它写 `discard` 合法但多余。

```loom
write_log("start")        // Unit result is already a statement
discard calculate_price() // explicitly ignore a non-Unit result
```

`discard` 不是“不执行”；operand 仍完整求值，包括所有副作用、合同检查和 fault。优化器只能删除已证明不可观察的整个求值。普通具体类型默认允许显式 discard，语言不定义 `Discardable`、`MustUse` 或 `NonDiscardable` concept。未约束 type parameter、`Self`、associated projection 或递归包含它们的类型不能证明没有 Task/MustScope obligation，因此泛型代码中的 discard 保守报 `CannotDiscardUnknownType`。

## 5. 受约束值：`Price`

```loom
module shop.price

import standard.float.ParseFloatError
import standard.float.parse_float

pub type Price = Float where self >= 0.0

pub enum PriceInputError {
    InvalidNumber(ParseFloatError)
    OutOfRange(ConstraintError)
}

pub fn parse_price(text Text) Result[Price, PriceInputError] {
    let raw = match parse_float(text) {
        Ok(value) => value
        Err(error) => return Err(PriceInputError.InvalidNumber(error))
    }

    match Price(raw) {
        Ok(price) => Ok(price)
        Err(constraint_error) => Err(PriceInputError.OutOfRange(constraint_error))
    }
}
```

这里的 `raw` 来自解析边界，proof domain 无法证明其值，所以 `Price(raw)` 是运行期 checked construction，静态类型为 `Result[Price, ConstraintError]`。相反，`Price(10.0)` 若能静态证明 predicate，类型直接是 `Price`，也不生成运行期检查。编译器不会隐式抛异常、隐式传播错误或把未知 `Float` 当成 `Price`。

`Price` 只能保证写出的谓词。上述定义会拒绝 NaN，但允许正无穷；若业务还要求有限值，必须显式写出：

```loom
import standard.float.is_finite

pub type Price = Float where is_finite(self) && self >= 0.0
```

## 6. record、invariant 与 method：`Order`

```loom
module shop.order

import shop.price.Price
import standard.float.is_finite

pub enum DiscountError {
    ExceedsSubtotal
}

pub record Order {
    subtotal Price
    discount Price
    note     Option[Text]

    invariant is_finite(self.subtotal) &&
        is_finite(self.discount) &&
        self.discount <= self.subtotal
}

impl Order {
    pub method total(self) Float
        ensures result >= 0.0
    {
        self.subtotal - self.discount
    }

    pub method try_apply_discount(mut self, discount Price)
        Result[Unit, DiscountError]
        ensures match result {
            Ok(_) => self.discount == discount
            Err(_) => self.discount == old(self.discount)
        }
    {
        if discount > self.subtotal {
            return Err(DiscountError.ExceedsSubtotal)
        }

        assert discount <= self.subtotal
        self.apply_discount(discount)
        Ok(Unit)
    }

    method apply_discount(mut self, discount Price) Unit
        requires discount <= self.subtotal
        ensures self.discount == discount
    {
        self.discount = discount
    }
}
```

这段代码固定了几个重要风格：

- method 放在 `impl Order` 中；
- `self` 默认深只读，因此 `total` 是默认的 const-like method；
- 只有 `mut self` method 能修改字段，而且调用点必须持有 `var` receiver；该 receiver 以独占 inout place 传入，正常的 Ok/Err 返回都会写回；
- `requires` 和 `ensures` 紧邻签名，`assert` 位于实现中；
- `old(self.discount)` 表示调用入口的值；
- 每个 private/public method 都建立 invariant 边界；返回 `Err` 仍是正常出口，先检查 record invariant，再检查 ensures；
- invariant 暂时失效时，`self` 不能逃逸，也不能作为 receiver 调用另一个 method；
- `Result` 不提供自动事务回滚，失败时不修改 receiver 是该 method 自己声明并实现的合同。

`total` 返回 `Float` 是有意的：Core 0.1 不承诺一般 refinement 证明，普通减法会擦除 `Price` 约束。若调用方需要新的 `Price`，必须重新执行 `Price(order.total())`。

## 7. enum、Option、Result 与 match

enum variant 写在封闭声明中；match 不 fall through，必须穷尽：

```loom
pub enum LookupError {
    Missing
    Unavailable(Text)
}

pub fn describe(result Result[Text, LookupError]) Text {
    match result {
        Ok(value) => value
        Err(LookupError.Missing) => "missing"
        Err(LookupError.Unavailable(reason)) => reason
    }
}
```

可预期失败使用 `Result`，缺失值使用 `Option`。跨错误类型必须像 `parse_price` 那样显式映射；不存在隐式错误转换或 checked exception。

## 8. 普通 test

普通测试只是在测试构建中发现的 `test fn`，不引入第二套运行模型：

```loom
fn checked_price(raw Float) Result[Price, ConstraintError] {
    Price(raw)
}

test fn negative_price_is_rejected() {
    let rejected = match checked_price(-0.01) {
        Err(_) => true
        Ok(_) => false
    }

    assert rejected
}

test fn order_total_is_non_negative() {
    let subtotal = Price(100.0)
    let discount = Price(20.0)
    let order = Order {
        subtotal = subtotal
        discount = discount
        note = None
    }

    let total = order.total()
    assert total == 80.0
    Unit
}
```

`Price(-0.01)` 本身是静态可否证的构造，会产生 `ConstraintUnsatisfied` 编译诊断；测试运行期拒绝路径时，应像上例一样经过真实的动态输入边界。`ConstraintError` 只表示约束/外部 invariant 建立失败，不是通用 `Error` 父类型。

测试正常返回 `Unit` 或 `Ok(Unit)` 即通过；返回 `Err`、产生 ContractFault、RuntimeFault 或发生未处理缺陷即失败。

## 9. Core 0.2 concept 与接口参数

Core 0.2 的主写法：

```loom
pub concept Ordered {
    method less_equal(self, other Self) Bool
}

pub dyn concept Formatter {
    associated type Error

    method format(self, document Document)
        Result[Text, Self.Error]
}

impl Ordered for Price {
    method less_equal(self, other Self) Bool {
        self <= other
    }
}

fn smaller[T: Ordered](left T, right T) T {
    if left.less_equal(right) { left } else { right }
}

fn format(
    formatter Formatter[Error = FormatError],
    text Text,
) Result[Text, FormatError] {
    formatter.format(text)
}

fn explicitly_erased(value dyn Formatter[Error = FormatError]) Unit {
    Unit
}
```

参数使用 `name Type`，不写 `name: Type`。普通 `C` 是接口参数的惯用形式；参数位置的 `dyn C` 具有相同可观察语义，只显式强调类型擦除。字段、返回、tuple/list 与泛型嵌套显式写 `dyn C`。具体实参自动适配，`mut self` receiver 要求 `var` place；普通接口 copy 隔离 underlying logical value。源码不写 `view[...]`、borrow、lifetime、`box/shared` 或其他所有权 carrier。

Core 0.2 parser 的 top-level sequence 增加 `(pub)? concept`、`(pub)? dyn concept` 与 `impl Concept for Type`。conformance body 的 member-start sequence 是 `associated type`、`method` 和 `static method`；它与 Core 0.1 的 inherent `impl Type` 分开恢复。语义与动态兼容以 [Core 0.2 规范](05-concepts-and-dynamic-polymorphism.md)为准，精确 parser 形状与拒绝规则以 [可执行合同第 11 节](06-executable-contract.md#11-core-02-parserchecker-降级合同)为准。

## 10. Core 0.3 scoped、defer 与 Task

词法资源写法：

```loom
scoped file = open(path)
defer {
    closeWithProtocolName(handle)
}
```

不写 `scoped let`，也不写析构属性或 ownership/lifetime。`scoped` binding 本身稳定；资源内部需要变化时仍通过已有的 `mut self` method。cleanup 属于最内层 block，按 LIFO 执行。

async 函数仍使用普通逻辑返回类型：

```loom
async fn load(id Int) Document {
    ...
}

let document = load(1).await
let decoded = load(1).await.decode()
let document = load(1).await?
Task.sleep(10).await // 10 milliseconds
Task.sleep(milliseconds(10)).await
```

固定元数的并发 join 返回 tuple：

```loom
let user, settings = Task.all(loadUser(), loadSettings()).await
```

动态数量使用同构 task list：

```loom
var tasks = List[Task[Report]]()
// 根据运行时输入加入任意多个 Task[Report]
let reports = Task.all(tasks).await
```

`.await` 是不可重载的后缀关键字，不是普通零参数 method；写 `.await`，不写 `.await()`，旧前缀 `await task` 按普通非法语法报错。`?` 是与 async 无关的独立后缀传播运算符：`task.await?` 先取得 `Result[T, E]`，再在 `Ok` 时产生 `T`、在 `Err` 时从当前 callable 返回 `Err`。当前不做隐式错误转换，`E` 必须与当前 callable 的 `Result[_, E]` 完全一致；`!` 不构成强制 await/unwrap 语法。`Task.sleep(delay)` 接受非负毫秒 `Int` 或 `Duration`；`Task.waitReadable(fd)` 与 `Task.waitWritable(fd)` 等待借用 descriptor 的一次 readiness，均返回可存储的 `Task[Unit]`。仍未终结的 Task 必须在词法 scope 结束前被 await、加入 join 或返回。`Task.all(...)` 等组合本身也只产生 Task，取得结果仍须显式 `.await`。tuple 与 list 不隐式互转；异构动态集合必须使用显式 enum/tagged union 或共同 `dyn C`，不得自动擦除为 `any`。

第一批真实异步资源 API 保持小而显式：`standard.file.open_read(path)`、`standard.file.create(path)` 与 `standard.net.connect(host, port)` 返回 Task；`File`/`Socket` 提供 `read_text`、`write_text`，并且必须立即绑定为 `scoped`。它们的 `close` 由 compiler-known 块级 cleanup 调用；对 scoped 变量手动 `close` 会被静态拒绝。

## 11. 当前没有写法的方向

Core 0.1 不定义或保留以下表面：

- AOP-like 注入、contribution、slot、flow；
- desired-state、operator、reconcile；
- capability、provider、effect；
- `example`、`scenario`、`property`；
- composition bundle；`loom.toml`、path/文件/HTTPS registry dependency、认证发布、lockfile、optional-dependency feature 与 bin/test/lib target 已由工具链定义；
- 第二套 trait/interface、concept conformance 之外的自由 extension declaration、继承、开放/多重派发和运行期实现发现。

`concept` 与显式 dyn receiver dispatch 已经进入 Core 0.2；GC、scoped/defer 与结构化 Task 已进入 Core 0.3，均不属于本节。其余方向不是被永久否决；它们必须先由独立的小例子闭合语义，再进入后续 Core 版本。
