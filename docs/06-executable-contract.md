# loom-lang Core 0.1–0.3 可执行合同

状态：Executable Contract / Implemented by C1 Reference

证据等级：C1 executable core（Core 0.1–0.3 fixture 与 artifact 已闭环）

日期：2026-08-25

本文闭合 [Core 0.1 语义基线](02-language-design-baseline.md)、[Core 0.1 表面](03-surface-and-style.md)、[Core 0.2 concept/dyn 规范](05-concepts-and-dynamic-polymorphism.md)和 [Core 0.3 GC/cleanup/Task 规范](08-memory-cleanup-and-async.md)留给 parser、checker、编译器和工具的可执行选择。它只把已确认语义降到 token、grammar、failure envelope 和 artifact 合同，**不增加任何语言能力**。

权威分工如下：

- `02` 定义 Core 0.1 可观察语义；
- `03` 定义惯用表面和恢复不变量；
- `05` 定义 Core 0.2 static concept、接口参数与 erased dyn 语义；
- `08` 定义 Core 0.3 GC、词法清理、Task 与 coroutine 语义；
- 本文定义实现必须共享的可执行合同。

如果旧示例的换行方式与本文冲突，formatter 和 parser 以本文为准。旧 `view[...]`、`box[...]`、`shared[...]` 及其他所有权 carrier 不是当前源码语法，parser 必须稳定拒绝。

## 1. 源码与标识符

### 1.1 编码和字符

源文件必须是合法 UTF-8；非法 byte sequence 报 `InvalidUtf8`，不做替换字符恢复。文件可以有且仅有一个位于 byte 0 的 UTF-8 BOM；其他位置的 BOM 报 `InvalidSourceCharacter`。除该可选 BOM、字符串和注释外，只接受：

- Unicode XID 标识符字符；
- ASCII 语法 token；
- ASCII 空白；
- LF 或 CRLF 换行。

编译器必须在 version metadata 中公开自己使用的 Unicode XID 数据版本。编译身份按源码中的 Unicode scalar sequence 精确比较；不做隐式 normalization、case folding 或 confusable folding。

### 1.2 标识符

标识符 grammar 为：

```text
identifier-start    := "_" | XID_Start
identifier-continue := "_" | XID_Continue
identifier          := identifier-start identifier-continue*
```

ASCII keyword 在标识符分类后做精确、大小写敏感匹配。单独的 `_` 是 wildcard token，不是可绑定的名字；`_value` 是普通标识符。名称风格仍由 `03` 规定，不等同于词法拒绝。

### 1.3 空白和换行

非换行 ASCII 空白是 `U+0009 TAB`、`U+000B VT`、`U+000C FF` 和 `U+0020 SPACE`。换行只能是：

```text
NL := U+000A | U+000D U+000A
```

CRLF 在 lexer 中产生一个 `NL` token。单独 CR 报 `InvalidLineEnding`；其他 Unicode whitespace 报 `InvalidSourceCharacter`。lexer **必须保留 `NL`**，由 parser 按第 3 节决定是 trivia、separator 还是 error-island boundary。

## 2. 注释、字符串与数值 token

### 2.1 注释

- `//` 开始普通行注释；
- `///` 开始文档行注释，连续 doc lines 附着后面第一条声明；
- 注释在 `NL` 前结束，不消费 `NL` token；
- `//` 在字符串内只是文本；
- Core 不接受 block comment。

一个空白行会结束 doc-comment attachment；没有被合法声明消费的 `///` 报 `OrphanDocComment`。

### 2.2 字符串

普通字符串使用双引号，不能跨 LF 或 CRLF。原始 U+0000–U+001F 控制字符不得直接出现。允许的 escape 仅有：

```text
\"  \\  \/  \b  \f  \n  \r  \t  \0
\uXXXX
\u{H...}
```

`\uXXXX` 使用恰好四个 hex digits；UTF-16 surrogate 必须以合法 high/low pair 出现，单独 surrogate 拒绝。`\u{H...}` 使用 1–6 个 hex digits，结果必须是不在 surrogate range 内且不大于 `U+10FFFF` 的 Unicode scalar。`\0` 表示 U+0000。

字符串在 EOF 前未闭合报 `UnterminatedString`，在物理换行前未闭合报 `NewlineInString`；未知 escape 报 `InvalidEscape`，损坏或越界的 Unicode escape 报 `InvalidUnicodeEscape`。错误字符串在当前物理行结束 error island，不吞后续声明。

### 2.3 Int 与 Float literal

token grammar 为：

```text
DIGITS         := [0-9]+
EXPONENT       := [eE] [+-]? DIGITS
INT_LITERAL    := DIGITS
FLOAT_LITERAL  := DIGITS "." DIGITS EXPONENT?
                | DIGITS EXPONENT
```

不接受 digit separator、radix prefix、suffix、`.5`、`1.`、`NaN`、`Inf` 或 `Infinity` literal。lexer 采用 longest match。负号不属于 literal；`-1` 和 `-1.0` 都是 unary `-` 应用于正 literal。

Int token 先保留精确十进制 magnitude，checker 再做 i64 range check。`9223372036854775808` 只能作为直接 unary `-` 的 operand 来表示 `Int.MIN`；其他上下文报 `IntegerLiteralOutOfRange`。

Float literal 按 IEEE 754 round-to-nearest, ties-to-even 转换为 binary64。数学值溢出到 infinity 报 `FloatLiteralOutOfRange`；正确 rounding 到 subnormal 或正负零是合法的。NaN 和 infinity 可以由运算或标准库建立，但没有源码 literal。

## 3. `NL`、continuation 与 separator

lexer 总是发出 `NL`，parser 按下列唯一规则消费：

1. 在 `(...)` 和 `[...]` 内，`NL` 始终是 trivia；列表元素仍必须用 `,` 分隔。
2. 在 `{...}` 内，`NL` 默认是 separator。
3. 一个 `NL` 的前一个非 trivia token 若为 opening delimiter `(`、`[` 或 `{`，该 `NL` 是 continuation trivia。
4. 前一个非 trivia token 若为需要右 operand 的 operator，该 `NL` 是 continuation trivia。该组是 `.`、unary `!`、unary/binary `-`、`+`、`*`、`/`、`<`、`<=`、`>`、`>=`、`==`、`!=`、`&&`、`||`、assignment `=` 和 match `=>`。
5. 换行前置 operator 不会 continuation；`left NL + right` 不等价于 `left + right`。formatter 必须把跨行 infix operator 放在前一行末尾。
6. fn/method/concept-method 声明头中，参数列表后的可选返回类型、`requires`/`ensures` 和 body opening `{` 是同一 declaration 的 grammar；它们之间的 `NL` 是 declaration trivia，不会把声明截断。返回类型缺失时固定为 `Unit`，不是 body inference。

分隔规则：

- top-level declaration 由一个或多个 `NL` 分隔，top level 不接受 `,`；
- block statement、record field/literal field、enum variant、impl/concept member 可以用一个或多个 `NL` 或一个 `,` 分隔；
- match arm 可以用 `NL` 或 `,` 分隔；
- parameter、argument、generic argument/binding 使用 `,`，其中 `NL` 只是 trivia；
- closing `}` 前允许 trailing separator；
- 一个或多个 `NL` 构成一个 separator run；`,` 后紧随的一个或多个 `NL` 属于同一个 separator run，不产生空 item/statement；
- `,` 不是 expression operator；分隔符只在对应 grammar context 中合法；
- `;` 在所有上下文都报 `SemicolonNotSupported`。

递归语法 wrapper 的公开上限为 128，nesting contract version 为 2。atomic expression/type/pattern 不消耗预算；unary、delimiter recursion、type/pattern payload，以及 call/member 等迭代构造消耗预算。超限必须产生 `SyntaxNestingLimit`、保持 token lossless，并恢复到后续声明；不得发生进程栈溢出。

## 4. expression、statement、`if` 与 `match`

### 4.1 precedence 和 associativity

由高到低：

| 级别 | 形式 | associativity |
|---|---|---|
| postfix | call、field/method access、显式 generic application | left |
| unary | unary `-`、`!` | right |
| multiplicative | `*`、`/` | left |
| additive | `+`、`-` | left |
| relational compare | `<`、`<=`、`>`、`>=` | non-associative |
| equality | `==`、`!=` | left |
| boolean and | `&&` | left, short-circuit |
| boolean or | `||` | left, short-circuit |

relational compare 不能链式书写；`a < b < c` 报 `ChainedComparison`。equality 按左结合解析，后续仍须正常通过类型检查。

### 4.2 block 和 statement context

block 是按源码顺序执行的 statement 列表，可选择一个尾 expression。有尾 expression 时 block 值是该 expression；否则为 `Unit`。为允许 formatter 把 `}` 单独放一行，最终 expression 与 `}` 之间只有 `NL` 时仍是尾 expression；最终 expression 后出现显式 `,` 才把它固定为 expression statement。非尾 expression statement 处于 Unit context，其类型必须是 `Unit`。

Core statement 仅有：

```text
let binding = expression
var binding = expression
place = expression
return expression?
assert expression
Unit-valued expression
```

无 operand 的 `return` 等价于 `return Unit`；因此只能匹配逻辑返回类型 `Unit`。显式返回其他类型的 callable 使用它会得到普通返回类型不匹配诊断。空 block 和只有 Unit-valued statements、没有尾 expression 的 block 都产生 `Unit`。省略只适用于 callable 的返回 annotation；`Result[Unit, E]`、`Task[Unit]`、字段、参数和其他类型位置仍须显式写 `Unit`。

assignment 只是 statement，不是 expression；不能用在 initializer、argument、condition、match RHS 或 block tail，不能链式，违反报 `AssignmentInExpression`。左侧必须是 checker 认可的可写 place，权限仍由 `let`/`var`、field visibility 和 `self`/`mut self` 规则决定。

### 4.3 `if`

`if` 在 expression context 必须有 `else`，两个 branch 必须形成同一静态类型：

```loom
if condition {
    when_true
} else {
    when_false
}
```

只有当整个 `if` 直接出现在 Unit statement context，且 then block 类型为 `Unit` 时可省略 `else`。其他上下文缺 `else` 报 `MissingElse`。condition 必须是 `Bool`，不做 truthiness 转换。

### 4.4 `match`

match arm 形式为：

```text
pattern => expression
pattern => { statement-or-tail-expression }
```

RHS 只能是一个 expression 或 block。arm 由 `NL` 或 `,` 分隔，可在 `}` 前 trailing。穷尽性、arm 类型统一、unreachable arm 与 Core pattern 范围仍按 `02` 执行。

## 5. Int 运行合同

`Int` 是有符号二进制 64 位整数，范围 `-9223372036854775808`–`9223372036854775807`。Core 对 Int 只提供 unary `-`、`+`、`-`、`*`、`/`、相等与顺序比较；没有隐式 Int/Float 转换，本版也不定义显式数值转换 API。

- `+`、binary `-`、`*` 和 unary `-` 使用 checked i64；溢出产生 `RuntimeFault` / `IntegerOverflow`；
- `/` 向零截断；除数为零产生 `IntegerDivisionByZero`；
- 最小 Int 值 `-9223372036854775808` 除以 `-1` 产生 `IntegerDivisionOverflow`；
- constant folding 必须保留同一 RuntimeFault，不得把它改成 wrap、undefined behavior 或 debug-only panic；
- RuntimeFault 终止普通语言控制流，不是 `Result`、`ContractFault` 或可捕获异常。

Int 算术因为可产生 RuntimeFault，**仍不得出现在 contract predicate 中**。Int literal、相等和顺序比较仍可用。

`standard.int.parse_int(Text) Result[Int, ParseIntError]` 接受可选的单个 `+`/`-` 和至少一个 ASCII 十进制 digit；其他文本返回 `ParseIntError.InvalidSyntax`，超出上述 i64 范围返回 `ParseIntError.OutOfRange`。它是普通可失败函数，不进入 contract predicate 子集。

## 6. Float parse、format 和 canonical encoding

Float 运算与比较仍按 `02` 的 IEEE 754 binary64 语义。本节只闭合文本和 artifact 边界。

`standard.float.parse_float(Text) Result[Float, ParseFloatError]` 接受第 2.3 节的 Float 十进制 grammar，并另外接受精确的 `NaN`、`Infinity` 和 `-Infinity`；`NaN` 建立 canonical quiet NaN。其他文本返回 `ParseFloatError.InvalidSyntax`；有限十进制值 rounding 到 infinity 返回 `ParseFloatError.OutOfRange`。

`standard.float.format_float(Float) Text` 输出：

- 有限值：在 ties-to-even parse 下恢复同一 binary64 bits 的 shortest decimal；
- 若 shortest decimal 没有 `.` 或 exponent，追加 `.0`，使它仍是 Float 表面；
- exponent 使用小写 `e`，不输出多余 `+` 或 exponent leading zero；
- `-0.0` 保留符号；
- special value 精确输出 `NaN`、`Infinity`、`-Infinity`。

有限候选必须匹配第 2.3 节的 `FLOAT_LITERAL`。若多个同样短的候选恢复相同 bits，先选不使用 exponent 的候选，仍并列时按 ASCII byte order 选择最小者。

canonical Float bits 为 IEEE 754 binary64 raw bits，artifact 中使用 network byte order（big-endian）。所有 NaN sign/payload 在 canonical boundary 归一为 quiet NaN `0x7ff8000000000000`；正负零和 infinity 保留各自 bits。因此对 canonical value 满足：

```text
canonical_bits(parse_float(format_float(value))) == canonical_bits(value)
```

## 7. contract predicate 与 `old`

### 7.1 可执行 predicate 子集

contract predicate 只允许：

- Bool、Int、Float、Text 字面量；
- 参数、`self`、字段、`result` 和合法 `old(expr)`；
- `assert` 位置之前已经建立的 immutable local；
- Float 算术、unary `-`、相等/顺序比较，以及 Bool 的 `!`/`&&`/`||`；
- Int 相等/顺序比较，但不包含 Int 算术；
- 封闭值上的穷尽 `match`；
- compiler-known pure/total predicate，Core 仅有 `standard.float.is_finite`。

不允许用户 fn/method 调用、索引、assignment、I/O、可变状态、checked construction 或返回业务失败的 expression。checker 不做一般终止证明；只认该闭合 grammar 和 builtin catalog。

### 7.2 `old` 深值快照

`old(expr)` 仍只允许出现在 `ensures`。在 `requires` 全部通过后、body 执行前，runtime 按源码顺序求值并急切捕获每个 `old` 入口值。快照是逻辑深值拷贝：

- scalar 保留规范值/bits；
- Text 复制完整值；
- constrained value 保留 nominal type 与 base value；
- record、enum、Option 和 Result 递归复制所有 payload；
- 后续对 caller place 或 `self` 的修改不能改变快照；
- erased interface 参数本身不是可快照值，对它做 `old` 报 `OldOfView`；code 名保留兼容性，诊断文案不得要求用户理解 view/borrow 语法。

实现可以合并可证明相同的快照，但必须保持上述观察语义。快照资源耗尽属于 host/runtime defect，不可伪装为 ConstraintError、ContractFault 或 RuntimeFault。

## 8. builtin catalog

本版 compiler-known catalog 是闭合的：

| 名称 | 分类 | 合同 |
|---|---|---|
| `Bool` | primitive | `true`/`false`，`==`、`!=`、`!`、`&&`、`||` |
| `Int` | primitive | 第 5 节的 checked i64 运算与比较 |
| `Float` | primitive | `02` 的 binary64 运行语义与第 6 节的边界 |
| `Text` | primitive value | UTF-8 文本值，支持值 `==`/`!=` |
| `Unit` | primitive value | 唯一 `Unit` 值 |
| `Option[T]` | prelude enum | `None`、`Some(T)` |
| `Result[T,E]` | prelude enum | `Ok(T)`、`Err(E)` |
| `ConstraintError` | prelude failure value | proof unknown 的 checked construction 可处理失败；无用户构造器，不是通用 Error 父类型 |
| `ContractFault` | prelude report type | 不可构造、不可捕获，只由 contract runtime/host 报告 |
| `standard.float.ParseFloatError` | standard enum | `InvalidSyntax`、`OutOfRange` |
| `standard.int.ParseIntError` | standard enum | `InvalidSyntax`、`OutOfRange` |
| `standard.int.parse_int` | standard function | 第 5 节 |
| `standard.float.parse_float` | standard function | 第 6 节 |
| `standard.float.format_float` | standard function | 第 6 节 |
| `standard.float.is_finite` | compiler-known predicate | pure、deterministic、total，可用于 contract |
| `standard.process.arguments` | standard function | 返回不含 executable path 的 `List[Text]` |
| `standard.process.environment` | standard function | `Text -> Option[Text]`；缺失或 host 非 Unicode 值为 `None` |

`RuntimeFault` 是 host/test report category，**不是 prelude 类型或语言值**。除上表外，其他标准库名称都必须显式 import，也不会因为被标准库实现而自动获得 compiler-known 纯度或终止性。

constrained numeric value 可以按 `02` 作为 base numeric value 读取；builtin 运算结果是 base type，不自动重建 refinement。operator 不调用 Core 0.2 concept，本 catalog 不是 operator-overloading hook。

### 8.1 construction proof classification

sema 必须为每个 constrained constructor 和带 invariant 的 record literal 固定一种 checked MIR construction mode：

- `proven`：表达式类型是 nominal value，MIR 直接建立 constrained/record 表示，解释器和 LLVM 都不得再次求 predicate；
- `runtime`：表达式类型是 `Result[nominal, ConstraintError]`，两个后端都执行同一 predicate/invariant；
- `plain`：只允许用于没有 invariant 的普通 record。

静态 false construction 不得进入 MIR，而是分别报告 `ConstraintUnsatisfied` 或 `InvariantUnsatisfied`。proof domain 与 NaN 安全规则由 `02` 第 9 节固定；backend optimization 不得把 unknown 重新分类为 proven，也不得为 proven 路径恢复检查。

同一 proof engine 还给纯且 total 的合同检查分类。已由入口 nominal 事实、前序成功合同或闭合表达式证明的 `requires`、`ensures`、receiver invariant 和 `assert` 不进入 checked MIR；unknown 或 disproven 合同仍进入 MIR，以保留 `ContractFault`、blame、span 和测试失败行为。分类 requires 时不得先假设自身，分类 record invariant 时不得先假设自身；ensures 可以使用已经通过的 requires、当前 receiver invariant 与返回 nominal 类型事实。`mut self` 的 requires 事实只属于入口快照，可证明 `old(self...)`，不得证明退出时可能已改变的当前 `self...`。一个保留的合同成功后仍可建立后续 proof fact。这消除的是有独立依据的检查，不是循环假设合同成立。

## 9. diagnostic code 与 JSON

### 9.1 code 命名规则

所有对外 `code` 共享一个全局 namespace，必须：

- 使用 ASCII `PascalCase`；
- 使用稳定的原因名，不包源码位置、类型名、编译器阶段或数字序号；
- 静态诊断描述被拒绝的条件，不以 `Error` 结尾；
- contract 缺陷以 `Fault` 结尾；
- runtime 程序失败使用领域名，不以后端名或 `Panic` 命名；
- 一个 code 发布后不改名、不改主条件；需要新条件时新增 code；
- human `message` 可以改进，机器消费者只依赖 `schema_version`、`category`、`code` 和结构化字段。

Core 0.1 parser/checker 首批稳定 code：

```text
InvalidUtf8 InvalidSourceCharacter InvalidLineEnding
OrphanDocComment UnterminatedString NewlineInString InvalidEscape
InvalidUnicodeEscape InvalidIntegerLiteral IntegerLiteralOutOfRange
InvalidFloatLiteral FloatLiteralOutOfRange UnexpectedToken
UnexpectedEndOfFile MissingSeparator SemicolonNotSupported
SyntaxNestingLimit
ChainedComparison AssignmentInExpression MissingElse InvalidMatchArm
NestedDeclarationNotAllowed MissingModuleDeclaration
DuplicateModuleDeclaration ModuleCycle DuplicateDeclaration UnknownName
NameNotVisible WildcardImportNotSupported TopLevelStatementNotAllowed
TypeMismatch CannotInferType AmbiguousTypeInference InvalidGenericOperation
MissingField UnknownField DuplicateField NonExhaustiveMatch
UnreachableMatchArm InvalidTestSignature ImmutableBindingAssignment
InvalidAssignmentTarget ForeignInherentImpl ReadonlyReceiverMutation
MutReceiverRequiresVar InoutAliasConflict InvariantIsolationViolation
InvalidContractExpression InvalidOldExpression OldOfView
ContractFaultNotCatchable ConstraintUnsatisfied InvariantUnsatisfied
```

Core 0.2 稳定 code 沿用 `05` 第 13 节，并纳入同一 namespace：

```text
MissingConformance ForeignConformance DuplicateConformance
OverlappingConformance IncompleteConformance
ConformanceSignatureMismatch AssociatedBoundNotSatisfied
AssociatedProjectionCycle AmbiguousConceptMethod DynNotDeclared
DynStaticRequirement DynGenericMethod DynSelfLeak
DynAssociatedTypeUnbound DynAssociatedTypeMismatch
DynMutReceiverUnavailable IllegalDynConversion ViewEscape
DynViewInGeneric BorrowConflict ReadonlyBorrowConflict UseAfterViewMove
DynCarrierRequired AssociatedProjectionAmbiguous
UnconstrainedImplParameter ConformanceResolutionCycle
IllegalDynAbiBoundary DynOwnedCarrierUnavailable
```

failure/value code：

```text
ConstraintViolation InvariantViolation
PreconditionFault PostconditionFault InvariantFault AssertionFault
IntegerOverflow IntegerDivisionByZero IntegerDivisionOverflow
ArtifactVersionMismatch CompilerDefect InterpreterDefect
```

未被上述 code 精确覆盖的 parser 形状使用 `UnexpectedToken` 或 `UnexpectedEndOfFile`，通用类型不匹配使用 `TypeMismatch`；实现不得临时生成位置相关或数字 code。

### 9.2 span

稳定 span JSON 为：

```json
{
  "path": "src/order.loom",
  "start_byte": 0,
  "end_byte": 4,
  "start_line": 1,
  "start_column": 1,
  "end_line": 1,
  "end_column": 5
}
```

byte offset 是相对原 UTF-8 bytes 的 zero-based、half-open offset；line/column 是 one-based，column 按 Unicode scalar 计数。`path` 必须是使用 `/` 的 project-relative path，不输出绝对路径。LSP adapter 在边界转换为 LSP 要求的 UTF-16 position，不改变 compiler JSON。

### 9.3 静态诊断 envelope

第 9.3–9.6 节列出的字段就是 schema version 1 的稳定字段集，必须全部出现。无值的 nullable span 用 `null`，集合无值用空 array，`details` 无值用空 object：

```json
{
  "schema_version": 1,
  "category": "diagnostic",
  "severity": "error",
  "code": "MissingElse",
  "message": "if expression requires else",
  "primary_span": {"path":"src/order.loom","start_byte":0,"end_byte":2,"start_line":1,"start_column":1,"end_line":1,"end_column":3},
  "related": [
    {"label":"if starts here","span":{"path":"src/order.loom","start_byte":0,"end_byte":2,"start_line":1,"start_column":1,"end_line":1,"end_column":3}}
  ],
  "notes": [],
  "details": {}
}
```

`severity` 的 Core 值为 `error`、`warning`、`info`；只有 `error` 阻止 build。一个 invocation 的 diagnostics 按 `primary_span.path`、`start_byte`、`end_byte`、`code`、`message` 排序；`related` 按自己的 path/span/label 排序。两者都不得依赖文件遍历顺序。

### 9.4 ConstraintError

```json
{
  "schema_version": 1,
  "category": "constraint_error",
  "code": "ConstraintViolation",
  "target_type": "shop.price.Price",
  "predicate": "self >= 0.0",
  "path": [],
  "value_summary": "Float(-0.01)",
  "contract_span": {"path":"src/price.loom","start_byte":24,"end_byte":42,"start_line":3,"start_column":20,"end_line":3,"end_column":38}
}
```

record construction invariant 使用 `InvariantViolation`。`path` 是 field/variant path segments 的 JSON string array。

### 9.5 ContractFault

```json
{
  "schema_version": 1,
  "category": "contract_fault",
  "code": "PreconditionFault",
  "message": "precondition failed",
  "blame": "caller",
  "contract_span": {"path":"src/order.loom","start_byte":88,"end_byte":121,"start_line":7,"start_column":9,"end_line":7,"end_column":42},
  "call_span": {"path":"src/cart.loom","start_byte":40,"end_byte":67,"start_line":4,"start_column":5,"end_line":4,"end_column":32},
  "value_summary": "Price(120.0)"
}
```

`blame` 只能是 `caller`、`callee` 或 `implementation`。`PreconditionFault` 为 caller，`PostconditionFault` 为 callee，`InvariantFault`/`AssertionFault` 为 implementation。不适用的 call span 使用 `null`。

### 9.6 RuntimeFault

```json
{
  "schema_version": 1,
  "category": "runtime_fault",
  "code": "IntegerDivisionByZero",
  "message": "integer division by zero",
  "operation": "divide",
  "span": {"path":"src/math.loom","start_byte":31,"end_byte":36,"start_line":3,"start_column":5,"end_line":3,"end_column":10},
  "operand_summary": ["Int(1)", "Int(0)"]
}
```

`operation` 只能是 `add`、`subtract`、`multiply`、`divide` 或 `negate`。RuntimeFault 是 test/host 失败，不是语言值。

### 9.7 安全 value summary

summary 必须 deterministic，并且：

- Bool、Int、Float 和 Unit 使用 canonical scalar spelling；
- Text 只输出 UTF-8 byte length，形如 `Text(bytes=12)`，不输出内容；
- constrained scalar 输出 nominal type 与 scalar summary；
- record 只输出限定类型名，enum 只输出限定类型名和 variant；
- Option/Result 只输出 constructor 与 payload 的递归安全 summary；
- 不输出绝对路径、指针、hidden dyn type、Text 内容或 record field value。

## 10. native artifact、ABI 与 host boundary

默认 `loomc build` 必须产生宿主平台 native executable：checked MIR 先经过 root/witness reachability，再 lower 为 LLVM IR、验证、优化并输出 object，最后由平台 linker 链接。development 使用 O0 + global DCE，`--release` 使用 O2 + global DCE；两者均在优化前后验证。`--target-triple T --emit object` 产生 T 的 relocatable object；没有匹配 Loom runtime/linker 时禁止跨目标 executable link。`loomc test` 使用同一路径生成 native test harness；`loomc run PATH` 编译临时 native executable 后运行。

前端、checked MIR、root graph 和缓存 identity 对相同输入必须 deterministic。最终 executable 不承诺逐字节 reproducible，因为系统 linker 可能加入平台 metadata；它仍不得让时间戳、绝对路径、文件遍历顺序或编辑器状态改变语言行为。

解释器只作为 `--backend interpreter` 显式选择的语义 oracle。其 `.loomi` image 是 compiler-private versioned artifact，并固定记录 Loom language version；不兼容 artifact format/version 必须报 `ArtifactVersionMismatch`，不兼容语言版本必须报 `ArtifactLanguageVersionMismatch`，不得猜测运行。默认 `build/test/run` 不能隐式回退到解释器。

record/enum layout、typed IR、calling convention、contract thunk、concept witness table 和成员顺序均为 compiler-private ABI。源码不能观察它们；Core 不承诺跨编译器版本、dynamic library、plugin 或 FFI ABI。

工具边界的终止分类：

| 结果 | CLI exit | 报告 |
|---|---:|---|
| 成功 | 0 | 正常结果 |
| source diagnostic、test Err、ConstraintError test failure、ContractFault、RuntimeFault | 1 | 对应结构化报告 |
| CLI invocation/config/artifact version 错误 | 2 | diagnostic / `ArtifactVersionMismatch` / `ArtifactLanguageVersionMismatch` |
| compiler、backend 或 interpreter defect | 3 | 对应 defect code |

defect 只能在 CLI/host 边界报告并终止当前执行；不得暴露为语言 exception、`Err`、ContractFault 或 RuntimeFault。debug/release 不得改变上述分类。

完整 stage、root policy、LLVM binding、缓存和替代后端见 [编译过程与后端定案](07-compiler-pipeline-and-backends.md)。

## 11. Core 0.2 parser/checker 降级合同

本节不改变 `05` 语义，只冻结 parser 同步点与接口参数 checker 边界。reference implementation 已按 Core 0.1 C1a–C1d、Core 0.2 C1e–C1f 的顺序接入。

### 11.1 declaration 开始序列

Core 0.2 增加：

```text
(pub)? concept
(pub)? dyn concept
impl GenericParams? ConceptRef for Type
```

concept member 开始序列：

```text
associated type
method
static method
```

conformance member 开始序列：

```text
associated type
method
static method
```

`impl Type` 仍是 inherent impl；`impl ConceptRef for Type` 是 conformance。parser 在 `impl` error island 中必须同时识别两类 member set，但不能把损坏的 conformance 重分类为 inherent impl。

### 11.2 必需表面形状

```text
GenericParams       := "[" GenericParam ("," GenericParam)* "]"
GenericParam        := Identifier (":" GenericBounds)?
ConceptRef          := QualifiedName AssociatedBindings?
AssociatedBindings := "[" AssociatedBinding ("," AssociatedBinding)* "]"
AssociatedBinding  := Identifier "=" Type
GenericBounds       := ConceptRef ("+" ConceptRef)*
QualifiedProjection := "<" Type "as" ConceptRef ">" "." Identifier
DynType             := "dyn" ConceptRef
```

associated requirement 写 `associated type Name` 或 `associated type Name: GenericBounds`；conformance binding 写 `associated type Name = Type`。method-specific generic arguments继续用 `[...]`。其余 signature、contract、projection、owner-orphan、overlap/termination 和 dot-call candidate 结果由 `05` 规定；内部求解算法不是源码语义，但必须产生同一接受/拒绝结果和同一稳定 code。

函数参数仍统一使用 `name Type`，不使用全局冒号：

```loom
fn show(value Display) Text
fn explicitly_erased(value dyn Display) Text
```

泛型 bound 保留 `T: C`，因为这里的冒号表达“类型参数满足约束”，不是参数名和类型之间的分隔符。

### 11.3 接口参数算法

- 当参数类型解析为 `dyn concept C` 时，`value C` 与 `value dyn C` 进入相同的 erased-interface 类型检查；前者是惯用形式，后者只强调擦除；
- concrete 实参只在存在唯一显式 conformance 且 associated bindings 完全相同时自动适配；
- 显式 `dyn C` 是普通一等类型，可返回、存入 record/enum、放入 tuple/list、嵌套为泛型实参；这些位置不把裸 `C` 隐式解释成接口类型；
- stored `dyn C` 的普通 copy 必须复制 underlying logical value 并保留同一 proof，不能建立源码可观察的共享可变别名；
- `mut self` requirement 的 receiver 必须是 `var` place；同步 concrete-to-interface 调用可以用 call-scoped inout 并在正常返回后写回，异步调用则复制拥有值进入 Task；
- call-scoped 写回/reborrow 载体是 checked-MIR 内部值，不得存储、返回、嵌套或进入异步调用；
- 编译器可以对具体类型和 witness 静态可知的调用直接派发；否则使用携带已选 proof 的 compiler-private 表示。当前 LLVM C1 可以传递 data/witness pair，但该形状不是源码、artifact 或未来后端合同；
- 旧 `view[dyn C]`、`view[mut dyn C]` 和显式 view construction 报 `UnsupportedSyntax`；`box[...]`、`shared[...]` 同样不得产生可进入 typed program 的类型；
- 当前没有 universal `any`，也不得从 `any` 运行时搜索 conformance 并转换为 `dyn C`。

这些规则不建立源码级 borrow、lifetime、move token 或 owner freeze。一等接口的物理表示、同步参数的临时写回和地址传递完全属于 compiler-private ABI。

## 12. Core 0.3 parser/checker 增量合同

Core 0.3 的完整语义由 [GC、词法清理与异步任务定案](08-memory-cleanup-and-async.md)规定。本节固定已经进入 parser、checker、MIR、interpreter 与 LLVM native backend 的可执行形状。

基础形状：

```text
AsyncFunction  := "async" "fn" FunctionTail
AsyncTest      := "test" "async" "fn" FunctionTail
ScopedBinding  := "scoped" Identifier Type? "=" Expression
DeferItem      := "defer" Block
AwaitSuffix    := "." "await"
PropagateSuffix := "?"
TupleType      := "(" Type "," (Type ("," Type)*)? ")"
TupleExpr      := "(" Expression "," (Expression ("," Expression)*)? ")"
TupleBinding   := "let" Identifier ("," Identifier)+ "=" Expression
ForRangeItem   := "for" Identifier "in" Expression ".." Expression Block
EmptyList      := "List" "[" Type "]" "(" ")"
TaskJoin       := "Task" "." ("all" | "settled" | "any" | "race")
                  "(" Expression ("," Expression)* ")"
TaskSleep      := "Task" "." "sleep" "(" Expression ")"
TaskWait       := "Task" "." ("waitReadable" | "waitWritable") "(" Expression ")"
```

`for` 的区间是 `Int` 上的半开区间 `[start, end)`；上下界各求值一次，iteration binding 不可修改。每轮 body 是独立词法 scope，因此该轮注册的 `defer`/`scoped` 在进入下一轮前已经完成。`List[T]()` 建立空的可增长同构 list；`var list` 可调用 `list.add(value)`，所有 list 可调用 `length() Int` 与 `get(index Int) Option[T]`。越界和负 index 返回 `None`。

`pub async fn` 合法；method/concept requirement 的 async 形式当前拒绝。`scoped` 不与 `let`/`var` 连写。显式类型仍写 `scoped name Type = value`，不增加 `name: Type`。

parser recovery 的 declaration start set 必须识别 `async fn`、`pub async fn`、`test async fn`；block recovery 必须识别 `scoped` 与 `defer`。`.await`、call/member 与独立的 `?` 共用 postfix chain 和 nesting budget，并保持输入 lossless；旧前缀 `await task` 只是非法语法，按普通 `UnexpectedToken` 诊断，不设置迁移专用错误码，也不得进入 checked program。`a.await?` 合法，`a.await!` 不定义为 unwrap 或 fault 运算符。

当前 checker 只发布已验证的 async 形状；不支持的 async method/concept requirement、contract/interface-cross-await 形状必须给出 source diagnostic。线性表达式中的后缀 await 会在 MIR lowering 时按求值顺序提取成隐藏 suspension binding，因此 `task.await.decode()`、`task.await + 1` 与 `task.await?` 可恢复执行；if/match/block 内的 await 保留自己的 numbered state，并由同一 resume dispatch 恢复。`?` 仅接受 `Result[T, E]`，要求当前 callable 返回错误类型完全相同的 `Result[_, E]`，并降低为显式 `Ok/Err` 分支；`Err` 复用普通 return 路径，因此执行全部词法 cleanup。`Task.sleep(milliseconds)`、`Task.waitReadable(fd)` 与 `Task.waitWritable(fd)` 都构造可存储 Task；descriptor 仅在 registration 生命周期内被借用，runtime 不负责关闭。固定异构 Task 参数产生 tuple，单个同构 task list 产生 list；`all/settled/any/race` 共享真实 JoinState、取消与 drain。LLVM safepoint 上的精确 moving collector 会重写跨 await frame roots。

## 13. 实现关门

本文的实现关门结果：

1. Core 0.1 lexer/parser、checker、contract runtime、LLVM native artifact 和 JSON diagnostics 已接入 C1a–C1d reference implementation；
2. Core 0.2 grammar、static concept 与 erased-interface checker 已按 C1e–C1f 接入同一 pipeline；
3. Core 0.3 grammar、GC、块级 cleanup、Task/coroutine、wait registration、join 与取消已接入 interpreter 和 LLVM native pipeline；
4. root analysis 覆盖 entry/test、direct/generic/static/dynamic witness、async constructor/resume 与 runtime edge，并只保留 live requirement slots；
5. formatter、CLI、native test runner 和 LSP 使用同一 token、span、code 和 failure schema；
6. 冷路径与任何未来增量路径必须得到相同 typed program、diagnostic ordering、reachability 和运行结果；
7. parser/checker/backend 实现细节可以替换，但不能自行发明第二套换行、overflow、fault、snapshot、builtin、interface 或 code 规则。

所有权/借用、AOP-like 组合、live/AST 编辑、desired-state/operator、一般 capability/effect、composition bundle 与专用 example/scenario 均不是 Core 0.1–0.3 blocker。基础 package manifest、path/文件/HTTPS registry dependency、认证发布、lockfile、optional-dependency feature、target、可信离线 registry cache 和持久编译 cache 已经闭环。
