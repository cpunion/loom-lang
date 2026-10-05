use std::{fs, path::Path, process::Output};

mod common;
use common::success;

fn cached(mode: &str, package: &Path, cache: &Path, extra: &[&str]) -> Output {
    common::command(&[mode])
        .arg(package)
        .arg("--frontend-cache")
        .arg(cache)
        .args(extra)
        .env("LOOM_NATIVE_TIMINGS", "1")
        .output()
        .unwrap()
}

fn checked(output: &Output, hit: bool) {
    success(output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let status = if hit { "hit" } else { "miss" };
    assert_eq!(
        stderr.matches("loom cache: frontend ").count(),
        1,
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("loom cache: frontend {status}\n")),
        "{stderr}"
    );
}

#[test]
fn independent_impl_groups_replay_native_methods_from_current_source() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    for entry in fs::read_dir(common::root().join("compiler/examples/data_packs")).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "loom")
        {
            fs::copy(&path, package.join(path.file_name().unwrap())).unwrap();
        }
    }
    checked(&cached("run", &package, &cache, &[]), false);
    fs::write(package.join("aaa.loom"), "fn unrelated() {}\n").unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    assert!(
        trace.contains("bodies reused ") && !trace.contains("bodies reused 0"),
        "{trace}"
    );
    let fresh = common::loom(&["run", package.to_str().unwrap()]);
    success(&fresh);
    let replayed = cached("run", &package, &cache, &[]);
    checked(&replayed, true);
    assert_eq!(replayed.stdout, fresh.stdout);
    let path = package.join("independent_impls.loom");
    let source = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        source.replace("right = right + 1", "right = right - 1"),
    )
    .unwrap();
    let invalid = cached("check", &package, &cache, &[]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("postcondition"));
}

#[test]
fn entry_storage_contracts_survive_edited_body_cache_reuse() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
import std.list.length
import std.list.push
fn append(values List[Int]) Int
ensures result == old(length(values))
ensures length(values) == old(length(values)) + 1
{
    let before = length(values)
    push(values, 0)
    before
}
fn twice(values List[Int]) Int
ensures result == old(length(values))
ensures length(values) == old(length(values)) + 2
{
    let first = append(values)
    assert append(values) == first + 1
    first
}
fn replace(values List[Int], item Int) Int
requires length(values) > 0
ensures result == old(values[0])
ensures values[0] == item
{
    let before = values[0]
    values[0] = item
    before
}
fn elements(values List[Int]) Int
requires length(values) > 0
ensures result == old(values[0])
{
    let first = replace(values, 7)
    assert replace(values, 9) == 7
    first
}
fn increase(values List[Int], turns Int) Int
requires length(values) > 0 && values[0] >= 0
ensures result >= old(values[0])
{
    var turn = 0
    while turn < turns {
        values[0] = values[0] + 1
        turn = turn + 1
    }
    values[0]
}
fn frame_write(values List[Int]) Int
ensures result == 0
{
    values[1] = 7
    0
}
fn frame_user(values List[Int]) Int
requires length(values) >= 2
ensures result == old(values[0])
{
    discard frame_write(values)
    values[0]
}
fn optional(values List[Int]) Int
ensures old(length(values)) == 0 || result == old(values[0])
ensures old(length(values)) != 0 || result == 0
{
    if length(values) == 0 {
        return 0
    }
    let before = values[0]
    values[0] = 13
    before
}
fn main() {
    let values List[Int] = []
    assert optional(values) == 0
    assert twice(values) == 0
    assert twice(values) == 2
    assert elements(values) == 0
    assert values[0] == 9
    assert increase(values, 2) == 11
    assert frame_user(values) == 11
    assert optional(values) == 11
    assert values[0] == 13
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(&path, format!("fn unrelated() Int {{ 1 }}\n{source}")).unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    let bodies: usize = trace
        .split(", bodies reused ")
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(bodies >= 8, "{trace}");
    checked(&cached("run", &package, &cache, &[]), true);
    // Replacing an entry guard with post-state length must not reuse its proof.
    fs::write(
        &path,
        source.replace("old(length(values)) == 0 ||", "length(values) == 0 ||"),
    )
    .unwrap();
    let invalid_guard = cached("check", &package, &cache, &[]);
    assert!(!invalid_guard.status.success());
    assert!(String::from_utf8_lossy(&invalid_guard.stderr).contains("bounds"));
    // The callee's declared contract stays true, but its write footprint changes.
    // A caller cannot retain proof of the previously untouched element.
    fs::write(&path, source.replace("values[1] = 7", "values[0] = 7")).unwrap();
    let invalid_frame = cached("check", &package, &cache, &[]);
    assert!(!invalid_frame.status.success());
    assert!(String::from_utf8_lossy(&invalid_frame.stderr).contains("postcondition"));
    // Reused entry/cell observations are not a proof of the edited loop body.
    fs::write(
        &path,
        source.replace("values[0] = values[0] + 1", "values[0] = values[0] - 1"),
    )
    .unwrap();
    let invalid_loop = cached("check", &package, &cache, &[]);
    assert!(!invalid_loop.status.success());
    assert!(String::from_utf8_lossy(&invalid_loop.stderr).contains("postcondition"));
    // The helper edit invalidates dependent summaries, not just its body.
    fs::write(
        &path,
        source.replace("    before\n", "    length(values)\n"),
    )
    .unwrap();
    let invalid = cached("check", &package, &cache, &[]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("postcondition"));
}

#[test]
fn closure_callers_reuse_without_restoring_old_capture_environments() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
import std.time.sleep_ms
fn increase(previous Int, amount Int) Int {
    previous + amount
}
fn counter(start Int) fn(Int) Int {
    var total = start
    fn(amount Int) Int {
        total = increase(total, amount)
        total
    }
}
async fn main() {
    let next = counter(1)
    let first = next(2)
    sleep_ms(1).await
    assert first + next(4) == 10
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(&path, format!("fn unrelated() Int {{ 1 }}\n\n{source}")).unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    let bodies: usize = trace
        .split(", bodies reused ")
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(bodies >= 4, "{trace}");
    let executable = common::executable(directory.path(), "cached-closures");
    checked(
        &cached(
            "build",
            &package,
            &cache,
            &["--output", executable.to_str().unwrap()],
        ),
        true,
    );
    success(&common::run_tasks(&executable));
    // A nested closure's source dependencies remain ordinary invalidation
    // edges, even when its callers have reusable concrete bodies.
    fs::write(
        &path,
        source.replace("previous + amount", "previous + amount + 1"),
    )
    .unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    let failed = cached("run", &package, &cache, &[]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("assertion failed"));
}

#[test]
fn embedded_tests_do_not_disable_production_definition_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
import std.int.to_text
fn answer() Text {
    to_text(7)
}
fn main() {
    assert answer() == "7"
}
test fn excluded() {
    assert false
}
"#;
    fs::write(&path, source).unwrap();
    fs::write(package.join("main_test.loom"), "not production syntax").unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(&path, format!("fn unrelated() Int {{ 1 }}\n{source}")).unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    let bodies: usize = trace
        .split(", bodies reused ")
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(bodies >= 2, "{trace}");
    // Fresh abstract checks may intern additional unused types; compare native
    // behavior, not private type-table numbering.
    let fresh = common::loom(&["run", package.to_str().unwrap()]);
    success(&fresh);
    checked(&cached("run", &package, &cache, &[]), true);
    fs::write(package.join("main_test.loom"), "test fn included() {}\n").unwrap();
    let testing = cached("test", &package, &cache, &[]);
    assert!(!testing.status.success());
    assert!(String::from_utf8_lossy(&testing.stderr).contains("assertion failed"));
}

#[test]
fn scoped_body_reuse_retains_list_cleanup_fault_draining_and_hidden_dispose_edges() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
import std.resource.Dispose
import std.resource.MustScope
import std.list.push
import std.task.outcome
import std.task.Outcome
import std.time.sleep_ms
record Guard {
    id Int
    trace List[Int]
}
impl MustScope for Guard {
}
impl Dispose for Guard {
    fn dispose(self Guard) {
        push(self.trace, self.id)
        if self.id == 2 {
            assert false
        }
    }
}
fn acquire(id Int, trace List[Int]) Guard {
    Guard { id = id, trace = trace }
}
async fn worker(trace List[Int]) {
    defer {
        push(trace, 9)
    }
    scoped values = [acquire(1, trace), acquire(2, trace)]
    sleep_ms(1).await
}
async fn main() {
    let trace List[Int] = []
    match outcome(worker(trace)).await {
        Outcome.Faulted(_) => {}
        _ => {
            assert false
        }
    }
    assert trace[0] == 2 && trace[1] == 1 && trace[2] == 9
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(&path, format!("fn unused() Int {{ 1 }}\n\n{source}")).unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    let bodies: usize = trace
        .split(", bodies reused ")
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(bodies >= 5, "{trace}");
    let executed = common::command(&["run"])
        .arg(&package)
        .arg("--frontend-cache")
        .arg(&cache)
        .env("LOOM_GC_STRESS", "1")
        .env("LOOM_NATIVE_TIMINGS", "1")
        .output()
        .unwrap();
    checked(&executed, true);
    // No explicit dispose() appears in worker's AST. The introduced cleanup
    // edge must still follow the changed implementation, which no longer faults.
    fs::write(&path, source.replace("self.id == 2", "self.id == 3")).unwrap();
    assert!(!cached("run", &package, &cache, &[]).status.success());
    fs::write(&path, source.replace("scoped values", "let values")).unwrap();
    assert!(!cached("check", &package, &cache, &[]).status.success());
}

#[test]
fn dynamic_instances_rebuild_witnesses_slots_and_async_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
concept Other {
    fn other(self Self) Int
}
concept Convert {
    type Item
    fn stored(self Self, item Self.Item) Self.Item
    fn echo[T](self Self, item T) T {
        item
    }
    fn selected(self Self, comptime enabled Bool, item Int) Int {
        comptime if enabled {
            item + 1
        } else {
            item
        }
    }
    async fn fetch(self Self, item Int) Int {
        item
    }
    fn unused(self Self) Int {
        123
    }
}
record Box {
    label Text
}
impl Convert for Box {
    type Item = Text
    fn stored(receiver Box, item Text) Text {
        assert receiver.label == "live"
        item
    }
}
async fn main() {
    let erased dyn Convert[Item = Text] = Box { label = "live" }
    assert erased.stored("kept") == "kept"
    assert erased.echo(true) && erased.echo(9) == 9
    assert erased.selected(true, 6) + erased.selected(false, 10) == 17
    assert erased.fetch(8).await == 8
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(
        &path,
        format!("fn unrelated(item dyn Other) dyn Other {{ item }}\n\n{source}"),
    )
    .unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    assert!(trace.contains(", bodies reused 7"), "{trace}");
    let executed = common::command(&["run"])
        .arg(&package)
        .arg("--frontend-cache")
        .arg(&cache)
        .env("LOOM_GC_STRESS", "1")
        .env("LOOM_NATIVE_TIMINGS", "1")
        .output()
        .unwrap();
    checked(&executed, true);
    success(&common::loom(&["run", package.to_str().unwrap()]));
    // Changing the selected default must rebuild uses, not keep the old slot's
    // implementation just because its receiver type is unchanged.
    fs::write(
        &path,
        source
            .replace("item + 1", "item + 2")
            .replace("== 17", "== 18"),
    )
    .unwrap();
    checked(&cached("run", &package, &cache, &[]), false);
}

#[test]
fn async_instances_reuse_before_lowering_with_real_waits_and_current_labels() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
import std.time.sleep_ms
record Pair {
    first Task[Int]
    second Task[Text]
}
async fn number(item Int) Int {
    sleep_ms(1).await
    assert item > 0
    item
}
async fn label() Text {
    "task"
}
fn forward(item Task[Int]) Task[Int] {
    item
}
async fn main() {
    let callback fn(Int) Task[Int] = number
    let values = Pair {
        first = forward(callback(7))
        second = label()
    }
    assert values.first.await == 7
    assert values.second.await == "task"
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    let shifted = format!("fn unused() Int {{ 1 }}\n\n{source}");
    fs::write(&path, shifted).unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    let bodies: usize = trace
        .split(", bodies reused ")
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(bodies >= 4, "{trace}");
    let executed = common::command(&["run"])
        .arg(&package)
        .arg("--frontend-cache")
        .arg(&cache)
        .env("LOOM_GC_STRESS", "1")
        .env("LOOM_NATIVE_TIMINGS", "1")
        .output()
        .unwrap();
    checked(&executed, true);

    let failing = source.replace("callback(7)", "callback(-7)");
    fs::write(&path, &failing).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    let shifted = format!("fn moved() Int {{ 2 }}\n\n{failing}");
    fs::write(&path, &shifted).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    let failed = cached("run", &package, &cache, &[]);
    assert!(!failed.status.success());
    let (line, text) = shifted
        .lines()
        .enumerate()
        .find(|(_, text)| text.contains("callback(-7)"))
        .unwrap();
    let column = text.find("callback(-7)").unwrap() + 1;
    let diagnostic = format!(
        "{}:{}:{column}: task created here",
        path.canonicalize().unwrap().display(),
        line + 1
    );
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains(&diagnostic), "{stderr}");
}

#[test]
fn method_instances_reuse_defaults_and_keep_inherited_contracts() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let rules = r#"
concept Keep {
    fn keep(self Self, input Int) Int
    requires input > 0
    ensures result == input
}
"#;
    let rules_path = package.join("rules.loom");
    fs::write(&rules_path, rules).unwrap();
    let path = package.join("main.loom");
    let source = r#"
concept Convert {
    type Item
    fn stored(self Self, item Self.Item) Self.Item
    fn echo[T](self Self, item T) T {
        item
    }
    fn selected(self Self, comptime enabled Bool, item Int) Int {
        comptime if enabled {
            item + 1
        } else {
            item
        }
    }
}
impl Convert for Int {
    type Item = Text
    fn stored(receiver Int, item Text) Text {
        item
    }
}
impl Keep for Bool {
    fn keep(receiver Bool, item Int) Int {
        item
    }
}
fn main() {
    assert 1.stored("kept") == "kept" && 1.echo(true)
    assert 1.selected(true, 6) + 1.selected(false, 10) == 17
    assert true.keep(7) == 7
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(&path, format!("fn unused() Int {{ 1 }}\n{source}")).unwrap();
    fs::write(
        &rules_path,
        format!("fn unrelated() Bool {{ true }}\n\n{rules}"),
    )
    .unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    assert!(trace.contains(", bodies reused 6"), "{trace}");
    checked(&cached("run", &package, &cache, &[]), true);
    success(&common::loom(&["run", package.to_str().unwrap()]));
    // Retain the inherited runtime entry check, not just the successful proof.
    fs::write(
        &path,
        source.replace("assert true.keep(7) == 7", "discard true.keep(-1)"),
    )
    .unwrap();
    assert!(!cached("run", &package, &cache, &[]).status.success());
    fs::write(
        &path,
        source.replace(
            "fn keep(receiver Bool, item Int) Int {\n        item",
            "fn keep(receiver Bool, item Int) Int {\n        0",
        ),
    )
    .unwrap();
    assert!(!cached("check", &package, &cache, &[]).status.success());
}

#[test]
fn variadic_instances_reuse_current_expanded_symbols_and_distinct_arities() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
concept Gather {
    fn gather[Ts...](self Self, budget Int, values Ts...) (Ts...)
    requires budget >= 0 {
        values
    }
}
impl Gather for Bool {}
fn bundle[Ts...](items Ts...) (Ts...) {
    items
}
fn keep[Ts...](value Int, ignored Ts...) Int
ensures result == value {
    value
}
fn mapped[Ts...](items (Ts...)) (Ts...) {
    comptime map item in items {
        item
    }
}
fn count[Ts...](items Ts...) Int
ensures result >= 0 {
    var total = 0
    comptime for index, item in items {
        discard item
        total = total + 1
    }
    total
}
fn main() {
    let pair = mapped(bundle(3, true))
    let triple = bundle(1, "two", false)
    assert pair.1 && !triple.2
    let boxed dyn Gather = true
    assert boxed.gather(0, pair...).1
    assert boxed.gather(0, "cached").0 == "cached"
    assert keep(pair.0, triple...) + count(triple...) + count(1) == 7
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(
        &path,
        format!("fn unrelated(item List[Bool]) List[Bool] {{ item }}\n{source}"),
    )
    .unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    assert!(trace.contains(", bodies reused 9"), "{trace}");
    // Fresh arity validation may intern abstract placeholders before the cached
    // path recreates concrete types. Those private IDs need not be byte-equal;
    // both native paths must execute every remapped call/layout correctly.
    let fresh = common::loom(&["run", package.to_str().unwrap()]);
    success(&fresh);
    let executed = cached("run", &package, &cache, &[]);
    checked(&executed, true);
    assert_eq!(executed.stdout, fresh.stdout);
    fs::write(&path, source.replace("    value\n}", "    0\n}")).unwrap();
    let invalid = cached("check", &package, &cache, &[]);
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8_lossy(&invalid.stderr).contains("required postcondition is not proved")
    );
    // None of this package's calls selects five elements. Restoring selected
    // bodies must not bypass the family's arbitrary-width induction proof.
    fs::write(
        &path,
        source.replace(
            "        total = total + 1",
            "        if index >= 4 { return -1 }\n        total = total + 1",
        ),
    )
    .unwrap();
    assert!(!cached("check", &package, &cache, &[]).status.success());
    fs::write(
        &path,
        source.replace("bundle(3, true)", "bundle(3, true, 9)"),
    )
    .unwrap();
    checked(&cached("run", &package, &cache, &[]), false);
    fs::write(&path, source.replace("bundle(3, true)", "bundle(3, 4)")).unwrap();
    assert!(!cached("check", &package, &cache, &[]).status.success());
}

#[test]
fn generic_refinements_reuse_current_predicates_arguments_and_helper_proofs() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
import std.result.Result
import std.result.ConstraintError
import std.list.push
import std.list.length

record State[T] {
    value T
    count Int
}

enum Payload {
    Number(Int)
}

fn valid[T](value State[T]) Bool {
    value.count >= 0
}

type Valid[T] = State[T] where valid(self)

fn checked[T](value State[T]) Result[Valid[T], ConstraintError] {
    Valid(value)
}

fn proven[T](value T, count Int) Valid[T]
requires count >= 0
{
    Valid(raw(value, count))
}

fn raw[T](value T, count Int) State[T]
ensures result.count == count
{
    State {
        value = value
        count = count
    }
}

fn tracked(values List[Int], trace List[Int]) List[Int] {
    push(trace, 7)
    values
}

fn count[T](value Valid[T]) Int
ensures result >= 0
{
    value.count
}

fn main() {
    let values = [1]
    let trace List[Int] = []
    let shared = proven(tracked(values, trace), 0)
    assert length(trace) == 1 && trace[0] == 7
    values[0] = -1
    assert shared.value[0] == -1 && count(shared) == 0
    let payload = proven(Payload.Number(4), 0)
    assert match payload.value {
        Payload.Number(value) => value == 4
    }
    let floating = proven(0.0 / 0.0, 0)
    assert floating.value != floating.value && count(floating) == 0
    assert count(Valid(State {
        value = 7
        count = 0
    })) == 0
    let text = match checked(State {
        value = "ready"
        count = 1
    }) {
        Result.Ok(value) => value
        Result.Err(_) => {
            assert false
            return
        }
    }
    assert count(text) == 1 && text.value == "ready"
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    checked(&cached("run", &package, &cache, &[]), true);
    fs::write(
        &path,
        format!("fn unrelated(value List[Bool]) List[Bool] {{ value }}\n{source}"),
    )
    .unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let fresh = common::loom(&["emit-checked", package.to_str().unwrap()]);
    success(&fresh);
    // Int and Text instances may be interned in a different order while
    // rekeying. Both native executions must retain their distinct predicates.
    let trace = String::from_utf8_lossy(&reused.stderr);
    assert!(
        trace.contains("bodies reused ") && !trace.contains("bodies reused 0"),
        "{trace}"
    );
    success(&common::loom(&["run", package.to_str().unwrap()]));
    checked(&cached("run", &package, &cache, &[]), true);

    // A weaker entry condition cannot keep the old partial-construction proof.
    fs::write(
        &path,
        source.replace("requires count >= 0", "requires count >= -1"),
    )
    .unwrap();
    let rejected = cached("emit-checked", &package, &cache, &[]);
    assert!(!rejected.status.success());

    fs::write(&path, source.replace("value.count >= 0", "value.count > 0")).unwrap();
    let rejected = cached("emit-checked", &package, &cache, &[]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("expected Valid"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    fs::write(&path, source).unwrap();
    checked(&cached("run", &package, &cache, &[]), true);
}

#[test]
fn staged_instances_persist_with_current_types_targets_and_constant_values() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
record Setting {
    delta Int
    metadata (Text, type)
}
fn increment(item Int) Int {
    item + 1
}
fn compute(comptime setting Setting, comptime op fn(Int) Int, item Int) Int {
    op(item) + comptime {
        setting.delta
    }
}
fn choose(comptime enabled Bool, item Int) Int {
    comptime if enabled {
        item + 1
    } else {
        item
    }
}
fn floating(comptime item Float) Float {
    item
}
fn main() {
    assert compute(Setting {
            delta = 2
            metadata = ("first", Int)
        }, increment, 5) == 8
    assert compute(Setting {
            delta = 4
            metadata = ("second", Text)
        }, increment, 5) == 10
    assert choose(true, 1) == 2 && choose(false, 1) == 1
    assert 1.0 / floating(0.0) > 0.0
    assert 1.0 / floating(-0.0) < 0.0
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(
        &path,
        format!("fn unused(item List[Bool]) List[Bool] {{ item }}\n{source}"),
    )
    .unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    assert!(trace.contains(", bodies reused 8"), "{trace}");
    let fresh = common::loom(&["emit-checked", package.to_str().unwrap()]);
    success(&fresh);
    assert_eq!(reused.stdout, fresh.stdout);
    checked(&cached("run", &package, &cache, &[]), true);

    // Same declaration, different instance key: the old specialized body must
    // not survive a changed aggregate constant just because its source matches.
    fs::write(
        &path,
        source
            .replace("delta = 2", "delta = 7")
            .replace("== 8", "== 13"),
    )
    .unwrap();
    checked(&cached("run", &package, &cache, &[]), false);
}

#[test]
fn computed_structural_types_restore_current_identities_after_an_edit() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
import std.meta.tuple
import std.meta.function
import std.option.Option
concept Family {
    type Item[T]
}
impl Family for Bool {
    type Item[T] = (T, Text)
}
fn selected_item(comptime receiver type) type {
    comptime if receiver implements Family {
        receiver.Item[Int]
    } else {
        Text
    }
}
fn labelled[S Family, T](source S, value S.Item[T]) (S.Item[T], Text) {
    discard source
    let Selected = comptime { S }
    let Labelled = comptime { tuple([Selected.Family.Item[T], Text]) }
    let labelled Labelled = (value, "labelled")
    labelled
}
fn increment(value Int) Int {
    value + 1
}
fn answer() (Int, Text) {
    let Pair = comptime { tuple([Int, Text]) }
    let pair Pair = (41, "answer")
    pair
}
fn tagged[T](value T) (T, Text) {
    let Tagged = comptime { tuple([T, Text]) }
    let tagged Tagged = (value, "tagged")
    tagged
}
fn identity[T](value T) T {
    value
}
fn apply[T](value T) T {
    let Callback = comptime { function([T], Option.Some(T)) }
    let callback Callback = identity[T]
    callback(value)
}
fn main() {
    let Callback = comptime { function([Int], Option.Some(Int)) }
    let callback Callback = increment
    let pair = answer()
    assert callback(pair.0) == 42 && pair.1 == "answer"
    assert tagged(true).0 && apply(pair.0) == 41
    assert labelled[Bool, Int](true, pair).0.0 == 41
    let Item = comptime { selected_item(Bool) }
    let item Item = (7, "selected")
    comptime if Item == (Int, Text) {
        assert item.0 == 7
    }
    assert comptime { selected_item(Int) == Text }
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(
        &path,
        format!("fn unused(value List[Bool]) List[Bool] {{ value }}\n{source}"),
    )
    .unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    let bodies: usize = trace
        .split(", bodies reused ")
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(bodies > 0, "{trace}");
    let fresh = common::loom(&["emit-checked", package.to_str().unwrap()]);
    success(&fresh);
    // Fresh checking may intern unused abstract signatures and shift private
    // table IDs. Compare actual unoptimized emission, retaining every live
    // signature, layout, operation and fault rather than those session IDs.
    let restored =
        loom_native::native_input::decode(std::str::from_utf8(&reused.stdout).unwrap()).unwrap();
    let fresh =
        loom_native::native_input::decode(std::str::from_utf8(&fresh.stdout).unwrap()).unwrap();
    use loom_native::codegen::{Backend, EmitOptions, Llvm, Optimization};
    for (name, program) in [("restored", &restored), ("fresh", &fresh)] {
        Llvm.emit(
            program,
            EmitOptions {
                object: &directory.path().join(format!("{name}.o")),
                ir: Some(&directory.path().join(format!("{name}.ll"))),
                test_mode: false,
                optimization: Optimization::O0,
            },
        )
        .unwrap();
    }
    assert_eq!(
        fs::read_to_string(directory.path().join("restored.ll")).unwrap(),
        fs::read_to_string(directory.path().join("fresh.ll")).unwrap()
    );
    checked(&cached("run", &package, &cache, &[]), true);
    fs::write(
        &path,
        source.replace("tuple([Int, Text])", "tuple([Bool, Text])"),
    )
    .unwrap();
    assert!(!cached("check", &package, &cache, &[]).status.success());
}

#[test]
fn generated_definitions_persist_across_processes_without_reusing_old_output() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let generation = package.join("generated.loom");
    let generated = r#"
comptime {
    """
    fn generated(item Int) Int {
        item + 1
    }
    """
}
"#;
    fs::write(&generation, generated).unwrap();
    let path = package.join("main.loom");
    let source = r#"
fn answer() Int
ensures result == 8
{
    generated(7)
}
fn main() {
    assert answer() == 8
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(&path, format!("fn unrelated() Int {{ 1 }}\n{source}")).unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    assert!(
        trace.contains("definitions reused 3, bodies reused 3"),
        "{trace}"
    );
    let fresh = common::loom(&["emit-checked", package.to_str().unwrap()]);
    success(&fresh);
    assert_eq!(reused.stdout, fresh.stdout);
    checked(&cached("run", &package, &cache, &[]), true);
    // The generating block grows, but its first definition is unchanged. Reuse
    // that body with the current block extent, not the old source-text interval.
    let extended = generated.replace(
        "item + 1\n    }",
        "item + 1\n    }\n    fn extra_generated() Int { 1 }",
    );
    fs::write(&generation, &extended).unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    assert!(trace.contains(", bodies reused 3"), "{trace}");
    let fresh = common::loom(&["emit-checked", package.to_str().unwrap()]);
    success(&fresh);
    assert_eq!(reused.stdout, fresh.stdout);
    fs::write(&generation, generated.replace("item + 1", "item + 2")).unwrap();
    assert!(
        !cached("emit-checked", &package, &cache, &[])
            .status
            .success()
    );
}

#[test]
fn changed_sources_reuse_persisted_definitions_and_rekey_native_bodies() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let path = package.join("main.loom");
    let source = r#"
record Box[T] {
    value T
}
fn identity[T](value T) T {
    value
}
fn answer() Int
ensures result == 7
{
    identity(Box { value = 7 }).value
}
fn main() {
    assert answer() == 7
}
"#;
    fs::write(&path, source).unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(
        &path,
        format!("fn unrelated(value List[Bool]) List[Bool] {{ value }}\n{source}"),
    )
    .unwrap();
    let reused = cached("emit-checked", &package, &cache, &[]);
    checked(&reused, false);
    let trace = String::from_utf8_lossy(&reused.stderr);
    assert!(
        trace.contains("definitions reused 3, bodies reused 3"),
        "{trace}"
    );
    let fresh = common::loom(&["emit-checked", package.to_str().unwrap()]);
    success(&fresh);
    assert_eq!(reused.stdout, fresh.stdout);
    checked(&cached("run", &package, &cache, &[]), true);

    let snapshot = fs::read_dir(cache.join("definitions-v2"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "ldef")
        })
        .unwrap();
    let successful = fs::read(&snapshot).unwrap();
    fs::write(&path, source.replace("value = 7", "value = 8")).unwrap();
    let rejected = cached("emit-checked", &package, &cache, &[]);
    assert!(!rejected.status.success());
    assert_eq!(fs::read(&snapshot).unwrap(), successful);

    // A damaged definition bundle is a miss, not partially accepted evidence.
    fs::write(&snapshot, &successful[..successful.len() - 1]).unwrap();
    fs::write(&path, format!("fn added() Int {{ 9 }}\n{source}")).unwrap();
    let recovered = cached("emit-checked", &package, &cache, &[]);
    checked(&recovered, false);
    assert!(
        String::from_utf8_lossy(&recovered.stderr)
            .contains("definitions reused 0, bodies reused 0")
    );
    let fresh = common::loom(&["emit-checked", package.to_str().unwrap()]);
    success(&fresh);
    assert_eq!(recovered.stdout, fresh.stdout);
}

#[test]
fn persisted_definitions_revalidate_observed_build_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    let source = r#"
import std.build.input_file
fn stable() Int {
    7
}
fn main() {
    assert stable() == 7
    assert input_file("message.txt") == "yes"
}
"#;
    let path = package.join("main.loom");
    fs::write(&path, source).unwrap();
    fs::write(package.join("message.txt"), "yes").unwrap();
    checked(&cached("emit-checked", &package, &cache, &[]), false);
    fs::write(&path, format!("fn added() Int {{ 1 }}\n{source}")).unwrap();
    fs::write(package.join("message.txt"), "no!").unwrap();
    let changed = cached("emit-checked", &package, &cache, &[]);
    checked(&changed, false);
    assert!(
        String::from_utf8_lossy(&changed.stderr).contains("definitions reused 0, bodies reused 0")
    );
    let fresh = common::loom(&["emit-checked", package.to_str().unwrap()]);
    success(&fresh);
    assert_eq!(changed.stdout, fresh.stdout);
    assert!(!cached("run", &package, &cache, &[]).status.success());
}

#[test]
fn frontend_reuses_checked_artifacts_without_skipping_outputs_tests_or_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("source 雪");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("main.loom"), "fn answer() Int ensures result == 42 { 42 }\nfn main() { assert answer() == 42 }\ntest fn embedded() { assert answer() == 42 }").unwrap();
    fs::write(
        package.join("main_test.loom"),
        "test fn external() { assert answer() == 42 }",
    )
    .unwrap();
    for hit in [false, true] {
        checked(&cached("check", &package, &cache, &[]), hit);
    }
    let uncached = common::loom(&["emit-checked", package.to_str().unwrap()]);
    success(&uncached);
    for hit in [false, true] {
        let output = cached("emit-checked", &package, &cache, &[]);
        checked(&output, hit);
        assert_eq!(output.stdout, uncached.stdout);
    }
    let executable = common::executable(directory.path(), "app");
    let receipt = directory.path().join("build.receipt");
    checked(
        &cached(
            "build",
            &package,
            &cache,
            &[
                "--output",
                executable.to_str().unwrap(),
                "--receipt",
                receipt.to_str().unwrap(),
                "--object-cache",
                cache.to_str().unwrap(),
            ],
        ),
        true,
    );
    assert!(
        fs::read_to_string(&receipt)
            .unwrap()
            .contains("checked-sha256 ")
    );
    success(&std::process::Command::new(&executable).output().unwrap());
    fs::remove_file(&executable).unwrap();
    // Frontend and object hits still recreate the final artifact and receipt.
    let built = cached(
        "build",
        &package,
        &cache,
        &[
            "--output",
            executable.to_str().unwrap(),
            "--receipt",
            receipt.to_str().unwrap(),
            "--object-cache",
            cache.to_str().unwrap(),
        ],
    );
    checked(&built, true);
    assert!(String::from_utf8_lossy(&built.stderr).contains("loom cache: hit\n"));
    assert!(executable.is_file());
    let ir = directory.path().join("app.ll");
    checked(
        &cached(
            "build",
            &package,
            &cache,
            &["--emit-ir", ir.to_str().unwrap()],
        ),
        true,
    );
    assert!(fs::read_to_string(ir).unwrap().contains("@main("));
    checked(&cached("run", &package, &cache, &[]), true);
    for hit in [false, true] {
        let output = cached("test", &package, &cache, &[]);
        checked(&output, hit);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "2 tests passed\n"
        );
    }
    fs::write(
        package.join("main_test.loom"),
        "test fn external() { assert false }",
    )
    .unwrap();
    checked(&cached("build", &package, &cache, &[]), true);
    let failed = cached("test", &package, &cache, &[]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("frontend miss"));
}

#[test]
fn frontend_invalidates_real_source_membership_dependencies_and_proofs() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let dependency = directory.path().join("dep");
    let fork = directory.path().join("fork");
    let cache = directory.path().join("cache");
    for path in [&package, &dependency, &fork] {
        fs::create_dir(path).unwrap();
    }
    let manifest = |path: &str| {
        format!("[module]\nname = \"app\"\n[dependencies.dep]\npath = \"../{path}\"\n")
    };
    fs::write(package.join("loom.toml"), manifest("dep")).unwrap();
    for path in [&dependency, &fork] {
        fs::write(path.join("loom.toml"), "[module]\nname = \"dep\"\n").unwrap();
        fs::write(
            path.join("lib.loom"),
            "pub fn value() Int ensures result == 42 { 42 }",
        )
        .unwrap();
    }
    fs::write(
        package.join("main.loom"),
        "import dep.value\nfn main() { assert value() == 42 }",
    )
    .unwrap();
    checked(&cached("check", &package, &cache, &[]), false);
    checked(&cached("check", &package, &cache, &[]), true);
    // Same bytes at a different module instance are a different semantic basis.
    fs::write(package.join("loom.toml"), manifest("fork")).unwrap();
    checked(&cached("check", &package, &cache, &[]), false);
    fs::write(
        fork.join("lib.loom"),
        "pub fn value() Int ensures result == 42 { 41 }",
    )
    .unwrap();
    let failed = cached("check", &package, &cache, &[]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("frontend miss"));
    fs::write(
        fork.join("lib.loom"),
        "pub fn value() Int ensures result == 42 { 42 }",
    )
    .unwrap();
    checked(&cached("check", &package, &cache, &[]), true);
    let extra = package.join("extra.loom");
    fs::write(&extra, "fn invalid() Int { true }").unwrap();
    assert!(!cached("check", &package, &cache, &[]).status.success());
    fs::write(&extra, "fn extra() Int { 1 }").unwrap();
    checked(&cached("check", &package, &cache, &[]), false);
    fs::remove_file(extra).unwrap();
    checked(&cached("check", &package, &cache, &[]), true);
    // Loading cannot be bypassed even with a warm successful artifact.
    fs::write(fork.join("loom.toml"), "not a manifest").unwrap();
    let invalid = cached("check", &package, &cache, &[]);
    assert!(!invalid.status.success());
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("frontend hit"));
}

#[test]
fn frontend_rechecks_damaged_bundles_and_never_publishes_failed_checks() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("cache");
    fs::write(directory.path().join("main.loom"), "fn main() {}").unwrap();
    checked(
        &cached("emit-checked", directory.path(), &cache, &[]),
        false,
    );
    let entries: Vec<_> = fs::read_dir(cache.join("checked-v3"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let bundle = &entries[0];
    let original = fs::read(bundle).unwrap();
    let mut damaged = original.clone();
    let position = damaged.len() - 65;
    damaged[position] ^= 1;
    fs::write(bundle, damaged).unwrap();
    checked(
        &cached("emit-checked", directory.path(), &cache, &[]),
        false,
    );
    assert_eq!(fs::read(bundle).unwrap(), original);
    checked(&cached("emit-checked", directory.path(), &cache, &[]), true);
    fs::write(
        directory.path().join("main.loom"),
        "fn bad() Int ensures result == 1 { 2 }\nfn main() {}",
    )
    .unwrap();
    for _ in 0..2 {
        assert!(
            !cached("emit-checked", directory.path(), &cache, &[])
                .status
                .success()
        );
    }
    assert_eq!(fs::read_dir(cache.join("checked-v3")).unwrap().count(), 1);
    for extra in [
        vec!["--frontend-cache", ""],
        vec!["--frontend-cache", "other"],
    ] {
        let invalid = cached("check", directory.path(), &cache, &extra);
        assert!(!invalid.status.success());
        assert!(String::from_utf8_lossy(&invalid.stderr).contains("--frontend-cache"));
    }
    assert!(
        !cached("resolve", directory.path(), &cache, &[])
            .status
            .success()
    );
}
