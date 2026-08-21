# loom-lang Core 0.1 表面与代码风格

状态：Core Surface 0.1 / Confirmed Draft

证据等级：C0 草案（常用写法已确认，完整 lexical grammar 尚待闭合）

日期：2026-08-21

本文只规定 [Core 0.1](02-language-design-baseline.md) 已确认能力的常用写法。AOP-like 组合、desired-state/operator、capability 和专用 `example`/`scenario` 不在本文中，也不预留关键字。

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

精确 token grammar 和 diagnostic code 在 C1 parser 合同中补齐，但不得改变这条顶层同步原则。

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
- 返回类型写在参数列表之后，不使用 `->`；
- 泛型实参使用方括号，如 `Result[Price, Violation]`；
- 缺失值写作 `Option[T]`，Core 0.1 不引入 `T?` 糖；
- `let` 声明不可变局部，`var` 声明可变局部；
- block 的尾表达式是返回值，`return` 只用于提前返回；
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

## 5. 受约束值：`Price`

```loom
module shop.price

import standard.float.ParseFloatError
import standard.float.parse_float

pub type Price = Float where self >= 0.0

pub enum PriceInputError {
    InvalidNumber(ParseFloatError)
    OutOfRange(Violation)
}

pub fn parse_price(text Text) Result[Price, PriceInputError] {
    let raw = match parse_float(text) {
        Ok(value) => value
        Err(error) => return Err(PriceInputError.InvalidNumber(error))
    }

    match Price(raw) {
        Ok(price) => Ok(price)
        Err(violation) => Err(PriceInputError.OutOfRange(violation))
    }
}
```

这里的 `Price(raw)` 是 checked construction，其静态类型始终是 `Result[Price, Violation]`。编译器自动插入并执行 `where` 检查，但不会隐式抛异常、隐式传播错误或把 `Float` 当成 `Price`。

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

    invariant is_finite(self.subtotal)
        && is_finite(self.discount)
        && self.discount <= self.subtotal
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
test fn negative_price_is_rejected() {
    let rejected = match Price(-0.01) {
        Err(_) => true
        Ok(_) => false
    }

    assert rejected
}

test fn order_total_is_non_negative() Result[Unit, Violation] {
    let subtotal = match Price(100.0) {
        Ok(value) => value
        Err(problem) => return Err(problem)
    }
    let discount = match Price(20.0) {
        Ok(value) => value
        Err(problem) => return Err(problem)
    }
    let order_result = Order {
        subtotal = subtotal
        discount = discount
        note = None
    }
    let order = match order_result {
        Ok(value) => value
        Err(problem) => return Err(problem)
    }

    let total = order.total()
    assert total == 80.0
    Ok(Unit)
}
```

测试正常返回 `Unit` 或 `Ok(Unit)` 即通过；返回 `Err`、产生 ContractFault 或发生未处理缺陷即失败。

## 9. 当前没有写法的方向

Core 0.1 不定义或保留以下表面：

- AOP-like 注入、contribution、slot、flow；
- desired-state、operator、reconcile；
- capability、provider、effect；
- `example`、`scenario`、`property`；
- package、target、feature/bundle；
- trait、extension method、继承和动态派发。

这些方向不是被永久否决；它们必须先由独立的小例子闭合语义，再进入后续 Core 版本。
