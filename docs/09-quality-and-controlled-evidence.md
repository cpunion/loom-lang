# 优化、性能、模糊测试与受控任务证据

状态：Active / C2 Implementation-Controlled Gates

日期：2026-08-25

本文固定实现质量证据，不增加 Loom 源码语法或运行时语义。当前 C2 主张严格限定为：三项冻结任务在解释器与 LLVM 后端上通过同一正确性 oracle，并在固定的编译、执行、增量和输入规模预算内完成。它不声称已经完成人类参与的开发效率对照，也不证明大型真实仓库收益；后者仍分别需要独立研究和 C3 证据。

## 1. 冻结任务

| 任务 | 源码 | SHA-256 | 主要机制 |
|---|---|---|---|
| constrained-contracts | `examples/core01/shop.loom` | `f3c6b8cad23cf4113e7555ac29d2307d853af10eff4ee89482ef4c8617a77472` | refined value、invariant、method contract、mutation |
| concept-polymorphism | `examples/core02/concepts.loom` | `60bc7e21bd475ae3fb0f795f25cbae92e4d86c7c48675abad02c9561d2701d4a` | static concept、associated type、first-class `dyn C` |
| structured-async | `examples/core03/tasks.loom` | `0981e9597a0a450c4a4bc035568be1e57fe50bb746dd827c7471aab45c0dae2d` | scoped/defer、Task、await、tuple/list join、取消 |

哈希变化会让门禁失败。任务语义确需修改时，必须同时审阅 oracle 并显式更新哈希，不能把漂移当作新证据。

每项任务必须满足：全项目静态检查无错误；解释器全部 `test fn` 通过且 `main` 返回 `Unit`；release LLVM native test harness 全部通过且 native `main` 输出 `Unit`。报告同时记录源码 bytes/lines/tokens、完整 MIR function/test 数、run/test root 的实际可达函数数，以及 analysis、解释执行、native build 和 native execution 时间。

## 2. 性能与增量门

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
| 整套受控任务 | 180 s |

门限故意高于正常机器的毫秒级结果，以吸收共享 CI 抖动；任何收紧都应先保留多次 runner 证据。结构门比 wall-clock 更强：单 body 修改若退化为全图检查，即使机器足够快也立即失败。

运行并保存机器可读报告：

```sh
cargo run --release -p loom-quality | tee target/c2-evidence.json
```

Linux LLVM 19 CI 在每次 push/PR 运行该命令并上传 JSON artifact。报告包含规范化 target triple 与实际优化 pipeline，不能拿不同 target/profile 的数字直接比较。

## 3. LLVM 优化证据

development 使用 `default<O0>,globaldce`，release 使用 `default<O2>,globaldce`，且两者在优化前后都通过 LLVM verifier。机器 IR 回归进一步固定：

- 不可达私有函数在两个 profile 都不进入 object；
- development 保留可达的 checked constant arithmetic helper；
- release 常量折叠并内联该 helper，移除对应 overflow intrinsic 与 machine function；
- release/native 仍通过同一合同、checked integer 和双后端结果 oracle。

因此这里验证的是实际 IR 变化，不只验证命令行 profile 字符串不同。

## 4. 持续模糊测试

独立的 `fuzz/` workspace 避免普通 stable build 依赖 libFuzzer。当前 target：

- `syntax`：任意 UTF-8 的 lexer losslessness、token/span 单调性、parser recovery、diagnostic span 边界；
- `artifact`：任意 bytes 以及从合法 seed 产生的结构化 mutation，贯穿 JSON nesting/header、Float side table、entry 与完整 checked-MIR validator。

CI 用 nightly coverage instrumentation 各运行 20 秒。崩溃必须先最小化，再升级为普通 deterministic regression；只保存 fuzzer artifact 不算修复。具体本地命令见 [`fuzz/README.md`](../fuzz/README.md)。

## 5. 证据边界

当前可以称为 C2 implementation-controlled evidence：固定任务的语义一致性、root/DCE、编译执行预算、增量选择性以及恶意输入边界都有自动门禁。仍不能据此声称 Loom 比某门成熟语言更易开发，也不能声称真实大型仓库已验证；这些结论需要预注册的人类 A/B 任务或 C3 repository，而不是继续扩大本报告的措辞。
