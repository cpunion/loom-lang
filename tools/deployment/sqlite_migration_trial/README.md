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

`inspect_sqlite_migration` remains an offline ledger and catalog check. The
explicit `inspect_live_sqlite_upgrade` call additionally reads the current
SQLite layout and stored rows for either `FreshUpgrade` or
`RetainedReupgrade`. It returns only `ChecksPassed`, `LayoutMismatch`, or
`DataOrLineageMismatch`, the recorded attempt state, and whether a failed attempt
needs an explicit resume. It can inspect a matching `Started` or `Failed`
attempt without changing the ledger. The layout and data/lineage queries are separate
read-only snapshots; their result may become stale, does not establish that
application code obeys the declared mapping, and never authorizes execution.
The executor checks again inside its write transaction.
`inspect_live_sqlite_downgrade` is the matching read-only preflight for the
fixed v2-to-v1 rollback. It requires the same verified package and a matching
completed, rollback-started, or rollback-failed attempt. It distinguishes exact
table/trigger layout from row and restoration-lineage mismatches, reports when
a prior rollback needs retry, and does not append an event. The trial checks a
clean rollback source, altered archived data, and a tampered trigger before and
after a failed rollback. Like the upgrade inspection, this is a potentially
stale advisory snapshot, not authorization to execute; the rollback transaction
rechecks its source.

An ordinary Loom caller with a declared `SqliteMigrationPackage` can request
the live check explicitly:

```loom
let inspection = inspect_live_sqlite_upgrade("/usr/bin/sqlite3", database,
    bundle, SqliteLivePhase.FreshUpgrade)?
assert inspection.status == SqliteLiveStatus.ChecksPassed
```

The runnable [`main.loom`](main.loom) imports these public `deployment` API
names and exercises the fresh and retained paths.

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
