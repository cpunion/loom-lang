use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn associated_results_and_bounded_data_use_native_value_layouts() {
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "associated");
    success(
        &common::command(&[
            "build",
            common::root()
                .join("compiler/examples/associated")
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
        "concept Source { type Item\ntype Wrapped[T]\nfn item(self Self) Self.Item }\nrecord Used { n Int }\nrecord Unused {}\nimpl Source for Used { type Item = Int\ntype Wrapped[T] = T\nfn item(self Used) Int { self.n } }\nimpl Source for Unused { type Item = Text\ntype Wrapped[T] = List[T]\nfn item(self Unused) Text { \"uncalled-associated-method\" } }\nfn forward[S Source](value S) S.Item { value.item() }\nfn family[S Source, T](source S, value S.Wrapped[T]) S.Wrapped[T] { value }\nfn main() { let used = Used { n = 42 }\nassert family[Used, Int](used, forward(used)) == 42 }").unwrap();
    let ir = temporary.path().join("associated.ll");
    let executable = common::executable(temporary.path(), "scalar");
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
    assert!(!ir.contains("loom_rt_") && !ir.contains("uncalled-associated-method"));
}

#[test]
fn missing_evidence_and_undetermined_associated_types_reject_in_source() {
    let temporary = tempfile::tempdir().unwrap();
    for text in [
        "concept C {}\nrecord Box[T C] { value T }\nfn hidden[T](value Box[T]) {}",
        "concept C { type Item }\nrecord A {}\nimpl C for A {}",
        "concept C { type Item }\nfn erased(value dyn C) {}",
        "concept C { type Item }\nconcept D { type Item }\nfn ambiguous[T C + D](value T) T.Item { 1 }",
        "concept C { type Item }\nrecord A {}\nimpl C for A { type Item = A.Item }",
    ] {
        fs::write(temporary.path().join("main.loom"), text).unwrap();
        let output = loom(&["check", temporary.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{text}: {output:?}");
        assert!(!output.stderr.is_empty());
    }
}
