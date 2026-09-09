use std::fs;
mod common;
use common::success;

#[test]
fn nested_patterns_share_native_compile_time_and_task_semantics() {
    let example = common::root().join("compiler/examples/patterns");
    success(&common::loom(&["check", example.to_str().unwrap()]));
    success(&common::loom(&["test", example.to_str().unwrap()]));
    success(&common::loom(&["run", example.to_str().unwrap()]));
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "patterns");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                example.to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&common::run_tasks(&executable));
    }

    fs::write(
        directory.path().join("main.loom"),
        "enum Bit { On Off }\nfn choose(value (Bit, Bit)) Int { match value { (Bit.On, _) => 1\n(_, Bit.On) => 2\n(Bit.Off, Bit.Off) => 0 } }\n\
         fn classify(value Int) Int { match value { 0 => 1\n7 => 2\nrest => rest } }\n\
         record Pair { first Int second Bool }\nfn pick(value Pair) Int { match value { Pair { second = true, first = n } => n\nwhole => whole.first } }\n\
         fn main() { assert choose((Bit.Off(), Bit.On())) == 2\nassert classify(7) == 2\nassert classify(9) == 9\nassert pick(Pair { first = 3 second = true }) == 3\nassert pick(Pair { first = 4 second = false }) == 4 }",
    )
    .unwrap();
    let ir = directory.path().join("patterns.ll");
    success(
        &common::command(&[
            "build",
            directory.path().to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    let ir = fs::read_to_string(ir).unwrap();
    assert!(ir.contains("switch i64"));
    assert!(ir.contains("icmp eq i64"));
    assert!(!ir.contains("call ptr @loom_"));
    assert!(!ir.contains("@loom_task_"));
    success(&common::run_tasks(&executable));
}
