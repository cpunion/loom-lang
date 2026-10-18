# Binary64

`encode_be(value Float) Bytes` returns a fresh eight-byte big-endian IEEE 754
encoding. `decode_be(bytes) Result[Float, DecodeError]` accepts exactly eight
bytes; other lengths return `WrongWidth`. Finite values, infinities and zero
signs round trip exactly. All NaN patterns decode to NaN; encoding any NaN emits
the canonical quiet NaN `7ff8000000000000`, not its original payload or sign.

Both functions use ordinary Loom arithmetic and work in compile-time execution.
They add no runtime primitive or database dependency. Callers synchronize shared
Bytes against concurrent writes while decoding. A storage engine may apply its
own affinity or canonicalization after decoding; these byte guarantees do not
prove database round-trip behavior.
