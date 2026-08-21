# loom-lang Core 0.2：concept 与显式动态多态

状态：Core 0.2 Static Concept + Borrowed Dyn / Confirmed Normative Design

证据等级：C0 规范（语义与表面已经确认，尚未实现）

日期：2026-08-21

本文是 Core 0.1 之后的下一份权威语言规范。它不改变 [Core 0.1](02-language-design-baseline.md) 的值、方法、invariant、合同和失败语义，而是在其上增加唯一的行为抽象 `concept`，以及同一 concept 的显式、词法 borrowed dyn 投影。

Core 0.2 的规范范围止于 C1e static concept 和 C1f `view[dyn C]` / `view[mut dyn C]`。`box[dyn C]` 与 `shared[dyn C]` 的显式载体方向已经选定，但其 affine、复制、销毁和泛型合同尚未闭合；它们在本文中只作为 **Accepted Direction / Non-Normative** 保存，不是 Core 0.2 可接受源码，也不预先命名实现阶段。

实现必须按 [能力分期](04-capability-stages.md) 分门验收。borrowed dyn 未完成不得阻塞 static concept；owned/shared 的 C0 所有权规范未闭合前，parser 必须拒绝相应类型和 construction。

## 1. 核心裁决

loom-lang 只保留一套行为抽象：

```text
T: C       已知具体 T 的静态约束
dyn C      隐藏具体 T 的存在类型投影
```

两者共享同一份 concept declaration 和同一份显式 conformance：

- `T: C` 表示对任意满足 `C` 的具体 `T` 都成立；编译期选择唯一 conformance，调用是静态派发；
- `dyn C` 表示存在某个隐藏的具体 `T`，值同时携带 `T: C` 的 witness；调用点不知道 `T`，通过 witness 动态派发；
- `concept` 是名义合同，不做结构化匹配、duck typing 或运行期 method lookup；
- 语言不再增加平行的 `trait`、`interface`、`protocol` 或 typeclass 关键字；
- 编译器可以单态化、传静态 dictionary 或 devirtualize，但不得改变上述可观察语义。

`dyn` 只增加 receiver dispatch。它不发现实现、不注入行为、不选择 provider，也不等同于 AOP、插件 registry、capability 或依赖注入容器。

## 2. concept declaration

### 2.1 普通静态 concept

```loom
pub concept Ordered {
    method less_equal(self, other Self) Bool
}

pub concept Zero {
    static method zero() Self
}
```

普通 `concept` 可以包含：

- `self` 或 `mut self` receiver method；
- method 自己的显式 rank-1 类型参数；
- `static method` requirement；
- `associated type` requirement；
- method 的 `requires` 和 `ensures`。

concept 不包含字段、存储状态、初始化代码或 default method body。Core 0.2 不包含 concept inheritance、associated const、GAT 或 concept 自身的类型参数。

`Self` 表示正在实现该 concept 的具体类型。普通 concept 可以在 receiver、其他参数、返回类型、泛型嵌套位置和 associated projection 中使用 `Self`。

### 2.2 承诺动态投影的 concept

```loom
pub dyn concept Formatter {
    associated type Error

    method format(self, document Document)
        Result[Text, Self.Error]
}
```

`dyn concept C` 仍然可以用于 `T: C` 的静态泛型。前置 `dyn` 只增加一项公开承诺：`C` 的全部 requirement 都满足第 8 节的动态兼容规则，因此可以形成 `dyn C`。

普通 `concept C` 即使暂时碰巧满足动态兼容规则，也不得被擦除成 `dyn C`。这使动态兼容成为定义处可检查、可版本化的 API，而不是某个调用点的偶然推断。

### 2.3 associated type

associated type 使用完整拼写：

```loom
pub concept Source {
    associated type Item

    method next(mut self) Option[Self.Item]
}
```

如果 associated type 本身需要满足 concept，写作：

```loom
associated type Item: Equatable
```

不使用裸 `type Item`：`type Price = Float where ...` 创建具体名义类型，而 `associated type` 是由 conformance 选择的抽象类型槽，两者应在源码中保持可辨认。

Core 0.2 没有 associated type default、associated const 或 generic associated type。

## 3. 泛型约束与静态派发

### 3.1 单个与多个 bound

类型参数的唯一约束写法是冒号；多个 concept 使用 `+`：

```loom
fn smaller[T: Ordered](left T, right T) T {
    if left.less_equal(right) {
        left
    } else {
        right
    }
}

fn inspect[T: Ordered + Show](value T) Text {
    value.show()
}
```

Core 0.2 不再提供 trailing `where T: C` 的等价写法。`type Price = Float where predicate` 中的 `where` 继续只表示值约束。

泛型函数在定义处只依据签名中的 bounds 检查。调用点传入的具体类型即使还有 inherent method 或其他 conformance，也不能让泛型 body 获得未声明能力。

### 3.2 associated binding 与 projection

associated binding 写在 concept 引用的方括号中：

```loom
fn read_one[S: Source[Item = Text]](source S) Option[Text] {
    var current = source
    current.next()
}
```

泛型代码可以用 `T.Item` 表示唯一可解析的 associated projection。若多个 bounds 都声明同名 associated type，必须使用限定形式：

```loom
<T as LeftConcept>.Item
```

不能唯一解析的未限定 projection 是静态错误。

### 3.3 static requirement

没有 receiver 的 requirement 必须显式写成 `static method`，并在调用处给出选择 conformance 的类型：

```loom
pub concept Zero {
    static method zero() Self
}

fn make_zero[T: Zero]() T {
    <T as Zero>.zero()
}
```

static requirement、返回 `Self` 的 requirement 和接收非 receiver `Self` 的 requirement 都适合静态泛型，但不能进入 dyn concept。

method-specific 类型实参沿用方括号，并可用于 dot call 或限定调用：

```loom
value.convert[Text](target)
<T as Convertible>.convert[Text](value, target)
```

省略 method type arguments 时，只按 Core 0.1 普通泛型函数规则从 value arguments 推断，不从返回类型猜测，也不设置默认类型；没有唯一解时静态失败。

## 4. 显式 conformance

### 4.1 声明

conformance 使用：

```loom
impl Ordered for Price {
    method less_equal(self, other Self) Bool {
        self <= other
    }
}
```

带 associated type：

```loom
impl Formatter for JsonFormatter {
    associated type Error = FormatError

    method format(self, document Document)
        Result[Text, FormatError]
    {
        format_json(document)
    }
}
```

inherent method 仍写成：

```loom
impl Price {
    method display(self) Text {
        format_price(self)
    }
}
```

`impl C for T` 和 `impl T` 是两种不同声明。同名同签名 inherent method 不会自动满足 concept；conformance body 可以显式转发给 inherent method。

### 4.2 requirement 完整性

一个 conformance 必须：

- 恰好一次绑定每个 associated type；
- 恰好一次实现每个 required method；
- 在替换 `Self = T` 和 associated bindings 后与 requirement 精确同型；
- 满足 associated type 上声明的 concept bounds；
- 不改变 receiver mode、增加参数或扩大返回错误类型。

generic requirement 的“精确同型”包括 method 类型参数的数量、rank、每个 bound、receiver、value 参数和返回类型；只允许类型参数改名后的 alpha-equivalence。concept contract 不在 impl 中重复，而是原样继承，因此 impl 也没有声明另一份 contract 的位置。impl 不得增加或删除 method bound。`static method` requirement 必须由同样的 `static method` 实现，不能改成 receiver method。

conformance body 不能混入额外方法；额外行为必须放进 `impl T`。

### 4.3 orphan rule 与 coherence

Core 0.2 采用 owner-orphan rule：

> `impl C for T` 只能声明在定义 `C` 的 module 或定义名义类型 `T` 的 module 中。

因此：

- `Price` 的 owner 可以让 `Price` conform 标准库 concept；
- 用户 concept 的 owner 可以为 `Int`、`Option[Order]` 或 `Vec[T]` 提供该 concept 的适配；
- 当当前 module 既不拥有 concept 也不拥有 outer nominal type 时，必须定义本地 nominal wrapper；
- conformance 没有 file-local、import-local 或 target-local 版本；
- import 只让名字可见，不注册、激活或选择 impl。

每个具体 `(T, C)` 最多有一个 conformance。associated type binding 不是第二个 instance 的区分键；重复 conformance 必须同时定位所有候选并拒绝，不能按 import、链接、文件或声明顺序择胜。

### 4.4 generic 与 conditional conformance

允许 concept owner 或 type owner 为一个具名泛型类型声明 conformance：

```loom
impl[T: Equatable] Equatable for Boxed[T] {
    method equal(self, other Self) Bool {
        self.value.equal(other.value)
    }
}
```

限制：

- 当前 module 必须拥有 concept 或 outer nominal constructor `Boxed`；
- impl head 的全部类型参数必须能从目标类型唯一确定；
- 对同一 concept 的任意两个 impl，若 target heads 可能统一，则全部拒绝；不同 concepts 不参与彼此的 overlap 检查；
- 编译器不能靠“更具体”或声明优先级选择实现；
- 不支持 specialization、negative impl 或 foreign blanket impl；
- `impl[T] C for T` 非法，因为 target 没有当前 module 拥有的 outer nominal constructor。

impl target 必须形如一个具名 nominal constructor 应用；所有 impl 参数都必须从该 head 唯一确定，否则报告 `UnconstrainedImplParameter`。conformance 条件只允许这些参数上的正向 concept bounds 与 associated equality，不允许否定、析取或用户 predicate；每个递归 prerequisite 必须作用于 target 的真结构子项。

解析必须是有限、确定的证明：同一 proof obligation 再次出现在自己的求解栈中时报告 `ConformanceResolutionCycle`，不能递归猜测；associated projection 成环同样拒绝。

无法证明两个 conditional conformance 不重叠时，checker 必须保守拒绝。

## 5. concept method resolution

对：

```loom
value.method(arguments)
```

checker 收集：

- inherent method；
- 当前泛型 bounds 保证的 concept methods；
- 当前 module 声明或由源码显式 import 的 concept methods，并要求 concrete type 具有唯一 conformance。

必须恰好有一个适用候选。语言不规定 inherent method 总是获胜，也不按 import 顺序择胜；多个候选是 `AmbiguousConceptMethod`。

这里“可见”只由当前 module 和显式 concept import 决定，不扫描整个程序的 concept。import 只开放 method namespace，不选择或激活 conformance；新增 import 或新增可见 method 可能产生诚实的 source ambiguity，但不能静默改变被调用实现。

使用完全限定调用消歧：

```loom
<Price as Ordered>.less_equal(left, right)
<T as Ordered>.less_equal(left, right)
```

限定调用只要求 `T` 与 `C` 的名字可解析以及唯一 conformance 存在，不要求把 `C` 的 methods 加入 dot-call namespace。

对 dyn carrier，静态类型只暴露其 `dyn concept` 的 methods；concrete inherent methods 和其他 conformances 都已经隐藏。

## 6. concept contract 与 concrete invariant

concept method 可以声明调用合同：

```loom
pub concept Indexed {
    associated type Item

    method at(self, position Int) Option[Self.Item]
        requires position >= 0
}
```

concept declaration 上的 `requires` 和 `ensures` 是唯一公共合同，适用于所有静态和动态调用。conformance implementation：

- 不重复、删除或改写 concept contract；
- 不得加强 `requires`；
- 可以在 body 内使用 `assert` 表示额外实现事实。

receiver 为带 invariant 的 concrete record 时，检查顺序固定为：

```text
concrete receiver 入口 invariant
→ concept requires
→ 捕获 old(...)
→ conformance body
→ concrete receiver 出口 invariant
→ concept ensures
```

该顺序在静态派发和 borrowed dyn 下完全相同。witness thunk 不得因为类型擦除而绕过 concrete invariant；未来 owned/shared dyn 若进入规范，也必须保持同一顺序。

`static method` 没有 receiver invariant，按 free fn 的既有顺序执行：`requires → old snapshot → body → ensures`。

concept 文档可以描述交换律等代数定律，但没有被可执行合同表达的 law 不是编译器事实，不能据此自动重排、并行或消除调用。

## 7. 动态投影的语义

```text
dyn C[A = X]
```

表示：

```text
exists Hidden.
    Hidden value
  + unique witness: impl C for Hidden
  + proof: Hidden.A == X
```

不同 concrete types 可以进入同一个 dyn carrier，只要它们：

- 都显式 conform `C`；
- associated bindings 精确相同；
- 使用同一种 carrier。

hidden type 不可由普通程序观察、恢复或用于分支。

## 8. dyn compatibility

### 8.1 定义处检查

`dyn concept C` 的每个 method 必须同时满足：

1. 有 `self` 或 `mut self` receiver；
2. 没有 method-specific 类型参数；
3. 不是 `static method`；
4. `Self` 只能出现在 receiver 位置；
5. 参数和返回值不得直接或嵌套包含 `Self`；
6. 可以使用 `Self.AssociatedType`；
7. associated type 不是 GAT；
8. 替换全部 associated bindings 后，签名是确定的普通函数类型。

合法：

```loom
pub dyn concept Source {
    associated type Item

    method next(mut self) Option[Self.Item]
}
```

不合法：

```loom
pub dyn concept Additive {
    static method zero() Self
    method add(self, other Self) Self
    method convert[T](self, value T) T
}
```

Core 0.2 不提供 `where Self: Sized`、`static only` 或单 method 的 dyn 豁免。需要这些 requirement 时，应保留普通静态 concept，或拆出一个独立 dyn concept。

### 8.2 associated binding

每个 dyn use 必须显式绑定 `C` 的全部 associated types：

```loom
dyn Source[Item = Text]
dyn Formatter[Error = FormatError]
```

即使当前调用的方法没有使用某个 associated type，也不得省略。创建 dyn value 时，concrete conformance 的 binding 必须精确相等。

binding 可以引用外层已知类型参数：

```loom
fn consume[T](source view[dyn Source[Item = T]]) Option[T]
```

## 9. dyn carrier

裸 `dyn C[...]` 只是 existential target，不是完整的值类型。Core 0.2 可接受源码必须选择 borrowed carrier：

```loom
view[dyn C[A = X]]
view[mut dyn C[A = X]]
```

未来拥有型方向已经选为：

```loom
box[dyn C[A = X]]
shared[dyn C[A = X]]
```

| carrier | 状态 | 逻辑所有权 | 可复制 | 可调用 receiver | 主要用途 |
|---|---|---|---:|---|---|
| `view[dyn C]` | Core 0.2 normative | 非拥有、只读借用 | 仅在同一词法 region 内 | `self` | 临时参数和局部调用 |
| `view[mut dyn C]` | Core 0.2 normative | 非拥有、独占 inout 借用 | 否 | `self`、`mut self` | 临时可写适配 |
| `box[dyn C]` | accepted direction | 唯一拥有 hidden value | move-only | `self`；`var box` 还可 `mut self` | 返回值、字段、异构容器 |
| `shared[dyn C]` | accepted direction | 共享拥有 hidden value | handle 可复制 | 仅 `self` | 多处长期持有的只读接口 |

裸 `dyn C`、所有权不明的隐式擦除以及 concrete-to-dyn 隐式 coercion 都是静态错误。Core 0.2 parser 还必须以稳定的 feature-not-available 诊断拒绝 `box[dyn C]` 和 `shared[dyn C]`。

### 9.1 显式 construction

```loom
let borrowed =
    view[dyn Formatter[Error = FormatError]](json_formatter)
```

构造时 checker 必须验证 concrete type、唯一 conformance、全部 associated bindings、`dyn concept` 标记以及 carrier 的 place/ownership 条件。

### 9.2 readonly view

`view[dyn C]`：

- Core 0.2 只能从仍存活的 concrete lvalue 建立；未来拥有型规范可以增加 box/shared owner；
- 不取得所有权，只能调用 `self` method；
- C1f 只允许作为参数或词法 local；
- 不能放进 record/enum、返回、捕获或转成 owning carrier；
- 不能作为用户泛型类型实参、associated binding 或其他类型的嵌套成分；
- owner 在 view region 内可以读取和建立更多 readonly views，但不能写入、调用 `mut self`、建立 mutable view、移动或销毁。

C1f 不引入显式 lifetime 参数，也不做 non-lexical lifetime 推断。参数 view 的 region 是本次调用；直接作为调用实参构造的临时 view 持续到该调用结束；绑定为 local 的 view 持续到所在 block 结束。上述保守限制使最小 lifetime 可判定。

### 9.3 mutable view

`view[mut dyn C]`：

- Core 0.2 只能从 concrete `var` place 建立；未来拥有型规范可以增加 `var box[dyn C]`；
- 在 view 生存期内独占该 place；
- 原 place 不能读、写、移动或再次借用；
- mutable view 是编译器已知的不可复制词法令牌；用它初始化另一 binding、赋值或作为 value argument 传递时会移动令牌，旧 binding 立即不可再使用；
- 移动令牌不会缩短原先按第 9.2 节确定的 borrow region；Core 0.2 不提供 mutable-view reborrow；
- carrier 自身携带独占可写权限，因此即使它是 `let` local 或普通 parameter binding，也能作为合法 inout place 调用 `mut self`；这不会允许重新赋值该 `let` binding；
- concrete invariant 隔离和入口/出口检查继续生效；
- 未来也不得从 `shared[dyn C]` 建立；
- 与 readonly view 一样，不能作为泛型类型实参、associated binding、嵌套字段、返回值或 capture。

因此 `let second = first` 之后再用 `first`，以及 `consume(first)` 后再用 `first`，都必须报告 `UseAfterViewMove`。直接构造为函数实参的 mutable view 只持续到该调用结束。由于 mutable view 不能进入用户泛型、aggregate、返回值或 capture，这条受限移动规则不是对普通 Core 类型偷偷引入通用 affine 泛型；后者仍属于 owning carrier C0。

### 9.4 Accepted Direction / Non-Normative：box

目标表面为：

```loom
let owned =
    box[dyn Formatter[Error = FormatError]](JsonFormatter {})
```

`box[dyn C]`：

- 唯一拥有 hidden value；
- construction 消费 concrete value；
- 赋值、传参或返回会移动 box，移动后使用原 binding 是静态错误；
- `let box` 只能调用 `self` method；
- `var box` 可以调用 `self` 和 `mut self` method；
- 可以显式借成 readonly 或 mutable view；
- 没有隐式 deep clone。

在它成为规范前，必须冻结最小 affine/move/drop rule、包含 box 的 aggregate 规则，以及无约束泛型面对 move-only type 的定义处检查。不能把 box 按普通可复制 record 偶然实现。

### 9.5 Accepted Direction / Non-Normative：shared

目标表面为：

```loom
let shared_formatter =
    shared[dyn Formatter[Error = FormatError]](JsonFormatter {})
```

`shared[dyn C]`：

- 共享拥有一个 hidden value；
- 从 concrete value 或 box construction 时消费输入 owner；
- 复制 handle 会建立另一位共享 owner；
- 所有 handle 观察同一 hidden value；
- 只能调用 `self` method，不能取得 mutable view；
- 没有自动 identity equality；
- 可以显式借成 readonly view。

Core 当前禁止 interior mutability。`shared` 不自动表示线程安全、可跨 async task 或具有效果权限；这些能力必须以后显式加入。

### 9.6 carrier 转换

Core 0.2 允许：

```text
concrete lvalue      -> view[dyn C]
concrete var place   -> view[mut dyn C]
```

Core 0.2 禁止 view 变成 owner、shared 或 concrete type。

拥有型规范未来若闭合，预期再加入：

```text
concrete value       -> box[dyn C]
concrete value       -> shared[dyn C]
box[dyn C]           -> view[dyn C]
var box[dyn C]       -> view[mut dyn C]
shared[dyn C]        -> view[dyn C]
box[dyn C]           -> shared[dyn C]    // 消费 box
```

即使未来拥有型载体加入，仍禁止：

```text
view        -> box/shared
shared      -> box
shared      -> mutable view
dyn C       -> concrete T
dyn C       -> unrelated dyn D
```

即使 hidden type 还 conform 其他 concept，也不能从 `dyn C` 动态发现或切换到 `dyn D`。dyn intersection 和 upcast 后置。

## 10. 调用和运行时边界

```loom
fn produce(
    formatter view[dyn Formatter[Error = FormatError]],
    document Document,
) Result[Text, FormatError] {
    formatter.format(document)
}
```

动态调用前，编译器已经确定 method、receiver mode、参数类型、返回类型、associated substitution 和 witness slot。运行时只读取 carrier 已有的 witness；不得扫描 registry、按名字搜索、根据环境选择实现或重新执行 conformance 检查。

`dyn C` 不自动满足泛型 bound `T: C`，也不做隐式 existential opening。需要泛型算法时使用 concrete/static path；需要动态异构时显式接受相应 dyn carrier。

所有 generic 参数、associated bindings 和 dyn carriers 默认不变型。`dyn Source[Item = Dog]` 不是 `dyn Source[Item = Animal]` 的子类型。

Core 0.2 不提供 dyn downcast、type id、反射、隐式 equality、ordering、hash、serialization 或 pointer identity。需要业务身份时，concept 应显式返回领域 key，而不是暴露 hidden type 或地址。

witness/vtable 的布局、成员顺序和地址是 compiler-private；源码声明顺序不产生稳定 ABI。Core 0.2 不承诺跨编译器版本、动态库、plugin 或 FFI 的 dyn ABI。

## 11. 数值、序列与服务的使用原则

### 11.1 不定义 `concept Int`

`Int`、`Float` 是 concrete types，不再复用为 concept 名。标准库行为抽象应按能力命名，例如候选的 `Additive`、`Zero`、`Ordered`、`Sequence`、`RandomAccess`，而不是建立一棵笼统的 `Number` 继承树。

Core 0.2 只冻结语言机制，不立即冻结标准库数值/容器 concept taxonomy，也不把内建 `+`、`==` 或索引语法改写成用户 concept dispatch。operator overloading、数值定律、溢出和 totality 必须分别有可执行合同后才能进入标准库规范。

### 11.2 数值默认静态

数值 concept 经常包含 `other Self`、返回 `Self`、`static method zero()`，并且调用密集：

```loom
pub concept Additive {
    static method zero() Self
    method add(self, other Self) Self
}
```

它天然适合 `T: Additive`，也天然不满足 dyn compatibility。不能为了接口形式统一而强行擦除。

### 11.3 序列默认静态，异构时才 dyn

```loom
pub concept Source {
    associated type Item
    method next(mut self) Option[Self.Item]
}
```

算法默认接受 `S: Source[Item = T]`。只有确实需要异构 source 或运行期替换时，owner 才应把它声明为 `dyn concept Source`，并在 Core 0.2 使用 `view[mut dyn Source[Item = T]]`；长期持有等待未来 owning carrier 规范。

### 11.4 服务接口适合 dyn，但 dyn 不等于 capability

请求/响应类型固定、调用方不需要知道 adapter 的接口通常适合 dyn：

```loom
pub dyn concept OrderReader {
    associated type Error

    method get(self, id OrderId)
        Result[Option[Order], Self.Error]
}
```

但 `dyn concept` 只解决接口值和派发，不负责：

- 发现或选择实现；
- provider 生命周期和依赖注入；
- I/O/effect 权限；
- async、并发或线程安全；
- retry、transaction 或 durable state。

这些仍是独立设计问题。

## 12. 版本兼容性

- 向 concept 增加 required method 会使现有 conformance 源码不完整，是 breaking change；
- 修改 associated type、receiver mode、参数、返回或 contract 是 breaking change；
- 把普通 concept 改成 dyn concept 只有在全部 requirement dyn-compatible 时才合法，但会新增公开动态表面；
- 从 dyn concept 移除 `dyn` 会使所有 dyn carrier API 失效，是 breaking change；
- 给 dyn concept 增加非 dyn-compatible requirement 必须在定义处拒绝，不能静默让旧 dyn API 消失；
- 改变 conformance 选择不得由 import、链接或文件顺序驱动。

## 13. 必须提供的诊断

至少稳定区分：

| code | 条件 |
|---|---|
| `MissingConformance` | `T` 未显式 conform `C` |
| `ForeignConformance` | impl 不在 `C` 或 `T` owner module |
| `DuplicateConformance` | 同一具体 `(T, C)` 重复 |
| `OverlappingConformance` | conditional impl 可能重叠 |
| `IncompleteConformance` | 缺 method 或 associated type |
| `ConformanceSignatureMismatch` | receiver/参数/返回不匹配 |
| `AssociatedBoundNotSatisfied` | binding 不满足 concept bound |
| `AssociatedProjectionCycle` | associated projection 递归成环 |
| `AmbiguousConceptMethod` | dot call 有多个候选 |
| `DynNotDeclared` | 普通 concept 被用于 dyn |
| `DynStaticRequirement` | dyn concept 含 static method |
| `DynGenericMethod` | dyn concept 含 generic method |
| `DynSelfLeak` | `Self` 出现在非 receiver 位置 |
| `DynAssociatedTypeUnbound` | dyn use 未绑定全部 associated types |
| `DynAssociatedTypeMismatch` | concrete binding 与 dyn binding 不同 |
| `DynMutReceiverUnavailable` | readonly view 调用 `mut self` |
| `IllegalDynConversion` | carrier 转换方向非法 |
| `ViewEscape` | lexical view 被返回、存储或捕获 |
| `DynViewInGeneric` | view 被用作泛型实参、associated binding 或嵌套类型 |
| `BorrowConflict` | mutable view 存在时再次访问 owner |
| `ReadonlyBorrowConflict` | readonly view 存在时写入、移动或可写借用 owner |
| `UseAfterViewMove` | mutable view 的借用令牌已经由赋值或传参移动 |
| `DynCarrierRequired` | 裸 `dyn C` 缺少 carrier |
| `AssociatedProjectionAmbiguous` | 未限定 projection 对应多个 concepts |
| `UnconstrainedImplParameter` | impl 参数不能从 nominal target head 决定 |
| `ConformanceResolutionCycle` | conditional conformance 证明不终止 |
| `IllegalDynAbiBoundary` | dyn carrier 出现在尚未支持的稳定 ABI/FFI 边界 |
| `DynOwnedCarrierUnavailable` | Core 0.2 源码使用尚未规范化的 box/shared dyn |

诊断必须包含 concrete type、concept、相关 impl 位置、associated substitution、具体不兼容 requirement 和全部冲突候选；不得只报告笼统的 “not object safe” 或 “type mismatch”。

LSP 必须支持 go-to-concept、go-to-impl、find implementations，并在 hover 中明确本次调用是 static conformance 还是 dynamic dispatch。

## 14. 明确后置

Core 0.2 不包含：

- structural/implicit conformance；
- concept inheritance、alias、intersection 或 dyn upcast；
- default method body、specialization、negative impl；
- foreign blanket impl；
- associated const/default、GAT 或 HKT；
- `where Self: Sized` 和单 method dyn 豁免；
- existential opening、downcast、type id、reflection；
- stable plugin/FFI dyn ABI；
- 显式 lifetime 参数、返回 view、weak shared、cycle collection 或 user destructor；
- interior mutability、锁、actor、Send/Sync；
- async concept method；
- operator overloading；
- effect、capability/provider、自动 DI、registry 或 service locator。

这些能力不能由编译器实现方便或某个标准库需求自行补入。

## 15. 实现证据门

### C1e：static concept

分两个 slice 实现并验收：

**C1e.1 nominal static kernel**：

- receiver-only `concept`、concrete `impl C for T`、单一 `T: C`；
- owner rule、唯一性、限定调用和 concept contract；
- 一个不依赖 associated type 或 operator 的静态 fixture。

**C1e.2 abstraction closure**：

- associated type、static/generic/receiver requirements；
- 多个 bounds、binding 与 projection；
- owner-orphan coherence 与 conditional generic conformance；
- method resolution、限定调用与 concept contracts；
- 数值和序列各一个纯静态 fixture。

不实现任何 dyn value。

### C1f：dyn + lexical view

分两个 slice 实现并验收：

**C1f.1 readonly dyn view**：

- `dyn concept` 定义处 compatibility checker；
- associated binding；
- readonly lexical view、witness dispatch 和 non-escape；
- concrete invariant/contract thunk；
- 一个纯 readonly Formatter fixture。

**C1f.2 mutable dyn view**：

- mutable lexical view 与 `mut self` dispatch；
- 独占 borrow、owner freeze 和 conflict diagnostics；
- 一个 mutable Source fixture。

这是最小可用 dyn 闭环，不引入 heap ownership。

### Owned carrier C0 closure（尚未命名 Core 版本）

在命名实现阶段前，必须先形成独立的规范裁决：

- `box` 的 affine move、drop、异常退出和 use-after-move；
- `shared` handle copy、销毁、hidden identity 与 cycle 边界；
- aggregate 含 owning carrier 时的 copy/move 性质；
- 无约束泛型 `T` 面对可能 move-only type 的定义处检查；
- owner/view 转换和完整诊断；
- 异构集合与长期持有接口 fixture。

在这些问题闭合前，`box/shared` 只是已选方向，不是 C1g 或任何可实施 Core 的承诺，也不能以运行库原型代替源码级所有权诊断。
