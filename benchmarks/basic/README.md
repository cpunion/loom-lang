# Loom 基础跨语言基准

这组受控微基准比较同一台机器上的 Loom release LLVM、Go、Rust、C 和 C++。它用于定位当前实现的数量级和优化方向，不是通用语言排名。

## 用例

五份源码都接受动态命令行 `CASE SIZE EXPECTED`；Rust runner 独立计算 `EXPECTED`，程序执行被测算法并校验结果，成功时只输出 `Unit`。答案是运行时参数而不是源码常量，优化器不能据此折叠热计算：

| case | 默认规模 | 主要路径 | 可观察 checksum |
|---|---:|---|---|
| `int_lcg` | 2,000,000 | 有界 `Int` 乘除和循环 | 最终 LCG state |
| `record_method` | 500,000 | mutable record method 与周期整数累计 | `total` 和 `calls` |
| `list_build_scan` | 10,000 | 从空 List 几何增长，再逐项 checked get | 元素和与长度 |
| `fib_recursive` | 32 | 非尾递归函数调用 | Fibonacci 值 |

所有数值都落在 signed 64-bit 范围内。Loom 和 Rust 保留 checked integer 行为；C、C++ 和 Go 的固定规模也不会溢出。List 用例不预留容量，五种实现都从空容器开始；它同时测量容器增长、元素写入和边界检查，不把结果解释为纯内存带宽。

Text 拼接和 `dyn` 分派没有放入 v1：各语言的字符串表示、分配策略、虚调用去虚化条件并不天然等价。它们应在 ABI/layout 优化阶段用单独、明确语义的 case 加入。

默认规模按当前 Loom 实现校准，使标准矩阵能在开发机上有界完成；它们不是成熟实现的吞吐上限。尤其 `list_build_scan` 的规模刻意较小，当前通用 `Value`/List 路径会成为主导热点。每个 case 内五种语言仍使用完全相同的规模；若最快实现接近进程启动时间，其相对倍数只能视为下界。

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

可重复 `--case NAME` 只选部分用例，也可用 `--warmups N`、`--runs N` 覆盖次数。runner 默认使用 `target/release/loomc`、`go`、Rust 1.88.0（`rustup run 1.88.0 rustc`）、`clang` 和 `clang++`；`LOOM_BENCH_LOOMC`、`LOOM_BENCH_GO`、`LOOM_BENCH_RUSTC`、`LOOM_BENCH_CC`、`LOOM_BENCH_CXX` 可覆盖单个工具路径。

标准 profile 会在构建前记录 1 分钟 load average；超过逻辑 CPU 数的 75% 时拒绝产生看似精确的报告。等机器空闲后重跑是默认处理；确实需要保存带噪声的诊断样本时可显式传 `--allow-busy-host`。`--quick` 是 correctness smoke，不受此预检约束，其耗时不应进入趋势图。

## 结果边界

JSON 保存工具版本和完整编译命令、源码 SHA-256、构建前 load average、一次构建时间、binary bytes、原始 runtime 纳秒样本、median/p05/p95/min/max/mean 和相对本 case 最快 median。runtime 是独立进程从 spawn 到 exit 的 wall time，包含参数解析、实际运算、动态 checksum 比较与固定输出。

构建时间只有一次 cold-like sample；binary size 还受静态/动态 runtime 和 strip 策略影响。共享机器结果不应做严格回归阈值，跨机器报告也不能直接求排名。稳定趋势应使用固定硬件和固定工具链；warm/incremental build、peak RSS、能耗和 profiler 属于后续独立证据。
