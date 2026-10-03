# CPU workers

```sh
LOOM_OPT_LEVEL=3 target/loom build benchmarks/workers --output target/worker-bench
time target/worker-bench serial
time target/worker-bench parallel
```

Both modes compute the same four dependent-integer kernels and print the same
checksum. They use the same worker-enabled executable and instrumentation;
this measures parallel execution, not the cost of adding instrumentation to a
previously local program. Alternate modes across several fresh-process samples
on an otherwise idle machine. Pool size is bounded by available CPUs and four.

Use `scripts/benchmark-basic.mjs` separately for no-worker scalar, record and
List comparisons against C/Go/Rust/Zig. GC stress is a correctness gate, not a
performance setting.

The `local-serial` and `local-parallel` modes instead update invocation-local
Lists through aliases, four million checked read/write pairs in total. They
exercise escape-sensitive guard removal, not shared atomic updates. Both modes
must print the same checksum; compare identical optimization levels and report
fresh-process medians separately from compiler time.
