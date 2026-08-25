# Loom Core 0.3 标准值、JSON、I/O 与日志

状态：Normative P1 Surface

日期：2026-08-25

本文固定 Core 0.3 可直接用于普通程序的最小标准库。它只增加 compiler-known 类型、普通函数和 method，不增加 operator overloading、ownership/borrow、effect、AOP、live 或运行时类型注册。

## 1. 值与失败边界

- `Text` 是有效 Unicode scalar sequence；源码不能构造无效 UTF-8 Text。
- `Bytes` 是不可变的任意 byte sequence，与 Text 不隐式转换。
- `Path` 是不可变、使用 `/` 分隔的 portable lexical path；它不做文件系统访问或 `.`/`..` 归一化。
- `TextMap[V]` 是不可变、Text-keyed 的有序 map。
- 值操作返回新值，不暴露地址、capacity、hash seed 或内部共享。
- 可预期的数据拒绝使用 `Option`/`Result`；compiler/runtime defect、非法 checked MIR、日志设备失败和不可恢复 OOM 不伪装成业务 `Err`。

这些类型都可由 GC 移动。`Text`、`Bytes`、`Path` 和 `TextMap[V]` 的 copy 保持值语义；实现可以共享 immutable storage，但源码不能观察共享。

## 2. Text、Bytes 与 Path

```loom
text.length() Int
text.get(index) Option[Text]
text.concat(other) Text
text.contains(needle) Bool
text.encode_utf8() Bytes

bytes.length() Int
bytes.get(index) Option[Int]
bytes.append(other) Bytes
bytes.decode_utf8() Result[Text, DecodeTextError]

Path.from_text(text) Result[Path, PathError]
path.as_text() Text
path.join(child) Result[Path, PathError]
```

`Text.length` 和 `Text.get` 以 Unicode scalar 计数，不以 UTF-8 byte 或 grapheme cluster 计数；负数或越界 index 返回 `None`。`contains` 是逐 scalar sequence 的精确、大小写敏感查找，不做 Unicode normalization 或 locale folding。

`Bytes.get` 返回 `0..255` 的 `Int`，负数或越界返回 `None`。`decode_utf8` 对任意无效 sequence 返回 `Err(DecodeTextError.InvalidUtf8)`。

`Path.from_text` 只在 Text 含 NUL 时返回 `Err(PathError.ContainsNul)`。absolute path 定义为以 `/` 开头；反斜杠和 drive spelling 不获得平台相关的特殊语义。`base.join(child)` 在 child 为 absolute 时返回 `Err(PathError.AbsoluteJoin)`，否则只按需要插入一个 `/`；空 component 合法，不折叠 `.`、`..` 或重复 separator。

## 3. TextMap

```loom
let empty = TextMap[Value]()
let next = empty.insert("key", value)

map.length() Int
map.contains(key) Bool
map.get(key) Option[Value]
map.insert(key, value) TextMap[Value]
map.remove(key) TextMap[Value]
```

`insert` 返回新 map；同 key 覆盖旧 value。`remove` 对不存在的 key 幂等。原 map 的可观察值不改变，因此这些 method 不要求 `var` receiver。

key 的 canonical order 是有效 UTF-8 编码的 byte lexicographic order；该顺序也等价保持 Unicode scalar order。第一版不暴露 iteration API，但 JSON formatting、结构化日志、artifact 和测试 oracle 都必须使用这个顺序，不能依赖随机 hash seed。只有当 `V` 具有静态可导出的 value-equality capability 时，`TextMap[V]` 才可使用 `==`/`!=`。

## 4. JSON

`Json` 是闭合递归值：

```loom
Json.Null
Json.Bool(Bool)
Json.Number(Float)
Json.Text(Text)
Json.Array(List[Json])
Json.Object(TextMap[Json])
```

普通入口必须显式 import：

```loom
import standard.json.parse_json
import standard.json.format_json

parse_json(text) Result[Json, JsonError]
format_json(value) Result[Text, JsonError]
```

`JsonError` 是可穷尽匹配的闭合 enum：

```loom
InvalidSyntax(offset Int)
NumberOutOfRange(offset Int)
DepthLimit
NonFiniteNumber
```

offset 是从输入开头计算的零基 UTF-8 byte offset。parser 必须消费完整 document，拒绝 trailing non-whitespace、非法 escape/surrogate、重复 object key 和超出 Float 表示范围的 number；重复 key 的 offset 指向第二个 key。container nesting 上限固定为 128，超过时返回 `DepthLimit`，不得栈溢出。

format 使用无多余 whitespace 的 canonical JSON：object key 采用 TextMap canonical order，string 使用 JSON escaping，有限 Float 使用最短 round-trip 表示并保留负零。`Json.Number` 本身允许普通 Float value；format 遇到 NaN 或 infinity 返回 `NonFiniteNumber`。format 同样执行 128 层深度限制。

## 5. 可恢复 I/O

Core 0.3 原有的 `open_read/create/connect` 和 resource `read_text/write_text` 保留；它们把 I/O 失败变成 task-local fault，便于既有程序继续工作。新程序可以选择显式、typed 的 `try_` 表面：

```loom
import standard.file.try_open_read
import standard.file.try_create
import standard.file.try_open_read_path
import standard.file.try_create_path
import standard.net.try_connect

try_open_read(Text) Task[Result[File, IoError]]
try_create(Text) Task[Result[File, IoError]]
try_open_read_path(Path) Task[Result[File, IoError]]
try_create_path(Path) Task[Result[File, IoError]]
try_connect(Text, Int) Task[Result[Socket, IoError]]

file.try_read_text() Task[Result[Text, IoError]]
file.try_write_text(Text) Task[Result[Unit, IoError]]
socket.try_read_text() Task[Result[Text, IoError]]
socket.try_write_text(Text) Task[Result[Unit, IoError]]
```

成功取得的 `File`/`Socket` 仍是 `MustScope`。惯用写法可以把 `?` 与后缀 `.await` 放入 `scoped` initializer；错误提前返回时尚未建立 resource binding，因此不会登记虚假的 cleanup：

```loom
scoped file = try_open_read_path(path).await?
```

`IoError` 是 opaque value，提供 `.kind() IoErrorKind` 与 `.message() Text`。`IoErrorKind` 是稳定、可穷尽匹配的闭合 enum：

```loom
NotFound
PermissionDenied
AlreadyExists
InvalidInput
ConnectionRefused
ConnectionReset
TimedOut
UnexpectedEof
Closed
Other
```

kind 由 host I/O error category 映射，不暴露不稳定的 OS integer code；message 用于人类诊断，不作为稳定比较键。Text read 得到无效 UTF-8 时映射为 `InvalidInput`。参数越出稳定语言范围、正常 OS 拒绝和资源状态错误进入 `Err(IoError)`；OOM、runtime ABI/version 错误和 checked-MIR defect 仍走不可恢复 fault 边界。

所有这些 I/O Task 使用与 `Task.sleep` 相同的 executor：blocking file/DNS 工作进入有界 worker pool，socket `WouldBlock` 进入 kqueue/epoll registration，完成只 enqueue ready task，不在 callback 栈重入 coroutine。

## 6. 日志

```loom
import standard.log.debug
import standard.log.info
import standard.log.warn
import standard.log.error
import standard.log.write

debug(message) Unit
info(message) Unit
warn(message) Unit
error(message) Unit
write(level LogLevel, message Text, fields TextMap[Text]) Unit
```

`LogLevel` 是 `Debug|Info|Warn|Error`。四个 helper 等价于用相应 level 和空 fields 调用 `write`。

每次调用向 stderr 写一个 UTF-8 JSON line。顶层 key 顺序固定为 `level`、`message`、`fields`；level 使用小写，string 使用 JSON escaping，fields 使用 TextMap canonical key order，不写额外 whitespace。单线程 executor 内一次调用对应一条完整 record。stderr 写失败触发不可恢复 `RuntimeFault`，code 为 `LogWriteFault`；日志 API 不把设备失败伪装成业务 `Result`。

## 7. 后端一致性门

解释器和 LLVM/native 必须对同一 fixture 产生相同的：

- Unicode scalar length/index、byte index 和 invalid UTF-8 结果；
- Path construction/join 与真实临时文件读写；
- TextMap overwrite/remove/equality 与 canonical ordering；
- JSON parse error variant/offset、duplicate-key/depth/non-finite 行为和 canonical text；
- recoverable I/O kind/message shape、resource scoped cleanup；
- canonical stderr log bytes。

这些 builtin 仍必须进入 checked-MIR validator 和 closed-world reachability；仅声明标准类型或 import 未调用函数不形成 native code root。
