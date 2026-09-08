# Cleanup across suspension

```sh
target/loom check compiler/examples/async_cleanup
target/loom test compiler/examples/async_cleanup
target/loom run compiler/examples/async_cleanup
```

The program prints `cleanup finished`. `work` keeps a scoped guard and late-bound
defer captures across two real timer waits. The inner block cleans up immediately;
the outer block observes its changes. Return values are evaluated before cleanup,
so replacing `packet` during cleanup does not replace the returned value.

Change `value = 2` and its assertions to follow the updates. The colocated tests
also cover early/error returns, loop exits, and captured match bindings.

`std.resource.NoSuspend` is independent of `MustScope`: it forbids retaining a
value across await, including inside an aggregate or a pending call argument.
Bindings remain active until their block ends, even after their last read.
End that block before awaiting. Cleanup bodies themselves cannot await or create
Tasks. Cancellation drains children before the parent's cleanup; an unstarted
task has not acquired resources or registered cleanup.

This example uses a source-defined guard, not an asynchronous file API. Async
file/socket adapters and tuple/list task composition remain unfinished.
