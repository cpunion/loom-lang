# SHA-256

`digest(bytes)` returns a fresh 32-byte digest; `hex(bytes)` returns lowercase
hexadecimal Text. Both also accept `(bytes, start, end)` to hash a half-open range
without copying it. Inputs are neither modified nor retained.

For files and other chunked input, retain one incremental hasher:

```loom
import std.hash.sha256.new_hasher
import std.hash.sha256.update
import std.hash.sha256.hex

fn checksum(first Bytes, second Bytes) Text {
    let state = new_hasher()
    update(state, first)
    update(state, second)
    hex(state)
}
```

`update(state, bytes, start, end)` accepts a range too. Updates consume the input
immediately, retaining at most 63 unfinished message bytes in fixed-size scratch
storage. Complete blocks are read directly from the supplied buffer. Input chunk
sizes do not affect the digest; empty updates are allowed.

`digest(state)` and `hex(state)` take nondestructive snapshots. Later updates
continue the same message, and changing a returned digest cannot change the
hasher. Assignment shares the mutable state; it does not clone a hash prefix.
Serialize updates and snapshots, and synchronize concurrent writes to input
buffers. No locking, finalizer or resource cleanup is involved.

The implementation is ordinary Loom and shares one compression kernel between
one-shot and incremental operations. It follows
[FIPS 180-4](https://csrc.nist.gov/pubs/fips/180-4/upd1/final), with the entire
message length encoded only during snapshot padding. Invalid ranges or a total
message length of at least 2^61 bytes fault. The tests cover standard vectors,
partition/padding boundaries, snapshots and compile-time execution; they are not
a formal cryptographic verification or a certification claim.

```sh
target/loom test compiler/std/hash/sha256
```
