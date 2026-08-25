# loom-lang Core 0.2：concept 与多态

状态：Confirmed Normative Design + LLVM C1 Executable Reference

日期：2026-08-26

本文规定 Loom 唯一的行为抽象 `concept`、静态泛型约束、默认接口参数和显式 `dyn C`。本版不引入所有权、借用、生命周期、`view[...]`、`box[...]` 或 `shared[...]` 语法。

## 1. 核心模型

Loom 只有一份 concept declaration 和一份显式 conformance：

```text
T: C       已知具体 T 的静态约束
C          参数位置的默认接口类型
dyn C      显式写出的类型擦除接口
```

- `T: C` 在泛型定义处检查，编译器可以单态化或传静态 witness；
- 参数 `value C` 接受任意满足 `C` 的 concrete value，源码不要求调用方先构造接口值；
- 参数位置的 `C` 与 `dyn C` 具有相同可观察语义，`C` 是惯用简写；
- 编译器可以对具体调用去虚化；即使必须保留类型擦除，也不要求物化某种固定形状的接口对象；
- `concept` 是名义合同，不做 structural typing、duck typing 或运行时 method lookup；
- `dyn` 不发现实现、不选择 provider，也不等同于反射、插件 registry 或 AOP。
- 本设计只借用 Go 的简洁参数书写，不采用 Go 的 method-set conformance、universal interface 或运行时类型查询模型。

当算法需要同一个具体类型贯穿多个参数、返回值或 associated projection 时，使用泛型：

```loom
fn smaller[T: Ordered](left T, right T) T
```

当函数只需要调用接口方法、不需要在签名中保留 concrete type identity 时，优先使用普通接口参数：

```loom
fn render(value Display) Text
```

用户不应为了性能在 `C` 与 `dyn C` 之间反复决策；静态化和去虚化是编译器职责。

## 2. declaration

普通 concept 可以声明 receiver method、`static method`、method 泛型和 associated type：

```loom
pub concept Ordered {
    method less_equal(self, other Self) Bool
}

pub concept Zero {
    static method zero() Self
}
```

concept requirement、inherent method 与 conformance method 和普通函数使用同一返回规则：省略返回类型固定为 `Unit`，不从 implementation body 推断；非 `Unit` requirement 必须显式声明返回类型。

能够形成接口值的 concept 必须在定义处承诺动态兼容：

```loom
pub dyn concept Formatter {
    associated type Error

    method format(self, text Text) Result[Text, Self.Error]
}
```

`dyn concept` 仍可用于静态泛型 bound。前置 `dyn` 只承诺其 requirements 满足第 7 节的擦除规则，不创建第二套抽象。

associated type 使用完整拼写：

```loom
pub dyn concept Source {
    associated type Item
    method next(mut self) Option[Self.Item]
}
```

## 3. 简洁签名

value parameter、字段和返回类型采用类似 Go 的 `name Type` 书写，但这只是表面语法，不改变 Loom 的显式名义 conformance。源码不使用全局冒号：

```loom
fn dynamic_format(
    formatter Formatter[Error = FormatError],
    text Text,
) Result[Text, FormatError]
```

冒号只保留在 generic bound：

```loom
fn same[T: Equivalent](left T, right T) Bool
```

多个 bound 使用 `+`，associated binding 使用方括号内的 `=`：

```loom
fn inspect[T: Ordered + Display](value T) Text
fn take_one(source Source[Item = Int]) Option[Int]
```

显式形式也合法：

```loom
fn take_erased(source dyn Source[Item = Int]) Option[Int]
```

在 parameter position，后两种 Source 写法不产生不同的业务语义。parameter 以外只有显式 `dyn C` 是接口值类型；它可以作为函数返回、record field、enum payload、tuple/list element 或普通泛型实参。裸 `C` 不在这些位置暗中变成类型，因此 API 是否长期保存擦除值仍能从签名直接读出，并且不需要 carrier/ownership 语法。

```loom
record Renderer {
    display dyn Display
}

fn erase(value Label) dyn Display {
    value
}
```

## 4. conformance 与 coherence

conformance 必须显式声明：

```loom
impl Formatter for IdentityFormatter {
    associated type Error = FormatError

    method format(self, text Text) Result[Text, FormatError] {
        Ok(text)
    }
}
```

规则如下：

- 每个 required associated type 和 method 恰好实现一次；
- 替换 `Self` 与 associated bindings 后，签名必须精确同型；
- conformance body 不得混入额外方法；额外行为放在 `impl T`；
- `impl C for T` 只能位于定义 `C` 或 outer nominal type `T` 的 module；
- 对一个具体 `(T, C)` 最多存在一个 conformance；
- import 只控制名字可见性，不注册、激活或排序 impl；
- 不支持 specialization、negative impl、file-local impl 或链接顺序优先级。

允许受约束的 conditional conformance：

```loom
impl[T: Equivalent] Equivalent for Boxed[T] {
    method equivalent(self, other Boxed[T]) Bool {
        self.value.equivalent(other.value)
    }
}
```

impl head 必须由本地 nominal constructor 锚定，参数必须能从 target 唯一确定；可能 overlap、递归证明不终止或 associated projection 成环时一律静态拒绝。

## 5. method resolution

dot call 的候选只来自：

1. concrete type 的 inherent method；
2. 当前静态签名明确提供的 concept bounds；
3. 接口参数自身的 concept。

编译器不得因为项目中“碰巧存在”某个 conformance，就让未声明 bound 的 generic body 获得方法。存在歧义时必须使用限定调用：

```loom
<T as Ordered>.less_equal(left, right)
```

`static method` requirement 同样通过限定形式调用。没有运行时按名字搜索、fallback 或 import-order resolution。

## 6. associated type

接口参数使用 associated binding 固定 erased ABI 中可见的 projection：

```loom
formatter Formatter[Error = FormatError]
source Source[Item = Int]
```

形成 erased interface 时，所有影响 method signature 的 associated type 必须绑定。binding 不完整、与 concrete conformance 不一致或含未解析 projection 都是静态错误。所有 generic 参数、associated bindings 和接口类型默认不变型。

## 7. 动态兼容

`dyn concept C` 的每个 requirement 必须同时满足：

- 是 `self` 或 `mut self` receiver method；
- method 本身没有类型参数；
- 除 receiver 外，参数和返回类型不出现未擦除的 `Self`；
- 使用的 associated type 能在接口类型中完全绑定；
- 不含 `static method`；
- ABI 中的全部类型都已有普通运行时表示。

不满足时在 concept 定义处报告，而不是让各调用点偶然得到不同结论。

`dyn C` 是普通一等值，可以存储、复制、嵌套和返回。普通 copy 产生独立的逻辑值及同一份不可变 conformance proof；复制后的可变 receiver 不得通过隐藏别名改变原副本。对象地址、call-scoped carrier storage 和 proof layout 都不可观察。

上述“普通一等值”不允许类型擦除隐藏 Core 0.3 的资源或任务 obligation。concrete source 若具有直接或递归 `MustScope` obligation、是未消费 `Task`，或其泛型形状尚不能证明不含两者，则不得适配成可自由保存或 discard 的 `dyn C`。checker 必须在建立 erased interface 时拒绝，而不是等到 `discard dyn_value` 后丢失 concrete obligation；Core 没有运行时 obligation registry，也不借此引入 ownership、borrow 或 move carrier。

因此，即使 conditional witness 已能完整传递，下面的通用擦除仍必须拒绝：

```loom
fn erase[T: Display](value T) dyn Display {
    value
}
```

`T: Display` 只证明 conformance，不证明任意 `T` 都没有递归 `MustScope` 或 Task obligation。当前源码也没有“不含资源/任务”的通用 bound；只有 checker 对 concrete source 形状已精确证明安全时，才能建立 `dyn C`。

对已经存储的 `dyn C` 调用 `mut self` requirement，receiver 必须是 `var` place，修改该接口值内部的 concrete data。同步函数的 concrete `var T` 实参自动适配到 mutable 接口参数时，编译器使用仅覆盖该次调用的 stable proxy/copy-in-copy-out carrier，正常返回后把修改提交回原 place。该 carrier 是 checked MIR/backend 所有的临时稳定 storage，不是嵌入 `dyn` 值的 caller 裸地址，不能被存储、返回、嵌套或跨 `.await`；源码仍不暴露 borrow、lifetime、`&mut` 或 token。异步调用的接口参数按拥有值复制进 Task frame，不保留 caller place 的回写 carrier。

## 8. 表示与派发

`dyn C` 只规定一条语义事实：值携带编译期已经选定的 concrete data 和 `T: C` conformance proof。它不规定 fat pointer、对象头、vptr、table prefix 或内存中的字段数量。

后端可以按优化上下文选择：

- concrete type 与 proof 已知时直接调用、内联或单态化，不构造接口对象；
- 将 data/address 与 witness 拆成独立 SSA value 或隐藏参数；
- 在间接派发仍存在时，使用当前 LLVM C1 的 compiler-private data/witness pair；
- 对单一 live implementation 使用专用 thunk，或让优化器消除未使用的 table/slot。

优化顺序固定为：先去虚化并消除接口表示，再特化调用签名只传仍未知的部分，其次传递分离的 SSA data/proof，最后才物化 pair。把接口值压成一个指针不是独立目标；对象头、existential box、inline container 或 tagged pointer 只是把另一部分信息移到别处。当前 LLVM C1 为真正存活的一等接口值使用 GC-managed concrete value 加 witness，mutable call carrier 仍是短生命周期 compiler-private 状态；后端可基于实测改成单指针或其他布局，但不得改变复制隔离、派发、DCE 或 fault 语义。

witness 可以降低成 table，也可以降低成单独函数引用或被完全常量折叠。当前 LLVM C1 物化为两层 compiler-private 表示：

```text
WitnessDescriptor {
    prerequisite_count,
    method_count,
    methods,                 // concept-local dense live slots
}

WitnessInstance {
    descriptor,
    prerequisites,           // ordered contiguous proof-pointer array
}
```

descriptor 和 method array 是 process-lifetime compiler globals；每个 concept 为本产物中真正可达的 requirements 编排自己的稠密 slots。instance 只指向 descriptor 和按声明顺序存放的连续 prerequisite pointer array，conditional application 保留 proof DAG 共享。这些布局不含 concrete type id、concept id 或运行时查找键。

无 prerequisite 的 concrete proof 可直接是 immutable global instance；同步临时 conditional proof 可以是 stack instance。需要成为 owned `dyn C` 的非全局 proof 会以单次事务深拷贝进入非移动 GC proof arena，collector 从 live `dyn` 的 witness 根递归 mark 并 sweep；跨 `.await` 的隐藏 proof 则按 Task 拥有的独立 proof slots 与 arena 保存。两种 clone 都只在本次操作中保留 source map，不持久缓存栈地址。

concrete record 不嵌入 C++ 风格 vptr；派发不依赖 Java 对象头、class loader、反射或全局 conformance registry。

layout、slot order、symbol name 和是否存在 materialized interface object 都是 compiler-private，不承诺 FFI/plugin 稳定性。任何表示都必须保持相同的值、mutation、ConstraintError、ContractFault 和 RuntimeFault。

## 9. contract 顺序

static、devirtualized 与 witness dispatch 共用同一调用合同：

1. concrete receiver 入口 invariant；
2. concept/inherent `requires`；
3. `old` 快照；
4. implementation body；
5. concrete receiver 出口 invariant；
6. `ensures`。

witness thunk 不得绕过 concrete invariant，也不得把 ContractFault 改写为业务 `Err`。

## 10. 可达性、DCE 与 `any`

native build 从 `main`、全部 `test fn` 或显式 export root 遍历：

```text
root → reachable function → erased construction → live witness → used method slot
```

仅声明 `impl C for T` 不会使其进入产物。只有可达代码实际传递/构造该 witness，相关 table 与 method 才成为 live；去虚化后无引用的 table 可以由 LLVM global DCE/LTO 删除。

Core 不存在 universal `any`，也明确禁止未来通过以下路径发现 conformance：

```text
A → any → dyn C   // 禁止运行时搜索 A 是否 conform C
```

未来即使加入 `any`，也只能先显式恢复 concrete `A`，或在包装点已经携带 `C` witness。反射/plugin registry 若将来出现，必须是单独的 open-world 功能，并明确把 registry 标记为 DCE roots；它不属于默认语言与构建模式。

未来若加入显式 concept refinement，只有 `dyn Derived -> dyn Base` 的已有 proof 投影可以自动发生；`dyn Base -> dyn Derived` 不得按 concrete type 或方法集合搜索更强 conformance。是否物化 table/pointer 不改变这条规则。

## 11. 诊断

稳定诊断至少包括：

| code | 含义 |
|---|---|
| `MissingConformance` | 没有可证明的 conformance |
| `DuplicateConformance` | 同一 `(T, C)` 重复 |
| `OverlappingConformance` | conditional impl 可能重叠 |
| `ConformanceResolutionCycle` | proof 搜索成环 |
| `DynNotDeclared` | 普通 concept 被用于 erased interface |
| `DynStaticRequirement` | dyn concept 含 static requirement |
| `DynGenericMethod` | dyn concept 含 generic method |
| `DynSelfLeak` | 非 receiver `Self` 不能擦除 |
| `DynAssociatedTypeUnbound` | associated binding 不完整 |
| `DynAssociatedTypeMismatch` | binding 与 conformance 不一致 |
| `DynMutReceiverUnavailable` | `mut self` 接口实参不是 `var` place；消息不得要求用户书写 view/borrow 语法 |
| `AssociatedBoundNotSatisfied` | associated type binding 未满足声明的 concept bounds |
| `AssociatedProjectionCycle` | associated type projection 直接或间接成环 |

## 12. 不在当前范围

- 除 `dyn C` 外的内建接口 carrier 语法；`view`、`box`、`shared` 只是普通标识符，没有迁移兼容或专用语义；
- 所有权、借用、lifetime、move-only 接口载体；
- universal `any` 到 concept 的运行时转换；
- dyn downcast、reflection、type registry、接口 intersection/upcast；
- stable dynamic-library/plugin/FFI witness ABI；
- concept inheritance、default method、specialization、negative impl；
- AOP、live/AST 编辑和 operator runtime。

这些方向不得反向改变当前普通源码、静态类型、合同、编译产物与工具链闭环。
