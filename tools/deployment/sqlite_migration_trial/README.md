# SQLite migration trial

On macOS, build the two fixture applications with receipts, then run the trial
with two nonexistent database paths:

```sh
loom build tools/deployment/fixture_v1 --output /tmp/loom-v1 --receipt /tmp/loom-v1.receipt
loom build tools/deployment/fixture_v2 --output /tmp/loom-v2 --receipt /tmp/loom-v2.receipt
loom build tools/deployment/fixture_v1_hotfix --output /tmp/loom-v1-hotfix --receipt /tmp/loom-v1-hotfix.receipt
loom run tools/deployment/sqlite_migration_trial -- /tmp/loom-success.sqlite /tmp/loom-failure.sqlite /tmp/loom-v1.receipt /tmp/loom-v2.receipt /tmp/loom-v1-hotfix.receipt
```

The trial exercises a fixed offline orders upgrade, downgrade, and re-upgrade,
plus a failed upgrade that must not record completion.

The ordinary [`orders_migration`](../orders_migration/plan.loom) Loom module
declares a typed `SqliteMigrationPackage`; the trial instantiates it twice with
different package IDs and replacement values; the second reuses retained data
after downgrade. The package binds source/target schemas and verified receipt
identities, declares Stop policy and forward/recovery/re-upgrade actions, and
selects the existing positive-amount operation. No arbitrary SQL is accepted.

This tests the SQLite data and event transactions, including an explicit retry
after a failed transaction, rejection of a changed package plan on retry, and
refusal to roll back after trigger tampering. The attempt ledger retains the
package ID, receipt identities, fixed action version, and plan inputs as one
digest; it stores no row values. This is not general deployment correctness.
The executor accepts only the exact `STRICT` table definitions and catalog
checked in `sqlite_migration.loom`; application writers must be stopped.

The fixture imports v1 as **assumed**. Both bases include verified receipts
from real Loom builds; the package API rehashes both artifacts before each
transition. An observed event means this narrow SQL transaction completed.
The receipt does not prove the operator-supplied schema matches application
behavior, and it is not signed supply-chain evidence.

The separate `inspect_hotfix` call verifies both build receipts, the effective
recorded starting basis, and exact equality of the declared storage mappings.
Its result is explicitly `Unproven` and `can_execute = false`; it never writes
an event or authorizes a hotfix. Identical declarations cannot prove the new
application reads and writes stored data compatibly.

The archive, ledger, and migration triggers are tool-managed. Unrestricted external SQL can alter them after completion; SQLite `STRICT` also does not prove that stored `TEXT` bytes are valid UTF-8. Typed application access must enforce that boundary.
