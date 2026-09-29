# Asynchronous process capture

Run an executable with literal arguments, capture its binary output, and forward
its status. No shell is involved. From the repository root:

```sh
target/loom check compiler/examples/async_processes
target/loom run compiler/examples/async_processes -- target/loom --help
target/loom test compiler/std/process/tasks
cargo test -p loom-native --test async_process
```

`std.process.tasks.capture` and `capture_input` return ordinary Tasks and accept
the same optional `std.process.Options` as synchronous capture. `capture_input`
supplies binary stdin. Arguments, input and child-local options are copied when
the Task reaches its native wait, not when its handle is created. Other Tasks
can run while the child and its pipes are active.

The existing native I/O pool limits concurrent jobs to four per owner. Each
active capture also uses pipe-drain threads; the total native thread count is
not four. Completed output is copied into GC-rooted buffers on the owner thread.
Output is buffered without a size limit, not streamed.

Cancellation drops queued work, but drains a running capture and reaps the
direct child before returning. It does not terminate a process tree. A hung
child or descendant retaining a pipe can therefore delay cancellation;
`std.task.deadline` is not a hard process timeout.

The native test checks O0/O2 and moving GC, full-duplex binary pipes, input and
argument snapshots, child-local options, and cancellation. Its child requires
an acknowledgement from another Task, so synchronous capture cannot pass.
