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
fn bundle[Ts...](items Ts...) (Ts...) {
    items
}
fn mapped[Ts...](items (Ts...)) (Ts...) {
    comptime map item in items {
        item
    }
}
fn count[Ts...](items Ts...) Int {
    var total = 0
    comptime for item in items {
        discard item
        total = total + 1
    }
    total
}
fn main() {
    let pair = mapped(bundle(3, true))
    let triple = bundle(1, "two", false)
    assert pair.1 && !triple.2
    assert pair.0 + count(triple...) + count(1) == 7
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
    assert!(trace.contains(", bodies reused 6"), "{trace}");
    // Fresh arity validation may intern abstract placeholders before the cached
    // path recreates concrete types. Those private IDs need not be byte-equal;
    // both native paths must execute every remapped call/layout correctly.
    let fresh = common::loom(&["run", package.to_str().unwrap()]);
    success(&fresh);
    let executed = cached("run", &package, &cache, &[]);
    checked(&executed, true);
    assert_eq!(executed.stdout, fresh.stdout);
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

    let snapshot = fs::read_dir(cache.join("definitions-v1"))
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
    let entries: Vec<_> = fs::read_dir(cache.join("checked-v2"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let bundle = &entries[0];
    let original = fs::read(bundle).unwrap();
    let mut damaged = original.clone();
    let position = damaged.len() - 33;
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
    assert_eq!(fs::read_dir(cache.join("checked-v2")).unwrap().count(), 1);
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
