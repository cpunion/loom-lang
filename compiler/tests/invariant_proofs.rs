use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn invariant_effects_separate_fresh_scratch_from_shared_inputs() {
    let package = "compiler/examples/record_refinement";
    success(&common::loom(&["check", package]));
    let directory = tempfile::tempdir().unwrap();
    for level in ["0", "2"] {
        success(
            &common::command(&["test", package])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &Command::new(common::executable(
                &common::root().join(package).join("target"),
                "tests",
            ))
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
        );
        let artifact = common::executable(directory.path(), "invariant-effects");
        success(
            &common::command(&["build", package, "--output", artifact.to_str().unwrap()])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&Command::new(artifact).output().unwrap());
    }
}

#[test]
fn typed_inputs_prove_contracts_without_rechecking_construction() {
    let package = "compiler/examples/invariant_contracts";
    success(&common::loom(&["check", package]));
    success(&common::loom(&["test", package]));
    success(&common::loom(&["run", package]));
    let directory = tempfile::tempdir().unwrap();
    let artifact = common::executable(directory.path(), "invariant-contracts");
    let ir_path = directory.path().join("invariants.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package,
                "--output",
                artifact.to_str().unwrap(),
                "--emit-ir",
                ir_path.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(
            &Command::new(&artifact)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        if level == "0" {
            let ir = fs::read_to_string(&ir_path).unwrap();
            assert_eq!(
                ir.matches("icmp sgt i64").count(),
                4,
                "only the three source construction predicates remain"
            );
            assert_eq!(
                ir.matches("icmp sge i64").count(),
                2,
                "the source ordered predicate and ordinary List bounds check remain"
            );
            assert!(
                !ir.contains("icmp slt i64"),
                "the compile-time-only invariant helper must not become a native root"
            );
        }
    }
}

const UPDATE_TYPES: &str = r#"
import std.list.length
import std.list.push
import std.list.set

fn positive_elements(values List[Int]) Bool {
    var index = 0
    while index < length(values) {
        if values[index] <= 0 {
            return false
        }
        index = index + 1
    }
    true
}

type PositiveElement = Int where self > 0
type PositiveValues = List[Int] where length(self) > 0 && positive_elements(self)

fn replace_positive(values PositiveValues, index Int, value PositiveElement) {
    values[index] = value
}

fn forwarded(values List[Int], index Int, value Int) {
    set(values, index, value)
}

fn positive_scalar(value Int) Bool {
    value > 0
}

fn replace_checked(values PositiveValues, index Int, value Int)
requires positive_scalar(value)
{
    forwarded(values, index, value)
}

fn replace_twice(values List[Int], index Int, value Int) {
    let alias = values
    alias[index] = value
    set(alias, index, value)
}

fn fill_positive(values List[Int], value Int) {
    if value <= 0 {
        return
    }
    var index = 0
    while index < length(values) {
        replace_twice(values, index, value)
        index = index + 1
    }
}

fn append_positive(values List[Int], value Int) {
    if value <= 0 {
        return
    }
    let alias = values
    var index = 0
    while index < 2 {
        push(alias, value)
        index = index + 1
    }
}

fn append_guarded(values PositiveValues, value Int) {
    if value <= 0 {
        return
    }
    push(values, value)
}
"#;

#[test]
fn proved_element_updates_preserve_shared_constraints_and_revalidate_workers() {
    let package = tempfile::tempdir().unwrap();
    let path = package.path().to_str().unwrap();
    let main = package.path().join("main.loom");
    let cache = package.path().join("cache");
    let check = || common::loom(&["check", path, "--frontend-cache", cache.to_str().unwrap()]);
    let source = [
        UPDATE_TYPES,
        r#"
fn main() {
    let values = PositiveValues([1, 2])
    let alias = values
    replace_positive(values, 0, PositiveElement(3))
    forwarded(alias, 1, PositiveElement(4))
    values[0] = 5
    let replacement = values[0]
    assert replacement > 0
    let copied = replacement
    forwarded(alias, 0, copied)
    replace_checked(alias, 0, copied)
    (fn(items List[Int], index Int, value Int) {
        set(items, index, value)
    })(alias, 0, copied)
    let effects List[Int] = []
    forwarded(alias, {
        push(effects, 1)
        0
    }, copied)
    assert length(effects) == 1
    replace_twice(alias, 0, PositiveElement(5))
    fill_positive(alias, 4)
    fill_positive(values, -1)
    values[0] = 5
    assert alias[0] == 5 && values[1] == 4 && length(values) == 2
    push(values, PositiveElement(6))
    append_positive(alias, 7)
    append_positive(values, -1)
    assert length(alias) == 5 && values[2] == 6 && values[4] == 7
    append_guarded(alias, 8)
    append_guarded(values, -1)
    push(values, {
        push(effects, 2)
        PositiveElement(9)
    })
    assert length(effects) == 2 && length(alias) == 7 && values[5] == 8 && values[6] == 9
}

test fn preserved_updates() {
    main()
}
"#,
    ]
    .concat();
    fs::write(&main, &source).unwrap();
    success(&check());

    fs::write(
        &main,
        source.replace("alias[index] = value", "alias[index] = -1"),
    )
    .unwrap();
    let broken_store = check();
    assert!(!broken_store.status.success());
    assert!(String::from_utf8_lossy(&broken_store.stderr).contains("may invalidate"));
    fs::write(&main, &source).unwrap();
    success(&check());

    fs::write(&main, source.replace("value > 0", "value >= 0")).unwrap();
    let weakened_guard = check();
    assert!(!weakened_guard.status.success());
    assert!(String::from_utf8_lossy(&weakened_guard.stderr).contains("may invalidate"));
    fs::write(&main, &source).unwrap();
    success(&check());
    success(&check());
    fs::write(&main, source.replace("self > 0", "self >= 0")).unwrap();
    let weakened = check();
    assert!(!weakened.status.success());
    assert!(String::from_utf8_lossy(&weakened.stderr).contains("may invalidate"));
    fs::write(&main, &source).unwrap();
    success(&check());

    let artifact = common::executable(package.path(), "updates");
    for level in ["0", "2"] {
        for args in [
            vec!["test", path],
            vec!["build", path, "--output", artifact.to_str().unwrap()],
        ] {
            success(
                &common::command(&args)
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
        }
        success(
            &Command::new(&artifact)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
    success(&common::loom(&["run", path]));

    for rejected in [
        r#"
pub fn invalid(values PositiveValues, value Int) {
    values[0] = value
}
"#,
        r#"
pub fn invalid(values PositiveValues, input Int) {
    var value = input
    assert value > 0
    value = -1
    set(values, 0, value)
}
"#,
        r#"
fn unsafe_cleanup(values List[Int]) {
    defer {
        values[0] = -1
    }
    values[0] = 1
    assert false
}
pub fn invalid(values PositiveValues) {
    unsafe_cleanup(values)
}
"#,
        r#"
pub fn invalid(values PositiveValues) {
    set(values, 0, 0)
}
"#,
        r#"
pub fn invalid(values PositiveValues) {
    push(values, -1)
}
"#,
        r#"
fn bad_append(values List[Int]) {
    push(values, -1)
    assert false
}
pub fn invalid(values PositiveValues) {
    bad_append(values)
}
"#,
        r#"
fn temporary(values List[Int])
ensures positive_elements(values)
{
    values[0] = -1
    assert false
    values[0] = 1
}
pub fn invalid(values PositiveValues) {
    temporary(values)
}
"#,
        r#"
fn linked(values List[Int]) Bool {
    length(values) == 2 && values[0] == values[1] && values[0] > 0
}
type Linked = List[Int] where linked(self)
pub fn invalid(values Linked) {
    set(values, 0, PositiveElement(2))
}
"#,
    ] {
        fs::write(&main, [UPDATE_TYPES, rejected].concat()).unwrap();
        let result = check();
        assert!(!result.status.success(), "accepted: {rejected}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("may invalidate"));
    }

    // Bounds faults occur before the store; lexical cleanup sees the old value.
    fs::write(
        &main,
        [
            UPDATE_TYPES,
            r#"
import std.io.write_text
fn retained(values PositiveValues) {
    assert values[0] == 1 && values[1] == 2
    discard write_text("preserved\n")
}
fn main() {
    let values = PositiveValues([1, 2])
    defer {
        retained(values)
    }
    replace_twice(values, length(values), PositiveElement(3))
}
"#,
        ]
        .concat(),
    )
    .unwrap();
    success(&common::loom(&[
        "build",
        path,
        "--output",
        artifact.to_str().unwrap(),
    ]));
    let fault = Command::new(&artifact)
        .env("LOOM_GC_STRESS", "1")
        .output()
        .unwrap();
    assert!(!fault.status.success());
    assert_eq!(fault.stdout, b"preserved\n");
    assert!(String::from_utf8_lossy(&fault.stderr).contains("list index out of bounds"));

    let concurrent = [
        UPDATE_TYPES,
        r#"
import std.task.worker.run
async fn main() {
    let values = PositiveValues([1, 2])
    let writer = run(fn() Int {
        fill_positive(values, 3)
        append_positive(values, 5)
        0
    })
    discard run(fn() Int {
        replace_twice(values, 1, PositiveElement(4))
        push(values, PositiveElement(6))
        0
    }).await
    discard writer.await
    assert values[0] > 0 && values[1] > 0 && length(values) == 5
}
"#,
    ]
    .concat();
    fs::write(&main, concurrent).unwrap();
    success(&check());
    success(&common::loom(&[
        "build",
        path,
        "--output",
        artifact.to_str().unwrap(),
    ]));
    success(
        &Command::new(&artifact)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );

    // A sequentially preserving copy can break an ordering invariant if a
    // worker changes the right-hand cell between the read and the store.
    let sequential = r#"
import std.list.length
import std.task.worker.run
type Ordered = List[Int] where length(self) == 2 && self[0] >= 0 && self[0] <= self[1]
fn copy_right_to_left(values List[Int]) {
    let right = values[1]
    values[0] = right
}
fn main() {
    let values = Ordered([1, 2])
    copy_right_to_left(values)
    assert values[0] == 2
}
"#;
    fs::write(&main, sequential).unwrap();
    success(&check());
    fs::write(
        &main,
        sequential.replace("fn main()", "async fn main()").replace(
            "    copy_right_to_left(values)",
            "    discard run(fn() Int {\n        copy_right_to_left(values)\n        0\n    }).await",
        ),
    )
    .unwrap();
    let stale_copy = check();
    assert!(!stale_copy.status.success());
    assert!(String::from_utf8_lossy(&stale_copy.stderr).contains("interference-safe"));

    let observer = r#"
import std.task.worker.run
fn unchanged(values PositiveValues) Int
ensures result == old(values[0])
{
    values[0]
}
async fn main() {
    let values = PositiveValues([1, 2])
    discard run(fn() Int {
        unchanged(values)
    }).await
}
"#;
    let readonly = [UPDATE_TYPES, observer].concat();
    fs::write(&main, &readonly).unwrap();
    success(&check());
    let writable = readonly
        .replace(
            "    discard run(fn() Int {",
            r#"    let writer = run(fn() Int {
        fill_positive(values, 3)
        0
    })
    discard run(fn() Int {"#,
        )
        .replace(
            "    }).await\n}",
            "    }).await\n    discard writer.await\n}",
        );
    fs::write(&main, writable).unwrap();
    let changed = check();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("interference-safe"));
    fs::write(&main, readonly).unwrap();
    success(&check());

    // A proved append withdraws fixed-extent evidence from every alias, not
    // just read-only element evidence. Cache hits must observe changed bodies.
    let fixed = r#"
import std.task.worker.run
fn sampled_size(values PositiveValues) Int
ensures result == old(length(values))
{
    length(values)
}
async fn main() {
    let values = PositiveValues([1, 2])
    discard run(fn() Int {
        sampled_size(values)
    }).await
}
"#;
    let unchanged = [UPDATE_TYPES, fixed].concat();
    fs::write(&main, &unchanged).unwrap();
    success(&check());
    let growing = unchanged.replace(
        "        sampled_size(values)",
        "        append_positive(values, 3)\n        sampled_size(values)",
    );
    fs::write(&main, growing).unwrap();
    let changed = check();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("interference-safe"));
    fs::write(&main, unchanged).unwrap();
    success(&check());

    // A sampled guard permits growth sequentially, but cannot reserve room
    // across independently guarded accesses by other workers.
    let guarded_growth = r#"
import std.list.length
import std.list.push
import std.task.worker.run
type HeadLimit = List[Int] where length(self) > 0 && length(self) <= self[0]
fn append_if_room(values List[Int]) {
    if length(values) < values[0] {
        push(values, 0)
    }
}
fn main() {
    let values = HeadLimit([3, 0])
    append_if_room(values)
    assert length(values) == 3
}
"#;
    fs::write(&main, guarded_growth).unwrap();
    success(&check());
    fs::write(
        &main,
        guarded_growth.replace("fn main()", "async fn main()").replace(
            "    append_if_room(values)",
            "    discard run(fn() Int {\n        append_if_room(values)\n        0\n    }).await",
        ),
    )
    .unwrap();
    let stale_guard = check();
    assert!(!stale_guard.status.success());
    assert!(String::from_utf8_lossy(&stale_guard.stderr).contains("interference-safe"));
}
