# SQLite migration trial

On macOS, build the two fixture applications with receipts, then run the trial
with two nonexistent database paths:

```sh
loom build tools/deployment/fixture_v1 --output /tmp/loom-v1 --receipt /tmp/loom-v1.receipt
loom build tools/deployment/fixture_v2 --output /tmp/loom-v2 --receipt /tmp/loom-v2.receipt
loom run tools/deployment/sqlite_migration_trial -- /tmp/loom-success.sqlite /tmp/loom-failure.sqlite /tmp/loom-v1.receipt /tmp/loom-v2.receipt
```

The trial exercises a fixed offline orders upgrade, downgrade, and re-upgrade,
plus a failed upgrade that must not record completion.

This tests the SQLite data and event transactions, including an explicit retry after a failed transaction, rejection of a changed replacement plan on retry, and refusal to roll back after trigger tampering. The attempt ledger retains the fixed action version and all plan inputs as one digest; it stores no row values. This is not general deployment correctness. The executor accepts only the exact `STRICT` table definitions and migration catalog checked in `sqlite_migration.loom`; application writers must be stopped while it runs.

The fixture imports v1 as **assumed**. Both bases include verified receipts
from real Loom builds; the executor rehashes the target artifact before each
transition. An observed event means this narrow SQL transaction completed.
The receipt does not prove the operator-supplied schema matches application
behavior, and it is not signed supply-chain evidence.

The archive, ledger, and migration triggers are tool-managed. Unrestricted external SQL can alter them after completion; SQLite `STRICT` also does not prove that stored `TEXT` bytes are valid UTF-8. Typed application access must enforce that boundary.
