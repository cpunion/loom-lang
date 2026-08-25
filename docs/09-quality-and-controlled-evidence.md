# 优化、性能、模糊测试与受控任务证据

状态：Active / C3 Multi-Package Repository Gate

日期：2026-08-25

本文固定实现质量证据，不增加 Loom 源码语法或运行时语义。三项冻结任务继续提供 C2 oracle；提交到仓库的 `examples/c3` 另提供一个非生成的 3-package/24-module workload，在解释器与 LLVM 后端上执行同一 main/test，并记录 package、module、checked MIR、root reachability 和时间数据。这形成最小 C3 repository gate，但不声称已经完成人类开发效率对照、长期生产运维或对所有领域普适。

## 1. 冻结任务

| 任务 | 源码 | SHA-256 | 主要机制 |
|---|---|---|---|
| constrained-contracts | `examples/core01/shop.loom` | `f3c6b8cad23cf4113e7555ac29d2307d853af10eff4ee89482ef4c8617a77472` | refined value、invariant、method contract、mutation |
| concept-polymorphism | `examples/core02/concepts.loom` | `60bc7e21bd475ae3fb0f795f25cbae92e4d86c7c48675abad02c9561d2701d4a` | static concept、associated type、first-class `dyn C` |
| structured-async | `examples/core03/tasks.loom` | `0981e9597a0a450c4a4bc035568be1e57fe50bb746dd827c7471aab45c0dae2d` | scoped/defer、Task、await、tuple/list join、取消 |

哈希变化会让门禁失败。任务语义确需修改时，必须同时审阅 oracle 并显式更新哈希，不能把漂移当作新证据。

每项任务必须满足：全项目静态检查无错误；解释器全部 `test fn` 通过且 `main` 返回 `Unit`；release LLVM native test harness 全部通过且 native `main` 输出 `Unit`。报告同时记录源码 bytes/lines/tokens、完整 MIR function/test 数、run/test root 的实际可达函数数，以及 analysis、解释执行、native build 和 native execution 时间。

另有一个不含 `main` 的异步回归门 [`fixtures/async-generic-contracts`](../fixtures/async-generic-contracts/main.loom)：它要求 conditional conformance proof 在首次 suspension 后仍由 Task frame 持有，并同时覆盖 async `requires`/`ensures` fault、`Task.settled` 与 `Task.any` sibling cancellation。`loom-quality` 会在解释器与 release LLVM native test harness 中各执行一次，避免只靠单 crate 测试或 development profile 证明该路径。

## 2. C3 repository workload

[`examples/c3`](../examples/c3/README.md) 固定一个 checkout service 形状的多包工程：`foundation` 提供 constrained values、record、enum 和 dynamic concept；`catalog` 作为 direct path dependency 提供 conformance 与业务组合；`application` 显式声明它实际 import 的两个 direct dependencies，并提供 bin/test target。门禁要求：

- 3 个 package、24 个源码 module 都进入 package-aware source/semantic identity；
- interpreter 与 LLVM 的 `application.main.start` 和两个普通 test 结果一致；
- 报告保存所有源码的 canonical SHA-256、完整 MIR 数及 main/test 实际 root 数；
- package isolation、transitive import、同名 module 注入和依赖写保护由独立负例测试验证，不能靠 workload 恰好不触发来代替。

## 3. 性能与增量门

`loom-quality` 使用绝对上界来阻止数量级退化、意外全图重查和挂死，不把一次 CI wall clock 当成语言运行时排名：

| 门 | 上界或结构要求 |
|---|---|
| 每任务 analysis | 10 s |
| 每任务 interpreter main + tests | 15 s |
| 每任务 release native main + tests build | 60 s |
| 每任务 native main + tests execution | 15 s |
| 约 1.8 MB lossless parser/recovery | 8 s |
| Core 0.3 checked-MIR artifact decode + validate 32 次 | 15 s |
| 64-module 单 body 修改 | 最多重查 1 module，至少复用 63 module，10 s |
| C3 repository analysis/native build | 15 s / 90 s |
| async generic contracts interpreter/native build/native run | 15 s / 60 s / 15 s |
| 整套受控任务 | 300 s |

门限故意高于正常机器的毫秒级结果，以吸收共享 CI 抖动；任何收紧都应先保留多次 runner 证据。结构门比 wall-clock 更强：单 body 修改若退化为全图检查，即使机器足够快也立即失败。

运行并保存机器可读报告：

```sh
cargo run --release -p loom-quality | tee target/c3-evidence.json
```

LLVM 19 CI 在每次 push/PR 运行该命令并上传 JSON artifact。报告包含规范化 target triple 与实际优化 pipeline，不能拿不同 target/profile 的数字直接比较。

## 4. LLVM 优化证据

development 使用 `default<O0>,globaldce`，release 使用 `default<O2>,globaldce`，且两者在优化前后都通过 LLVM verifier。机器 IR 回归进一步固定：

- 不可达私有函数在两个 profile 都不进入 object；
- development 保留可达的 checked constant arithmetic helper；
- release 常量折叠并内联该 helper，移除对应 overflow intrinsic 与 machine function；
- release/native 仍通过同一合同、checked integer 和双后端结果 oracle。

因此这里验证的是实际 IR 变化，不只验证命令行 profile 字符串不同。

## 5. 持续模糊测试

独立的 `fuzz/` workspace 避免普通 stable build 依赖 libFuzzer。当前 target：

- `syntax`：任意 UTF-8 的 lexer losslessness、token/span 单调性、parser recovery、diagnostic span 边界；
- `artifact`：任意 bytes 以及从合法 seed 产生的结构化 mutation，贯穿 JSON nesting/header、Float side table、entry 与完整 checked-MIR validator。
- `semantics`：生成有界 constrained-integer 程序，交叉检查 direct proof elimination、静态失败、运行时 validation 和解释执行，防止“消除检查”变成不可靠证明。

CI 用 nightly coverage instrumentation 各运行 20 秒。崩溃必须先最小化，再升级为普通 deterministic regression；只保存 fuzzer artifact 不算修复。具体本地命令见 [`fuzz/README.md`](../fuzz/README.md)。

## 6. 证据边界

当前可以称为 C3 implementation-controlled repository evidence：固定任务与多包 workload 的语义一致性、package boundary、root/DCE、编译执行预算、增量选择性以及恶意输入边界都有自动门禁。仍不能据此声称 Loom 比某门成熟语言更易开发，也不能把一个 24-module workload 外推为大型生产仓库的长期收益；这些结论仍需要预注册的人类 A/B 任务与独立外部项目证据。
