use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn compile_time_dynamic_calls_reify_shared_receivers_without_retaining_evaluation_targets() {
    let temporary = tempfile::tempdir().unwrap();
    let example = common::root().join("compiler/examples/comptime_dynamic");
    let executable = common::executable(temporary.path(), "comptime-dynamic");
    let ir = temporary.path().join("comptime-dynamic.ll");
    success(&common::loom(&["check", example.to_str().unwrap()]));
    success(&common::loom(&["test", example.to_str().unwrap()]));
    success(&common::loom(&["run", example.to_str().unwrap()]));
    fs::write(
        temporary.path().join("main.loom"),
        r#"import std.io.write_text
concept Read {
    fn read(self Self) Int
    fn unused(self Self) Int
}
impl Read for Int {
    fn read(self Int) Int { self }
    fn unused(self Int) Int {
        discard write_text("COMPTIME-UNUSED-IO")
        91827364
    }
}
fn computed() Int {
    comptime {
        let value dyn Read = 7
        value.read()
    }
}
fn main() { assert computed() == 7 }
"#,
    )
    .unwrap();
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
        success(
            &Command::new(&executable)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        success(
            &common::command(&[
                "build",
                temporary.path().to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&Command::new(&executable).output().unwrap());
        let text = fs::read_to_string(&ir).unwrap();
        for absent in ["@loom.witness.", "COMPTIME-UNUSED-IO", "91827364"] {
            assert!(
                !text.contains(absent),
                "compile-time target leaked: {absent}"
            );
        }
    }
    // An already boxed result carries evidence across the factory's test scope;
    // the production template does not gain permission to box Item itself.
    fs::write(
        temporary.path().join("main.loom"),
        "concept C { fn value(self Self) Int }\nrecord Item { number Int }\n\
         fn precompute(comptime factory fn() dyn C) dyn C { comptime { factory() } }\n\
         fn production() Bool { comptime if Item implements C { true } else { false } }",
    )
    .unwrap();
    fs::write(
        temporary.path().join("main_test.loom"),
        "impl C for Item { fn value(self Item) Int { self.number } }\n\
         fn factory() dyn C { Item { number = 7 } }\n\
         test fn transferred() { let value = precompute(factory)\n\
             assert value.value() == 7 && !production() }",
    )
    .unwrap();
    success(&common::loom(&["test", temporary.path().to_str().unwrap()]));
}
