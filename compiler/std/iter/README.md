# Iterators

`Iterator` has an associated `Item` type and `next(self Self) Option[Self.Item]`.
Implement it for an application type or use `from_list(values)` and the half-open
`range(start, end)`. Generic and `dyn Iterator[Item = T]` consumers share this API.

```loom
import std.iter.range
import std.iter.filter
import std.iter.map
import std.iter.take
import std.iter.collect

fn main() {
    let even = filter(range(0, 100), fn(value Int) Bool { value % 2 == 0 })
    let doubled = map(even, fn(value Int) Int { value * 2 })
    let first = collect(take(doubled, 3))
    assert first[0] == 0 && first[1] == 4 && first[2] == 8
}
```

`map`, `filter` and `take` are lazy. `collect` produces a fresh outer List;
`fold` threads an accumulator; `any`/`all` short-circuit. Callbacks run in pull
order, without prefetching or parallelism. A callback fault propagates normally;
already-consumed input is not replayed. `take` requires a nonnegative count and
does not pull when its allowance is exhausted.

List iteration captures a fixed shared view: subsequent element updates remain
visible, appends are excluded, and removing/replacing a tail does not substitute
new identities for captured elements. It is not a content snapshot. Copies of
the provided stateful iterators share their position. Create another iterator to
start an independent traversal. Generic adapters inherit their source's sharing.

Consumers stop at the first `None`. `map` and `filter` forward their source's
exhaustion behavior; user iterators may resume later. `take` stays exhausted after
either its allowance or its source ends. These are ordinary source-library
policies, not compiler assumptions about purity, termination or permanent EOF.
No new loop syntax, coroutine protocol or runtime primitive is involved.
