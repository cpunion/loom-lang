# SQLite migration trial

On macOS, run `loom run tools/deployment/sqlite_migration_trial -- <new-success-db> <new-failure-db>` with two nonexistent database paths. The trial exercises a fixed offline orders upgrade, downgrade, and re-upgrade, plus a failed upgrade that must not record completion.

This tests the SQLite data and event transactions, including an explicit retry after a failed transaction and refusal to roll back after trigger tampering. It is not general deployment correctness. The executor accepts only the exact `STRICT` table definitions and migration catalog checked in `sqlite_migration.loom`; application writers must be stopped while it runs.

The fixture imports v1 as **assumed**. Its basis digests are example values supplied by the caller, not recomputed from a checked Loom artifact or lockfile. An observed event here means this narrow SQL transaction completed, not that arbitrary application behavior or artifact identity was proved.

The archive, ledger, and migration triggers are tool-managed. Unrestricted external SQL can alter them after completion; SQLite `STRICT` also does not prove that stored `TEXT` bytes are valid UTF-8. Typed application access must enforce that boundary.
