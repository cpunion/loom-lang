mod common;
use common::success;

#[test]
fn source_workers_share_typed_data_and_drain_under_moving_gc() {
    let temporary = tempfile::tempdir().unwrap();
    let cache = temporary.path().join("frontend-cache");
    let example = common::executable(temporary.path(), "workers");
    let ir = temporary.path().join("workers.ll");
    let tests = common::executable(temporary.path(), "worker-tests");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "test",
                "compiler/std/task/worker",
                "--frontend-cache",
                cache.to_str().unwrap(),
            ])
            .args(["--no-run", "--output"])
            .arg(&tests)
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&std::process::Command::new(&tests).output().unwrap());
        success(&common::run_tasks(&tests));
        success(
            &common::command(&[
                "build",
                "compiler/examples/workers",
                "--output",
                example.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&common::run_tasks(&example));
        if level == "0" {
            let text = std::fs::read_to_string(&ir).unwrap();
            let bodies: Vec<_> = text
                .split("\ndefine ")
                .filter(|body| {
                    body.contains("call ptr @loom_rt_bytes_new")
                        && body.contains("call ptr @loom_rt_list_new")
                })
                .collect();
            assert_eq!(bodies.len(), 1, "missing private buffer function");
            assert!(!bodies[0].contains("call void @loom_rt_shared_access"));
            assert!(bodies[0].contains("@loom_rt_worker_checkpoint"));
            assert!(text.contains("call void @loom_rt_shared_access_begin"));
        }
    }
}

#[test]
fn workers_reject_resource_transfer_and_stale_shared_proofs() {
    let package = tempfile::tempdir().unwrap();
    for body in [
        "async fn main() { let mutex = new()\nscoped held = lock(mutex)\ndiscard run(fn() Int { discard held\n1 }).await }",
        "async fn main() { let mutex = new()\nscoped result = run(fn() Guard { lock(mutex) }).await }",
        "fn unchanged(values List[Int]) Int ensures result == std.list.length(values) { std.list.length(values) }\nasync fn main() { discard run(fn() Int { unchanged([1]) }).await }",
        "fn unchanged(values List[Int]) List[Int] requires length(values) == 1 ensures length(result) == 1 { values }\nasync fn main() { discard run(fn() List[Int] { unchanged([1]) }).await }",
        "fn unchanged(values List[Int]) Int ensures length(values) == length(values) { 0 }\nasync fn main() { discard run(fn() Int { unchanged([1]) }).await }",
        "fn equal(a Int, b Int) Bool { a == b }\nfn unchanged(values List[Int]) Int ensures equal(length(values), length(values)) { 0 }\nasync fn main() { discard run(fn() Int { unchanged([1]) }).await }",
        "fn count(values List[Int]) Int { length(values) }\nfn unchanged(values List[Int]) Int ensures count(values) == count(values) { 0 }\nasync fn main() { discard run(fn() Int { unchanged([1]) }).await }",
        "fn ignore(value Int) Bool { true }\nfn unchanged(values List[Int]) Int ensures ignore(get(values, 0)) { 0 }\nasync fn main() { discard run(fn() Int { unchanged([1]) }).await }",
        "fn same(value Int) Bool { value == value }\nfn unchanged(values List[Int]) Int requires length(values) < 9223372036854775807 ensures same(length(values) + 1) { 0 }\nasync fn main() { discard run(fn() Int { unchanged([1]) }).await }",
        "fn unchanged(values List[Int]) Int ensures result == old(length(values)) { length(values) }\nasync fn main() { discard run(fn() Int { unchanged([1]) }).await }",
        "fn unchanged(values List[Int]) Int requires length(values) >= 1 ensures length(values) == 0 || get(values, 0) == result { get(values, 0) }\nasync fn main() { discard run(fn() Int { unchanged([1]) }).await }",
        "fn unchanged(values List[Int]) Int requires length(values) < 9223372036854775807 ensures length(values) + 1 >= 1 { 0 }\nasync fn main() { discard run(fn() Int { unchanged([1]) }).await }",
        "fn local() Int ensures result == 1 { let original = [1]\nlet nested = [original]\nstd.list.push(std.list.get(nested, 0), 2)\nlength(original) }\nasync fn main() { discard run(local).await }",
        r#"
type Nonempty = List[Int] where length(self) > 0

fn unchanged(values Nonempty) Int
ensures result == old(length(values))
{
    length(values)
}

async fn main() {
    let values = Nonempty([1])
    discard run(fn() Int {
            unchanged(values)
        }).await
}
"#,
        r#"
type Pair = List[Int] where length(self) == 1

fn unchanged(values Pair) Int
ensures result == old(values[0])
{
    values[0]
}

async fn main() {
    let values = Pair([1])
    discard run(fn() Int {
            unchanged(values)
        }).await
}
"#,
    ] {
        std::fs::write(package.path().join("main.loom"), format!(
            "import std.task.worker.run\nimport std.sync.mutex.new\nimport std.sync.mutex.lock\nimport std.sync.mutex.Guard\nimport std.list.length\nimport std.list.push\nimport std.list.get\n{body}\n"
        )).unwrap();
        let output = common::loom(&["check", package.path().to_str().unwrap()]);
        assert!(!output.status.success(), "accepted: {body}");
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(
            message.contains("scoped")
                || message.contains("NoSuspend")
                || message.contains("MustScope")
                || message.contains("postcondition")
                || message.contains("interference-safe"),
            "{message}"
        );
    }
}

#[test]
fn edited_worker_builds_revalidate_cached_observation_identities() {
    for (predicate, accepted) in [
        ("same(length(values)) && same(old(length(values)))", true),
        ("length(values) == length(values)", false),
    ] {
        let package = tempfile::tempdir().unwrap();
        let cache = package.path().join("cache");
        std::fs::write(
            package.path().join("observations.loom"),
            format!(
                r#"
import std.list.length

fn same(value Int) Bool {{
    value == value
}}

fn observed(values List[Int]) Int
ensures {predicate}
{{
    length(values)
}}
"#
            ),
        )
        .unwrap();
        let main = package.path().join("main.loom");
        std::fs::write(&main, "fn main() { discard observed([1]) }\n").unwrap();
        let check = || {
            common::command(&[
                "check",
                package.path().to_str().unwrap(),
                "--frontend-cache",
                cache.to_str().unwrap(),
            ])
            .output()
            .unwrap()
        };
        success(&check());
        std::fs::write(
            main,
            r#"
import std.task.worker.run

async fn main() {
    discard run(fn() Int {
            observed([1])
        }).await
}
"#,
        )
        .unwrap();
        let output = check();
        assert_eq!(output.status.success(), accepted, "{output:?}");
        if !accepted {
            assert!(String::from_utf8_lossy(&output.stderr).contains("interference-safe"));
        }
    }
    let package = tempfile::tempdir().unwrap();
    let cache = package.path().join("cache");
    for (predicate, accepted) in [
        ("length(values) > 0 && values[0] > 0", true),
        ("length(values) > 0", false),
    ] {
        std::fs::write(
            package.path().join("main.loom"),
            format!(
                r#"
import std.list.length
import std.task.worker.run

fn valid(values List[Int]) Bool {{
    {predicate}
}}

type Protected = List[Int] where valid(self)

fn first(values Protected) Int
ensures result == old(values[0])
{{
    values[0]
}}

async fn main() {{
    let values = Protected([1])
    assert run(fn() Int {{
            first(values)
        }}).await == 1
}}
"#
            ),
        )
        .unwrap();
        let output = common::command(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
        .output()
        .unwrap();
        assert_eq!(output.status.success(), accepted, "{output:?}");
        if !accepted {
            assert!(String::from_utf8_lossy(&output.stderr).contains("interference-safe"));
        }
    }
}

#[test]
fn shared_factory_proofs_reject_aliases_and_publication() {
    for (params, factory, argument) in [
        ("values List[Int]", "values", "[1]"),
        (
            "values List[Int]",
            "if length(values) > 0 { return values }\nclone(values)",
            "[1]",
        ),
        (
            "values List[Int]",
            "var copied = clone(values)\ncopied = values\ncopied",
            "[1]",
        ),
        (
            "values List[List[Int]]",
            "get([get(values, 0)], 0)",
            "[[1]]",
        ),
        ("values List[List[Int]]", "get(clone(values), 0)", "[[1]]"),
        (
            "values List[List[Int]]",
            "let copied = [1]\npush(get([values], 0), copied)\ncopied",
            "[[1]]",
        ),
        (
            "values List[List[Int]]",
            "let copied = [1]\npush(values, copied)\ncopied",
            "[[1]]",
        ),
        (
            "values List[List[Int]]",
            "let copied = [1]\ndiscard publish(values, copied)\ncopied",
            "[[1]]",
        ),
    ] {
        let package = tempfile::tempdir().unwrap();
        std::fs::write(
            package.path().join("factory.loom"),
            format!(
                r#"
import std.list.clone
import std.list.length
import std.list.push
import std.list.get

fn publish(target List[List[Int]], value List[Int]) Int
ensures result == 0
{{
    push(target, value)
    0
}}

fn copied({params}) List[Int]
ensures length(result) >= 0
{{
    {factory}
}}

fn extended({params}) Int
ensures result >= 1
{{
    let output = copied(values)
    push(output, 7)
    length(output)
}}

"#
            ),
        )
        .unwrap();
        let main = package.path().join("main.loom");
        let cache = package.path().join("cache");
        let check = || {
            common::command(&[
                "check",
                package.path().to_str().unwrap(),
                "--frontend-cache",
                cache.to_str().unwrap(),
            ])
            .output()
            .unwrap()
        };
        std::fs::write(
            &main,
            format!("fn main() {{ discard extended({argument}) }}\n"),
        )
        .unwrap();
        success(&check());
        std::fs::write(
            main,
            format!(
                r#"
import std.task.worker.run

async fn main() {{
    let values = {argument}
    discard run(fn() Int {{
            extended(values)
        }}).await
}}
"#
            ),
        )
        .unwrap();
        let output = check();
        assert!(!output.status.success(), "accepted factory: {factory}");
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(
            message.contains("interference-safe") || message.contains("required postcondition"),
            "factory {factory}: {message}"
        );
    }
}

#[test]
fn ordinary_list_programs_keep_direct_lowering_without_worker_runtime() {
    let temporary = tempfile::tempdir().unwrap();
    let binary = common::executable(temporary.path(), "local");
    let ir = temporary.path().join("local.ll");
    success(&common::loom(&[
        "build",
        "compiler/examples/list_contracts",
        "--output",
        binary.to_str().unwrap(),
        "--emit-ir",
        ir.to_str().unwrap(),
    ]));
    let ir = std::fs::read_to_string(ir).unwrap();
    assert!(!ir.contains("loom_rt_worker_checkpoint"));
    assert!(!ir.contains("loom_rt_shared_access"));
    assert!(!ir.contains("loom_rt_task_run_shared"));
    success(&common::run_tasks(&binary));
}

#[test]
fn direct_helper_summaries_keep_private_calls_fast_and_shared_calls_guarded() {
    let package = tempfile::tempdir().unwrap();
    std::fs::write(
        package.path().join("main.loom"),
        r#"
import std.list.get
import std.list.set
import std.task.worker.run

fn create(value Int) List[Int] {
    [value]
}

fn alias(values List[Int]) List[Int] {
    values
}

fn private_step(values List[Int], count Int) {
    if count > 0 {
        set(values, 0, get(values, 0) + 1)
        private_step(values, count - 1)
    }
}

fn shared_step(values List[Int]) {
    set(values, 0, get(values, 0) + 1)
}

fn publish(target List[List[Int]], values List[Int]) {
    set(target, 0, values)
}

async fn main() {
    let worker = run(fn() Int {
            let values = alias(create(10))
            private_step(values, 2)
            get(values, 0)
        })
    let values = [0]
    // Both contexts use one shared_step body, not exponentially many clones.
    shared_step(create(20))
    let shared = run(fn() {
            shared_step(values)
        })
    assert worker.await == 12
    shared.await
    assert get(values, 0) == 1
    let target = [[0]]
    let published = run(fn() {
            let local = create(30)
            // A later publication keeps even earlier accesses guarded.
            shared_step(local)
            publish(target, local)
        })
    published.await
    assert get(get(target, 0), 0) == 31
}
"#,
    )
    .unwrap();
    let binary = common::executable(package.path(), "helpers");
    let ir = package.path().join("helpers.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package.path().to_str().unwrap(),
                "--output",
                binary.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&common::run_tasks(&binary));
        if level == "0" {
            let text = std::fs::read_to_string(&ir).unwrap();
            let bodies: Vec<_> = text
                .split("\ndefine ")
                .filter(|body| body.contains("list.element"))
                .collect();
            let recursive = bodies
                .iter()
                .find(|body| {
                    let name = body.split('@').nth(1).unwrap().split('(').next().unwrap();
                    body.contains(&format!("call void @{name}("))
                })
                .expect("missing private recursive helper");
            assert!(
                !recursive.contains("call void @loom_rt_shared_access"),
                "private recursive helper retained locks: {recursive}"
            );
            assert!(recursive.contains("@loom_rt_worker_checkpoint"));
            assert!(
                bodies
                    .iter()
                    .any(|body| { body.contains("call void @loom_rt_shared_access_begin") }),
                "mixed-context helper lost its locks"
            );
        }
    }
}

#[test]
fn shared_proofs_do_not_restore_private_storage_across_publication_or_loop_backedges() {
    let package = tempfile::tempdir().unwrap();
    for publish in [
        "std.list.set(target, 0, values)",
        "var index = 0\nwhile index < count { std.list.set(target, 0, values)\nindex = index + 1 }",
    ] {
        let body = format!(
            r#"
import std.list.length
import std.list.set
fn published(target List[List[Int]], count Int) List[Int]
ensures length(result) == 1
{{
    let values = [1]
    {publish}
    values
}}
"#
        );
        let path = package.path().join("main.loom");
        std::fs::write(
            &path,
            format!("{body}\nfn main() {{ discard published([[0]], 2) }}"),
        )
        .unwrap();
        success(&common::loom(&["check", package.path().to_str().unwrap()]));
        std::fs::write(&path, format!("{body}\nimport std.task.worker.run\nasync fn main() {{ discard run(fn() List[Int] {{ published([[0]], 2) }}).await }}")).unwrap();
        let output = common::loom(&["check", package.path().to_str().unwrap()]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("interference-safe"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
