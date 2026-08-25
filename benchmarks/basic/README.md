# Loom 基础跨语言基准

这组受控微基准比较同一台机器上的 Loom release LLVM、Go、Rust、C 和 C++。它用于定位当前实现的数量级和优化方向，不是通用语言排名。

## 用例

五份源码都接受动态命令行 `CASE SIZE EXPECTED`；Rust runner 独立计算 `EXPECTED`，程序执行被测算法并校验结果，成功时只输出 `Unit`。答案是运行时参数而不是源码常量，优化器不能据此折叠热计算：

| case | 默认规模 | 主要路径 | 可观察 checksum |
|---|---:|---|---|
| `int_lcg` | 2,000,000 | 有界 `Int` 乘除和循环 | 最终 LCG state |
| `record_method` | 500,000 | 跨函数 POD result/`mut self` InOut、SROA 与周期整数累计 | `total` 和 `calls` |
| `list_build_scan` | 10,000 | 从空 List 几何增长，再按已证明的 exact length 扫描 | 元素和与长度 |
| `fib_recursive` | 32 | 非尾递归函数调用 | Fibonacci 值 |

所有数值都落在 signed 64-bit 范围内。Loom 和 Rust 保留 checked integer 行为；C、C++ 和 Go 的固定规模也不会溢出。List 用例不预留容量，五种实现都从空容器开始；它同时测量容器增长、元素写入和 checked-get 边界安全语义及其可证明消除，不把结果解释为纯内存带宽。

Text 拼接和 `dyn` 分派没有放入 v1：各语言的字符串表示、分配策略、虚调用去虚化条件并不天然等价。它们应在 ABI/layout 优化阶段用单独、明确语义的 case 加入。

默认规模按当前 Loom 实现校准，使标准矩阵能在开发机上有界完成；它们不是成熟实现的吞吐上限。`record_method` 当前命中跨函数 POD 首阶段：producer 以 first-class LLVM `{i64, i64}` aggregate 返回，`counter.add` 通过 call-scoped `mut self` InOut pointer 调用，private 热路不 clone/build managed chain；release 可以内联/SROA 并删除未引用的 universal fallback。whole-value copy 或普通参数、readonly/contract/generic 等 universal boundary 仍物化独立 managed value，因此继续保持值复制和 moving-GC 语义。这不代表一般 aggregate ABI 已完成。

`list_build_scan` 曾用于定位整表 receiver clone 与链式 add/get 叠加产生的 O(n²) 退化；当前源码满足同步、不逃逸局部 `List[Int]` 的保守形状检查，因此 LLVM 使用 compiler-private contiguous `{data, len, cap}` storage。canonical append loop 在 preheader 读取 header 并以 SSA/phi 携带 data/len/cap，非增长迭代不再重载 header，reserve 成功后只重载 data/cap，element store 后立即提交 len；仍然从空 list 几何增长，不做预 reserve。随后零起点 scan 与 build 使用同一稳定 end，且直接穷尽匹配 `Option[Int]`，因此 exact-length proof 可以删除逐元素 upper-bound 与不可达 `None` edge；独立的 checked checksum 仍然保留。storage 不经过 GC，所有退出路径显式释放。exact proof 的 direct/Unit shape、end、binder 或同一 list 不变性无法证明时保留 checked generic-native get；只有 native-storage eligibility 失败才回退当前 universal `Value` lowering。

这些 case 固定的是当前 compiler-private 热路，不是统一容器/record ABI。`NativeLayout` catalog 已统一 scalar 与 direct-primitive POD record 的物理分类；private callable selector 支持 scalar 以及 POD result/`mut self` InOut，并为 native 与 universal body 分开计算 requirement。universal fallback 是当前不兼容 aggregate 边界的语义实现，不是历史兼容层。普通 POD 参数、readonly receiver、合同/invariant 和 fib assumed body 仍是 P0；List/managed generic layout、统一 clone/trace/drop plan 与 machine-instance identity 仍待扩展。每个 case 内五种语言继续使用完全相同的规模；若最快实现接近进程启动时间，其相对倍数只能视为下界。

## 运行

先构建当前 checkout 的 Loom 编译器和 runner，再运行标准采样：

```sh
cargo +1.88.0 build --release -p loom-cli -p loom-benchmark
target/release/loom-benchmark --output target/basic-benchmark.json
```

标准 profile 每个 case 先 warmup 3 次，再为每种语言采样 10 次；语言顺序按 case 和 round 轮换。快速正确性 smoke 使用较小规模、1 次 warmup 和 3 次测量：

```sh
target/release/loom-benchmark --quick --output target/basic-benchmark-quick.json
```

需要让热计算更充分地压过进程启动噪声时，可用独立 throughput profile。它把 `int_lcg`/`record_method` 放大到 100,000,000，`list_build_scan` 放大到 10,000,000，`fib_recursive` 放大到 40，默认 2 次 warmup 和 5 次测量；standard 仍使用上表规模。`--throughput` 与 `--quick` 互斥，与 standard 一样受 busy-host guard 约束；放大规模仍不把这些 synthetic case 变成语言排名：

```sh
target/release/loom-benchmark --throughput --output target/basic-benchmark-throughput.json
```

可重复 `--case NAME` 只选部分用例，也可用 `--warmups N`、`--runs N` 覆盖次数。每次 fixture 执行都有 deadline：quick/standard/throughput 默认分别为 10/30/60 秒，可用 `--timeout-seconds N` 覆盖；超时进程会被 kill 并 wait 回收，runner 报告对应 stdout/stderr 后失败，避免算法退化长期挂起或继续进入后续样本。runner 默认使用 `target/release/loomc`、`go`、Rust 1.88.0（`rustup run 1.88.0 rustc`）、`clang` 和 `clang++`；`LOOM_BENCH_LOOMC`、`LOOM_BENCH_GO`、`LOOM_BENCH_RUSTC`、`LOOM_BENCH_CC`、`LOOM_BENCH_CXX` 可覆盖单个工具路径。

standard/throughput profile 会在构建前记录 1 分钟 load average；超过逻辑 CPU 数的 75% 时拒绝产生看似精确的报告。等机器空闲后重跑是默认处理；确实需要保存带噪声的诊断样本时可显式传 `--allow-busy-host`。`--quick` 是 correctness smoke，不受此预检约束，其耗时不应进入趋势图。开发时可以在忙机上用更大规模、release IR、汇编或 retired-instruction counter 验证“每个元素少了固定指令”一类结构变化。retired instructions 不在 v1 JSON 内；只有在同 binary/ISA/profile/scale 下保存原始样本、命令和 commit 才能作结构对照，该手动样本的 wall time 和语言倍数不能写入受控结果。

## 结果边界

JSON 保存工具版本和完整编译命令、源码 SHA-256、构建前 load average、每次执行 deadline、一次构建时间、binary bytes、原始 runtime 纳秒样本、median/p05/p95/min/max/mean 和相对本 case 最快 median。runtime 是独立进程从 spawn 到 exit 的 wall time，包含参数解析、实际运算、动态 checksum 比较与固定输出。throughput 默认只有 5 个样本，因此 p05/p95 实际等同 min/max，不能解释成稳定尾延迟。

构建时间只有一次 cold-like sample；binary size 还受静态/动态 runtime 和 strip 策略影响。共享机器结果不应做严格回归阈值，跨机器报告也不能直接求排名。稳定趋势应使用固定硬件和固定工具链；warm/incremental build、peak RSS、能耗和 profiler 属于后续独立证据。
