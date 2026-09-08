# Timer programming trial

From the repository root:

```sh
target/loom fmt --check compiler/examples/timers
target/loom check compiler/examples/timers
target/loom test compiler/examples/timers
target/loom run compiler/examples/timers
```

The two tests pass; the program prints `queued` and then `timers finished`.
Both child tasks are queued before the first await. A waiting timer suspends its
Loom frame; the owner runs other ready tasks or blocks in the OS reactor when
there is no ready work. It does not spin or create a thread per timer.

Change the durations to try overlapping waits. `sleep_ms` and `sleep_ns` measure
from the start of their task body, not from the call that queues it. For a fixed
deadline, compute `monotonic_ns() + duration` and call `sleep_until_ns(deadline)`.
The clock has an unspecified process-local origin, not calendar time; do not
persist its deadlines across processes. Past deadlines and zero durations are
valid, but do not promise to yield. Negative durations fail their precondition;
deadline arithmetic uses ordinary checked integer arithmetic.

This is cooperative scheduling on one owner thread. Synchronous file I/O still
blocks that thread; async file/socket operations and task joins are unfinished.
