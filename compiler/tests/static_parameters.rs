use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn static_parameters_specialize_before_native_abi_and_reachability() {
    let package = tempfile::tempdir().unwrap();
    let executable = common::executable(package.path(), "comptime-parameters");
    success(
        &common::command(&[
            "build",
            common::root()
                .join("compiler/examples/comptime_parameters")
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

    fs::write(
        package.path().join("main.loom"),
        r#"pub fn unused(value Int) Int { value + 9001 }
fn adjust(value Int, comptime count Int, comptime enabled Bool, comptime note Text) Int {
    comptime if enabled { value + count } else { unused(value) }
}
fn main() {
    assert adjust(3, 4, true, "first") == 7
    assert adjust(3, 5, true, "second") == 8
}"#,
    )
    .unwrap();
    let executable = common::executable(package.path(), "scalar-static-parameters");
    let ir = package.path().join("parameters.ll");
    success(
        &common::command(&[
            "build",
            package.path().to_str().unwrap(),
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
    let functions: Vec<_> = ir
        .lines()
        .filter(|line| line.starts_with("define ") && line.contains("@loom.fn."))
        .collect();
    assert_eq!(functions.len(), 3, "{functions:?}");
    for function in functions {
        let parameters = function
            .split_once('(')
            .unwrap()
            .1
            .split(')')
            .next()
            .unwrap();
        assert!(
            parameters.is_empty() || (parameters.starts_with("i64 ") && !parameters.contains(',')),
            "static arguments leaked into the ABI: {function}"
        );
    }
    assert!(!ir.contains("9001") && !ir.contains("loom_rt_"));
}

#[test]
fn static_arguments_reject_captures_effects_and_undeclared_generic_requirements() {
    let package = tempfile::tempdir().unwrap();
    let declaration = "fn f(value Int, comptime count Int) Int { value + count }\n";
    let cases = [
        format!(
            "{declaration}fn runtime(count Int) Int {{ f(1, count) }}\nfn main() {{ discard runtime(2) }}"
        ),
        format!("{declaration}fn main() {{ let count = 2\ndiscard f(1, count) }}"),
        format!(
            "{declaration}import std.io.write_text\nfn effect() Int {{ discard write_text(\"unexpected-static-io\")\n2 }}\nfn main() {{ discard f(1, effect()) }}"
        ),
        format!("{declaration}fn main() {{ discard f(1, 1 / 0) }}"),
        format!("{declaration}fn main() {{ let callback = f\ndiscard callback }}"),
        "fn unsupported(comptime value Float) Float { value }\nfn main() {}".into(),
        "fn f(value Int) Int { value }\nfn f(comptime value Int) Int { value }\nfn main() {}"
            .into(),
        "fn unproved(comptime count Int) Int ensures result == 1 { 0 }\nfn main() {}".into(),
        r#"concept Display { fn display(self Self) Text }
impl Display for Int { fn display(self Int) Text { "int" } }
fn render[T](value T, comptime enabled Bool) Text {
    comptime if enabled { value.display() } else { "<value>" }
}
fn main() { discard render(3, true) }"#
            .into(),
    ];
    for text in cases {
        fs::write(package.path().join("main.loom"), &text).unwrap();
        let output = loom(&["check", package.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "accepted {text}: {output:?}");
        assert!(!output.stderr.is_empty());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("unexpected-static-io"));
    }
}

#[test]
fn static_function_arguments_are_direct_and_preserve_preconditions_and_effects() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    fs::write(
        &source,
        r#"
fn first(value Int) Int requires value > 0 { value + 1 }
fn second(value Int) Int { value + 2 }
fn choose() fn(Int) Int { second }
fn apply(value Int, comptime action fn(Int) Int) Int { action(value) }
fn main() { assert apply(1, first) == 2
    assert apply(1, choose()) == 3
    assert apply(2, second) == 4 }
"#,
    )
    .unwrap();
    let artifact = common::executable(package.path(), "static-functions");
    let ir = package.path().join("static-functions.ll");
    success(
        &common::command(&[
            "build",
            package.path().to_str().unwrap(),
            "--output",
            artifact.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(&Command::new(&artifact).output().unwrap());
    let ir = fs::read_to_string(ir).unwrap();
    let functions: Vec<_> = ir
        .lines()
        .filter(|line| line.starts_with("define ") && line.contains("@loom.fn."))
        .collect();
    assert_eq!(functions.len(), 5, "{functions:?}");
    assert!(
        functions.iter().all(|line| !line.contains("ptr ")),
        "callback ABI: {functions:?}"
    );
    assert!(
        !ir.lines().any(|line| line.contains("call i64 %")),
        "indirect callback: {ir}"
    );

    for invocation in ["apply(0, first)", "comptime { apply(0, first) }"] {
        fs::write(&source, format!("fn first(value Int) Int requires value > 0 {{ value }}\nfn apply(value Int, comptime action fn(Int) Int) Int {{ action(value) }}\nfn main() {{ discard {invocation} }}")).unwrap();
        let command = if invocation.starts_with("comptime") {
            "check"
        } else {
            "run"
        };
        let output = loom(&[command, package.path().to_str().unwrap()]);
        assert!(!output.status.success(), "ignored precondition: {output:?}");
        assert!(!output.stderr.is_empty());
    }
    let declarations = "import std.io.write_text\nfn effect(value Int) Int { discard write_text(\"runtime-callback\")\nvalue }\nfn apply(value Int, comptime action fn(Int) Int) Int { action(value) }\n";
    fs::write(
        &source,
        format!("{declarations}fn main() {{ assert apply(1, effect) == 1 }}"),
    )
    .unwrap();
    let checked = loom(&["check", package.path().to_str().unwrap()]);
    success(&checked);
    assert!(!String::from_utf8_lossy(&checked.stdout).contains("runtime-callback"));
    let ran = loom(&["run", package.path().to_str().unwrap()]);
    success(&ran);
    assert!(String::from_utf8_lossy(&ran.stdout).contains("runtime-callback"));
    fs::write(
        &source,
        format!("{declarations}fn main() {{ discard comptime {{ apply(1, effect) }} }}"),
    )
    .unwrap();
    let rejected = loom(&["check", package.path().to_str().unwrap()]);
    assert!(!rejected.status.success());
    assert!(!String::from_utf8_lossy(&rejected.stdout).contains("runtime-callback"));
}
