use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn named_callbacks_keep_native_layouts_and_exact_reference_reachability() {
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "callbacks");
    success(
        &common::command(&[
            "build",
            common::root()
                .join("compiler/examples/callbacks")
                .to_str()
                .unwrap(),
            "--output",
            executable.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(
        &Command::new(executable)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );
    fs::write(temporary.path().join("main.loom"),
        "fn increment(value Int) Int { value + 1 }\npub fn unused(value Int) Int { value + 9001 }\nfn apply(action fn(Int) Int, value Int) Int { action(value) }\nfn main() { assert apply(increment, 4) == 5 }").unwrap();
    let executable = common::executable(temporary.path(), "scalar-callback");
    let ir = temporary.path().join("callbacks.ll");
    success(
        &common::command(&[
            "build",
            temporary.path().to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(&Command::new(executable).output().unwrap());
    let ir = fs::read_to_string(ir).unwrap();
    assert!(!ir.contains("loom_rt_") && !ir.contains("9001"));
}

#[test]
fn callback_types_purity_and_entry_contracts_remain_checked() {
    let package = tempfile::tempdir().unwrap();
    for text in [
        "fn f(value Int) Int { value }\nfn main() { let action fn(Text) Int = f\ndiscard action }",
        "fn f(value Int) Int { value }\nfn f(value Text) Text { value }\nfn main() { let action = f\ndiscard action }",
        "import std.io.write_text\nfn impure(value Int) Int { discard write_text(\"effect\")\nvalue }\nfn call(action fn(Int) Int) Int { action(1) }\nfn main() { discard comptime { call(impure) } }",
        "import std.io.write_text\nfn impure(value Int) Int { discard write_text(\"effect\")\nvalue }\nfn pure(value Int) Int { value }\nfn main() { discard comptime { let action fn(Int) Int = if false { impure } else { pure }\naction(1) } }",
    ] {
        fs::write(package.path().join("main.loom"), text).unwrap();
        let output = loom(&["check", package.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{text}: {output:?}");
        assert!(!output.stderr.is_empty());
    }
    fs::write(package.path().join("main.loom"),
        "fn positive(value Int) Int requires value > 0 { value }\nfn call(action fn(Int) Int) Int { action(0) }\nfn main() { discard call(positive) }").unwrap();
    let output = loom(&["run", package.path().to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("precondition"));
}

#[test]
fn source_closures_preserve_shared_cells_cleanup_and_entry_contracts() {
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "closures");
    for example in ["closures", "comptime_closures"] {
        let package = common::root().join("compiler/examples").join(example);
        for level in ["0", "2"] {
            success(
                &common::command(&[
                    "test",
                    package.to_str().unwrap(),
                    "--no-run",
                    "--output",
                    executable.to_str().unwrap(),
                ])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
            );
            success(&common::run_tasks(&executable));
        }
    }
    fs::write(temporary.path().join("main.loom"),
        "fn make(limit Int) fn(Int) Int { fn(value Int) Int requires value > limit { value } }\nfn main() { let action = make(3)\ndiscard action(2) }").unwrap();
    let output = loom(&["run", temporary.path().to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("precondition"));
    fs::write(temporary.path().join("main.loom"),
        "fn make(limit Int) fn(Int) Int { fn(value Int) Int requires value > limit { value } }\nfn call(comptime action fn(Int) Int) Int { action(2) }\nfn main() { discard call(make(3)) }").unwrap();
    let output = loom(&["run", temporary.path().to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("precondition"));
}
