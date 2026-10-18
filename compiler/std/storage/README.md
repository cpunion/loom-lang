# Storage rows

Source-only storage descriptors and typed row codecs; no database driver or
deployment policy is built into this package. IDs are application-supplied stable
identities, independent of physical SQL names.

## Record codecs

`record_codec[T](table[, aliases])` returns `Result[PreparedRowCodec[T], CodecError]`.
For a visible unrefined record, ordinary compile-time field iteration generates
the field encoders and checked assembly. Records used from another package must
be `pub`; private types remain opaque. Normal resource-construction restrictions
still apply.

By default, a field name selects the column with that stable ID. Optional
`List[(Text, Text)]` aliases specify `(field_name, column_id)`. Preparation requires
a bijection covering all fields and columns with matching physical kinds and
nullability. Unknown or duplicate aliases, missing/duplicate column selections,
unsupported field codecs and shape mismatches return `InvalidSchema`.
Physical column order determines row order, not record declaration order.

The prepared codec privately snapshots the descriptor and resolved positions.
Later edits to the caller's descriptor or aliases do not change it. Row width,
kind and nullability are still validated at encode/decode boundaries.
`codec_table(codec)` returns an independent metadata copy for drivers to select
the same physical columns in the decoder's order.

`ColumnValue` defines each field's representation:

- `column_kind()` and `column_nullable()` describe its physical cell.
- `to_column(self Self)` encodes a value.
- `from_column(value Value)` returns a checked `Result[Self, CodecError]`.

Implementations are supplied for Int, Float, Text, Bytes, Bool (integer 0/1), and
`Option[T ColumnValue]` (NULL for None). Some(NULL) is rejected because a single
cell cannot distinguish it from None. Bytes retain ordinary sharing, not a deep
snapshot. User-defined/refined types provide an explicit implementation;
decoders use their normal checked constructors. Test-only implementations do not
become discoverable from production generic code.

See the [runnable example](../../examples/storage/README.md), including stable-ID
mapping and a constrained amount decoder. No per-record handwritten encoder or
runtime type registry is required.

## Custom mappings

`RowCodec[T]` remains available for explicit flattening and application-specific
policies. `encode_row` / `decode_row` validate caller-owned mutable descriptors;
`prepare_codec` freezes a descriptor for repeated `encode_prepared_row` /
`decode_prepared_row` calls. Callback state and Bytes may still be shared.
`validate_table` and `validate_row` can validate descriptors and physical rows
without a logical codec.

A valid descriptor or successful round trip does not prove an upgrade compatible.
Deployed-state comparisons still require explicit migration, failure and recovery
policies.
