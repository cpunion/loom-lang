# SQLite migration trial

On macOS, build the fixture applications with receipts, then run the trial
with three nonexistent database paths. Node.js is used only by the bundled
fault-injection fixture; the deployment library requires only the SQLite tool.

```sh
migration_dir="$(mktemp -d)"
loom build tools/deployment/fixture_v1 --output "$migration_dir/v1" --receipt "$migration_dir/v1.receipt"
loom build tools/deployment/fixture_v2 --output "$migration_dir/v2" --receipt "$migration_dir/v2.receipt"
loom build tools/deployment/fixture_v1_hotfix --output "$migration_dir/hotfix" --receipt "$migration_dir/hotfix.receipt"
loom run tools/deployment/sqlite_migration_trial -- \
  "$migration_dir/success.sqlite" "$migration_dir/failure.sqlite" "$migration_dir/rollback.sqlite" \
  "$migration_dir/v1.receipt" "$migration_dir/v2.receipt" "$migration_dir/hotfix.receipt"
```

The trial exercises a fixed offline orders upgrade, downgrade, and re-upgrade,
plus failed fresh and retained upgrades that must not record completion. After
rollback, it edits a restored zero amount and a retained note under v1, adds a
new zero-valued order, then forces a retained re-upgrade failure through a
damaged archive row. It checks that the failed attempt preserves those rows and
tables, leaves the effective basis unknown, and needs explicit repair and
resume. The successful retry preserves the v1 positive edit and note, while
replacing and archiving only the new zero. These are exact assertions for this
fixed schema and sample rows, not a proof for arbitrary migrations or writers.

The ordinary [`orders_migration`](../orders_migration/plan.loom) Loom module
declares a typed `SqliteMigrationPackage`; the trial instantiates it twice with
different package IDs and replacement values; the second reuses retained data
after downgrade. The package binds source/target schemas and verified receipt
identities, requires an explicit failure policy and forward/recovery/re-upgrade actions, and
selects the existing positive-amount operation. No arbitrary SQL is accepted.

This tests the SQLite data and event transactions, including an explicit retry
after a failed transaction, rejection of a changed package plan on retry, and
refusal to roll back after trigger tampering. The attempt ledger retains the
package ID, receipt identities, fixed action version, and plan inputs as one
digest, including the failure policy; it stores no row values. This is not general deployment correctness.
The executor accepts only the exact `STRICT` table definitions and catalog
checked in `sqlite_migration.loom`; application writers must be stopped.

`inspect_sqlite_migration` remains an offline ledger and catalog check. The
explicit `inspect_live_sqlite_upgrade` call additionally reads the current
SQLite layout and stored rows for either `FreshUpgrade` or
`RetainedReupgrade`. It returns only `ChecksPassed`, `LayoutMismatch`, or
`DataOrLineageMismatch`, the recorded attempt state, and a recovery action:
`SqliteUpgradeRecovery.None`, `Resume`, or `Rollback`. It can inspect matching
started/failed upgrades and failed-upgrade recovery without changing the ledger.
The layout and data/lineage queries are separate
read-only snapshots; their result may become stale, does not establish that
application code obeys the declared mapping, and never authorizes execution.
The executor checks again inside its write transaction.

`FailurePolicy.Stop` retains the failed attempt for explicit resume.
`FailurePolicy.Rollback` instead starts recovery after a failed upgrade process.
Because the data and completion event share one SQLite transaction, recovery
does not replay the upgrade or run a v2 downgrade: it checks the old source
layout, row constraints, retained lineage and exact ledger event under a write
lock before recording `RollbackComplete`. The failed upgrade still returns an
error. A missing acknowledgement after a committed upgrade remains an outcome
to inspect; it must not initiate rollback of that observed completion.

Failed recovery leaves `RollbackFailed` and no established effective basis.
After explicit repair, `recover_failed_sqlite_upgrade` retries the matching
recovery; repeating it after recorded completion adds no event. A successfully
completed upgrade uses `downgrade_sqlite_migration`, not failed-upgrade recovery.
The trial covers automatic recovery, damaged retained data, repair, and later
re-upgrade with preserved notes. Unstarted proposals and uncommitted upgrades
cannot establish retained-data lineage: that needs a real completed upgrade and
its rollback, unaffected by later unused plans.

A registered `NotDeployed` plan leaves the preceding observed or assumed basis
effective for inspection. `replace_unstarted_attempt` explicitly replaces that
named plan without treating its unused target as deployed; the obsolete identity
cannot be started or reused. `cancel_unstarted_attempt` withdraws a plan by
appending `Cancelled`, preserving the preceding basis and closing its identity.
Repeating cancellation returns the recorded result, even after a later plan.
`inspect_attempt` reads a named attempt's recorded outcome after validating the
whole ledger; it does not observe current application data. Started, failed and
rollback-in-progress attempts cannot be cancelled and remain unresolved.
The [ledger trial](../sqlite_trial/main.loom) exercises this
planning lifecycle against SQLite without changing application data.
`start_sqlite_migration` can start an already registered package plan only when
its identity, bases, digest and failure policy match. The migration trial replaces
an unused proposal, rejects a same-ID different package without appending an
event, then starts the selected package and runs the data migration.

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
