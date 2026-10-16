use std::fs;

mod common;
use common::success;

#[test]
fn dependent_store_arguments_revalidate_when_workers_enter_a_cached_build() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    let cache = package.path().join("cache");
    let declarations = r#"
import std.list.length

type Pair = List[Int] where length(self) == 2 && self[0] <= self[1]

fn copy_second(values Pair) {
    values[0] = values[1]
}

fn copy_first(values Pair) {
    values[1] = values[0]
}
"#;
    for shared in [false, true, false] {
        let entry = if shared {
            r#"
import std.task.worker.run

async fn main() {
    let values = Pair([1, 2])
    let first = run(fn() {
            copy_second(values)
        })
    let second = run(fn() {
            copy_first(values)
        })
    discard (first, second).await
}
"#
        } else {
            r#"
fn main() {
    let values = Pair([1, 2])
    copy_second(values)
    copy_first(values)
    assert values[0] == 2 && values[1] == 2
}
"#
        };
        fs::write(&source, format!("{declarations}\n{entry}")).unwrap();
        let output = common::command(&["check", package.path().to_str().unwrap()])
            .arg("--frontend-cache")
            .arg(&cache)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), !shared, "{output:?}");
        if shared {
            let message = String::from_utf8_lossy(&output.stderr);
            assert!(message.contains("constrained sequence"), "{message}");
        } else {
            success(&common::loom(&["run", package.path().to_str().unwrap()]));
        }
    }
}
