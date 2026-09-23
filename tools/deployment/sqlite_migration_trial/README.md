# SQLite migration trial

On macOS, run `loom run tools/deployment/sqlite_migration_trial -- <new-success-db> <new-failure-db>` with two nonexistent database paths. The trial exercises a fixed offline orders upgrade, downgrade, and re-upgrade, plus a failed upgrade that must not record completion.

This tests the SQLite data and event transactions, not general deployment correctness. The fixture explicitly imports v1 as **assumed**, and its basis digests are example values supplied by the caller. The executor does not recompute them from a checked Loom artifact or prove arbitrary application behavior. It assumes application writers are stopped; it supports only the exact mapping and catalog checked in `sqlite_migration.loom`.
