use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn dynamic_values_keep_gc_snapshots_and_only_retain_called_witness_slots() {
    let temporary = tempfile::tempdir().unwrap();
    let example = common::root().join("compiler/examples/dynamic");
    let executable = common::executable(temporary.path(), "dynamic");
    success(
        &common::command(&[
            "build",
            example.to_str().unwrap(),
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
        "concept Read { type Item\nfn read(self Self) Self.Item\nfn unused(self Self) Int\nfn tag[T](self Self) Int { comptime if T == Int { 1 } else { 2 } }\nfn unused_generic[T](self Self) Int { 9004 } }\nrecord A {}\nrecord B {}\nimpl Read for A { type Item = Int\nfn read(self A) Int { 7 }\nfn unused(self A) Int { 9001 } }\nimpl Read for B { type Item = Bool\nfn read(self B) Bool { 9002 == 0 }\nfn unused(self B) Int { 9003 } }\npub fn library_receiver() dyn Read[Item = Bool] { B {} }\nfn main() { let value dyn Read[Item = Int] = A {}\nassert value.read() == 7\nassert value.tag[Int]() == 1 && value.tag[Bool]() == 2 }").unwrap();
    let executable = common::executable(temporary.path(), "reachability");
    let ir = temporary.path().join("reachability.ll");
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
    for absent in ["9001", "9002", "9003", "9004"] {
        assert!(!ir.contains(absent), "uncalled implementation reached LLVM");
    }
}

#[test]
fn erased_values_require_explicit_evidence_and_do_not_enable_type_discovery() {
    let source = tempfile::tempdir().unwrap();
    for text in [
        "concept C {}\nfn main() { let value dyn C = 1 }",
        "concept C {}\nconcept D {}\nimpl C for Int {}\nimpl D for Int {}\nfn convert(value dyn C) dyn D { value }",
        "concept C {}\nimpl C for Int {}\nfn recover(value dyn C) Int { value }",
        "concept C { fn copy(self Self) Self }\nfn erased(value dyn C) {}",
        "concept C { fn copy[T](self Self, value T) (Self, T) }\nfn erased(value dyn C) {}",
        "concept C {}\nimpl C for Int {}\nfn main() { discard comptime { let value dyn C = 1\nvalue } }",
        "concept C { type Item }\nimpl C for Int { type Item = Int }\nfn main() { let value dyn C[Item = Bool] = 1 }",
        "concept C { type Item }\nfn change(value dyn C[Item = Int]) dyn C[Item = Bool] { value }",
    ] {
        fs::write(source.path().join("main.loom"), text).unwrap();
        let output = loom(&["check", source.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{text}: {output:?}");
        assert!(!output.stderr.is_empty());
    }
}
