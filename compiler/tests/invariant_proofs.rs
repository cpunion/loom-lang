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
    assert alias[0] == 5 && values[1] == 4 && length(values) == 2
}

test fn preserved_updates() {
    main()
}
"#,
    ]
    .concat();
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
pub fn invalid(values PositiveValues) {
    set(values, 0, 0)
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
    replace_positive(values, length(values), PositiveElement(3))
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
        replace_positive(values, 0, PositiveElement(3))
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
}
