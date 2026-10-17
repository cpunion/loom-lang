use std::fs;

mod common;
use common::success;

#[test]
fn append_frames_prove_entry_segments_without_hiding_destructive_or_shared_writes() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    let program = include_str!("../examples/list_append/main.loom");
    let columns_path = package.path().join("columns.loom");
    let columns = include_str!("../examples/list_append/columns.loom");
    let histograms_path = package.path().join("histograms.loom");
    let histograms = include_str!("../examples/list_append/histograms.loom");
    let cache = package.path().join("cache");
    let check = || {
        common::command(&["check", package.path().to_str().unwrap()])
            .arg("--frontend-cache")
            .arg(&cache)
            .output()
            .unwrap()
    };
    fs::write(&source, program).unwrap();
    fs::write(&columns_path, columns).unwrap();
    fs::write(&histograms_path, histograms).unwrap();
    success(&check());
    success(&check());
    fs::write(
        &columns_path,
        columns.replace(
            "requires get(source, index).ratio == get(source, index).ratio\n",
            "",
        ),
    )
    .unwrap();
    let output = check();
    assert!(
        !output.status.success(),
        "representation framing made NaN reflexive"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not proved"),
        "{output:?}"
    );
    fs::write(&columns_path, columns).unwrap();
    for changed in [
        histograms.replace(
            "    push(target, 7)",
            "    if length(source) > 0 {\n        source[0] = 0\n    }\n    push(target, 7)",
        ),
        histograms.replace(
            "length(source) != old(length(source)) || permutation(source, old(source))",
            "occurrences(source, 7) == occurrences(old(source), 7)",
        ),
    ] {
        fs::write(&histograms_path, changed).unwrap();
        let output = check();
        assert!(
            !output.status.success(),
            "an invalid whole histogram was preserved"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("not proved")
                || String::from_utf8_lossy(&output.stderr)
                    .contains("required proof was not established"),
            "{output:?}"
        );
    }
    fs::write(&histograms_path, histograms).unwrap();
    for changed in [
        program.replace("push(target, get(source, index))", "push(target, 0)"),
        program.replace(
            "        index = index + 1\n    }\n}",
            "        source[0] = 0\n        index = index + 1\n    }\n}",
        ),
        format!(
            "{}\n{}",
            program.replace(
                "push(target, get(source, index))",
                "push(target, corrupt(source, index))"
            ),
            r#"
fn corrupt(source List[Int], index Int) Int
requires index >= 0 && index < length(source)
{
    source[index] = 0
    get(source, index)
}
"#
        ),
    ] {
        assert_ne!(changed, program);
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(!output.status.success(), "invalid append proof: {changed}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved") || error.contains("contract"),
            "unexpected failure: {error}"
        );
    }
    let declarations = program.replace("fn main()", "fn exercise()");
    fs::write(
        &source,
        format!(
            "import std.task.worker.run\n{declarations}\n{}",
            r#"
async fn main() {
    let input = [1, 2, 3]
    let changing = run(fn() Int {
        input[0] = 0
        1
    })
    append([], input)
    discard changing.await
}
"#
        ),
    )
    .unwrap();
    let output = check();
    assert!(
        !output.status.success(),
        "shared append reused private observations"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("interference-safe"),
        "{output:?}"
    );
    fs::write(&source, program).unwrap();
    success(&check());
    let executable = common::executable(package.path(), "append-frames");
    let ir = package.path().join("append-frames.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package.path().to_str().unwrap()])
                .arg("--output")
                .arg(&executable)
                .arg("--emit-ir")
                .arg(&ir)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&executable));
        assert!(!fs::read_to_string(&ir).unwrap().contains("proof_"));
    }
}
