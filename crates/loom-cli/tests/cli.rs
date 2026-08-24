use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestProject(PathBuf);

impl TestProject {
    fn empty() -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("loom-cli-test-{}-{serial}", std::process::id()));
        fs::create_dir_all(&root).expect("create project");
        Self(root)
    }

    fn new(source: &str) -> Self {
        let project = Self::empty();
        project.write("main.loom", source);
        project
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().expect("test path has parent"))
            .expect("create source parent");
        fs::write(path, text).expect("write project file");
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn loomc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_loomc"))
}

fn cache_status(output: &[u8], layer: &str) -> Option<String> {
    String::from_utf8_lossy(output).lines().find_map(|line| {
        let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
        (value.get("category")?.as_str()? == "cache_result"
            && value.get("layer")?.as_str()? == layer)
            .then(|| value.get("status")?.as_str().map(str::to_owned))
            .flatten()
    })
}

fn cache_key(output: &[u8], layer: &str) -> Option<String> {
    String::from_utf8_lossy(output).lines().find_map(|line| {
        let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
        (value.get("category")?.as_str()? == "cache_result"
            && value.get("layer")?.as_str()? == layer)
            .then(|| value.get("key")?.as_str().map(str::to_owned))
            .flatten()
    })
}

#[test]
fn check_reports_source_errors_before_pipeline_incompleteness() {
    let project = TestProject::new("fn broken(");
    let output = loomc()
        .args(["--json", "check"])
        .arg(&project.0)
        .output()
        .expect("run loomc");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("MissingModuleDeclaration"), "{stdout}");
    assert!(!stdout.contains("CompilerPipelineIncomplete"), "{stdout}");
}

#[test]
fn clean_check_uses_the_real_semantic_pipeline() {
    let project = TestProject::new("module demo\n\nfn main() Unit {\n    Unit\n}\n");
    let output = loomc()
        .args(["check", "--json"])
        .arg(&project.0)
        .output()
        .expect("run loomc");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("\"status\":\"ok\""), "{stdout}");
    assert!(!stdout.contains("CompilerPipelineIncomplete"), "{stdout}");
}

#[test]
fn persistent_cache_hits_content_keys_and_final_artifacts() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");

    let check = |extra: &[&str]| {
        loomc()
            .args(["--json", "--backend", "interpreter"])
            .args(extra)
            .arg(&project.0)
            .output()
            .expect("run cached check")
    };
    let first = check(&["check"]);
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(
        cache_status(&first.stdout, "checked_mir").as_deref(),
        Some("miss")
    );

    let second = check(&["check"]);
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(
        cache_status(&second.stdout, "checked_mir").as_deref(),
        Some("hit")
    );

    project.write(
        "main.loom",
        "module demo\n\npub fn main() Unit {\n    assert true\n    Unit\n}\n",
    );
    let changed = check(&["check"]);
    assert_eq!(changed.status.code(), Some(0));
    assert_eq!(
        cache_status(&changed.stdout, "checked_mir").as_deref(),
        Some("miss")
    );

    let uncached = check(&["--no-cache", "check"]);
    assert_eq!(uncached.status.code(), Some(0));
    assert_eq!(
        cache_status(&uncached.stdout, "checked_mir").as_deref(),
        Some("disabled")
    );

    let first_artifact = project.0.join("first.loomi");
    let first_build = loomc()
        .args(["--json", "--backend", "interpreter", "build", "--output"])
        .arg(&first_artifact)
        .arg(&project.0)
        .output()
        .expect("build first cached artifact");
    assert_eq!(first_build.status.code(), Some(0));
    assert_eq!(
        cache_status(&first_build.stdout, "final_artifact").as_deref(),
        Some("miss")
    );

    let second_artifact = project.0.join("second.loomi");
    let second_build = loomc()
        .args(["--json", "--backend", "interpreter", "build", "--output"])
        .arg(&second_artifact)
        .arg(&project.0)
        .output()
        .expect("restore second cached artifact");
    assert_eq!(second_build.status.code(), Some(0));
    assert_eq!(
        cache_status(&second_build.stdout, "final_artifact").as_deref(),
        Some("hit")
    );
    assert_eq!(
        fs::read(first_artifact).expect("read first artifact"),
        fs::read(second_artifact).expect("read restored artifact")
    );
}

#[test]
fn unreachable_private_body_edits_reuse_native_object_and_final_link() {
    let project = TestProject::new(
        "module demo\n\npub fn main() Unit {\n    Unit\n}\n\nfn dead() Int {\n    1\n}\n",
    );
    let first_artifact = project.0.join("first.native");
    let first = loomc()
        .args(["--json", "build", "--output"])
        .arg(&first_artifact)
        .arg(&project.0)
        .output()
        .expect("build first native artifact");
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(
        cache_status(&first.stdout, "target_object").as_deref(),
        Some("miss")
    );

    project.write(
        "main.loom",
        "module demo\n\npub fn main() Unit {\n    Unit\n}\n\nfn dead() Int {\n    2\n}\n",
    );
    let second_artifact = project.0.join("second.native");
    let second = loomc()
        .args(["--json", "build", "--output"])
        .arg(&second_artifact)
        .arg(&project.0)
        .output()
        .expect("reuse DCE-safe final artifact");
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(
        cache_status(&second.stdout, "checked_mir").as_deref(),
        Some("miss")
    );
    assert_eq!(
        cache_status(&second.stdout, "final_artifact").as_deref(),
        Some("hit")
    );
    assert_eq!(
        fs::read(&first_artifact).expect("read first native artifact"),
        fs::read(&second_artifact).expect("read second native artifact")
    );

    let final_key = cache_key(&second.stdout, "final_artifact").expect("final artifact key");
    fs::write(
        project
            .0
            .join("target/loom/cache/v1/refs/artifact")
            .join(format!("{final_key}.json")),
        b"corrupt",
    )
    .expect("corrupt generated final ref");
    let third = loomc()
        .args(["--json", "build", "--output"])
        .arg(project.0.join("third.native"))
        .arg(&project.0)
        .output()
        .expect("relink cached target object");
    assert_eq!(third.status.code(), Some(0));
    assert_eq!(
        cache_status(&third.stdout, "final_artifact").as_deref(),
        Some("miss")
    );
    assert_eq!(
        cache_status(&third.stdout, "target_object").as_deref(),
        Some("hit")
    );
}

#[test]
fn manifest_targets_and_path_dependencies_drive_cli_roots() {
    let project = TestProject::empty();
    project.write(
        "utility/loom.toml",
        "schema = 1\n[package]\nname = \"utility\"\nversion = \"1.1.0\"\n",
    );
    project.write(
        "utility/src/lib.loom",
        "module utility\n\npub fn increment(value Int) Int {\n    value + 1\n}\n",
    );
    project.write(
        "application/loom.toml",
        "schema = 1\n[package]\nname = \"application\"\nversion = \"0.1.0\"\n[dependencies]\nutility = { path = \"../utility\", version = \"^1\" }\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"application.start\"\n[[target]]\nname = \"unit\"\nkind = \"test\"\n",
    );
    project.write(
        "application/src/main.loom",
        "module application\n\nimport utility.increment\n\npub fn start() Unit {\n    let value = increment(1)\n    assert value == 2\n    Unit\n}\n\ntest fn dependency_works() {\n    let value = increment(2)\n    assert value == 3\n    Unit\n}\n",
    );
    let root = project.0.join("application");

    for arguments in [
        vec!["check", "--target", "app"],
        vec!["run", "--target", "app"],
        vec!["test", "--target", "unit"],
    ] {
        let output = loomc()
            .args(arguments)
            .arg(&root)
            .output()
            .expect("run manifest command");
        assert_eq!(
            output.status.code(),
            Some(0),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let artifact = project.0.join("application.native");
    let build = loomc()
        .args(["--json", "build", "--target", "app", "--output"])
        .arg(&artifact)
        .arg(&root)
        .output()
        .expect("build manifest target");
    assert_eq!(build.status.code(), Some(0));
    assert_eq!(
        cache_status(&build.stdout, "final_artifact").as_deref(),
        Some("hit"),
        "the preceding source run and build share a target artifact key"
    );
    assert!(artifact.is_file());

    let artifact_run = loomc()
        .args(["run", "--artifact"])
        .arg(&artifact)
        .output()
        .expect("run materialized cached native artifact");
    assert_eq!(artifact_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&artifact_run.stdout), "Unit\n");

    let interpreted_artifact = project.0.join("application.loomi");
    let interpreted_build = loomc()
        .args([
            "--backend",
            "interpreter",
            "build",
            "--target",
            "app",
            "--output",
        ])
        .arg(&interpreted_artifact)
        .arg(&root)
        .output()
        .expect("build interpreted manifest target");
    assert_eq!(
        interpreted_build.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&interpreted_build.stdout),
        String::from_utf8_lossy(&interpreted_build.stderr)
    );
    let interpreted_run = loomc()
        .args(["--backend", "interpreter", "run", "--artifact"])
        .arg(&interpreted_artifact)
        .output()
        .expect("run interpreted manifest artifact");
    assert_eq!(interpreted_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&interpreted_run.stdout), "Unit\n");

    let wrong_kind = loomc()
        .args(["--json", "run", "--target", "unit"])
        .arg(&root)
        .output()
        .expect("reject test target for run");
    assert_eq!(wrong_kind.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&wrong_kind.stdout).contains("TargetKindMismatch"));
}

#[test]
fn library_targets_build_portable_validated_artifacts() {
    let project = TestProject::empty();
    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"sample\"\nversion = \"0.1.0\"\n[[target]]\nname = \"api\"\nkind = \"lib\"\n",
    );
    project.write(
        "src/lib.loom",
        "module sample\n\npub fn answer() Int {\n    42\n}\n",
    );
    let first_artifact = project.0.join("sample.loomlib");
    let first = loomc()
        .args(["--json", "build", "--target", "api", "--output"])
        .arg(&first_artifact)
        .arg(&project.0)
        .output()
        .expect("build portable library");
    assert_eq!(
        first.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        cache_status(&first.stdout, "final_artifact").as_deref(),
        Some("miss")
    );
    let checked = loom_mir::decode_interpreted_artifact(
        &fs::read(&first_artifact).expect("read portable library"),
    )
    .expect("decode and validate portable library");
    assert!(checked.as_program().exports.contains_key("sample.answer"));

    let second_artifact = project.0.join("sample-copy.loomlib");
    let second = loomc()
        .args(["--json", "build", "--target", "api", "--output"])
        .arg(&second_artifact)
        .arg(&project.0)
        .output()
        .expect("restore cached portable library");
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(
        cache_status(&second.stdout, "final_artifact").as_deref(),
        Some("hit")
    );
    assert_eq!(
        fs::read(first_artifact).expect("read first library"),
        fs::read(second_artifact).expect("read cached library")
    );

    for command in ["run", "test"] {
        let rejected = loomc()
            .args(["--json", command, "--target", "api"])
            .arg(&project.0)
            .output()
            .expect("reject library as executable target");
        assert_eq!(rejected.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&rejected.stdout).contains("TargetKindMismatch"));
    }
}

#[test]
fn cli_resolves_registry_features_and_enforces_lockfiles() {
    let project = TestProject::empty();
    let write_registry = |version: &str, answer: i64| {
        project.write(
            &format!("registry/utility/{version}/loom.toml"),
            &format!("schema = 1\n[package]\nname = \"utility\"\nversion = \"{version}\"\n"),
        );
        project.write(
            &format!("registry/utility/{version}/src/lib.loom"),
            &format!("module utility\n\npub fn answer() Int {{\n    {answer}\n}}\n"),
        );
    };
    write_registry("1.0.0", 10);
    write_registry("1.2.0", 12);
    project.write(
        "app/loom.toml",
        "schema = 1\n[package]\nname = \"app\"\nversion = \"0.1.0\"\n[registries]\nlocal = \"../registry\"\n[dependencies]\nutility = { registry = \"local\", version = \"^1\", optional = true }\n[features]\ndefault = [\"utilities\"]\nutilities = [\"dep:utility\"]\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"app.main\"\n",
    );
    project.write(
        "app/src/main.loom",
        "module app\n\nimport utility.answer\n\npub fn main() Unit {\n    let value = answer()\n    assert value > 0\n    Unit\n}\n",
    );
    let root = project.0.join("app");

    let resolved = loomc()
        .args(["--json", "resolve"])
        .arg(&root)
        .output()
        .expect("resolve registry graph");
    assert_eq!(resolved.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&resolved.stdout).contains("dependency_resolution"));
    let lock_path = root.join("loom.lock");
    let first_lock = fs::read_to_string(&lock_path).expect("read generated lockfile");
    assert!(first_lock.contains("version = \"1.2.0\""), "{first_lock}");

    let locked_check = loomc()
        .args(["--locked", "check"])
        .arg(&root)
        .output()
        .expect("check locked graph");
    assert_eq!(locked_check.status.code(), Some(0));

    let without_default = loomc()
        .args(["--no-default-features", "check"])
        .arg(&root)
        .output()
        .expect("disable optional registry dependency");
    assert_eq!(without_default.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&without_default.stderr).contains("utility"));
    let explicit_feature = loomc()
        .args(["--no-default-features", "--features", "utilities", "check"])
        .arg(&root)
        .output()
        .expect("explicitly activate optional dependency");
    assert_eq!(explicit_feature.status.code(), Some(0));

    write_registry("1.3.0", 13);
    let pinned = loomc()
        .arg("resolve")
        .arg(&root)
        .output()
        .expect("keep locked version without update");
    assert_eq!(pinned.status.code(), Some(0));
    assert!(
        fs::read_to_string(&lock_path)
            .expect("read pinned lockfile")
            .contains("version = \"1.2.0\"")
    );
    let updated = loomc()
        .args(["resolve", "--update"])
        .arg(&root)
        .output()
        .expect("refresh registry version");
    assert_eq!(updated.status.code(), Some(0));
    assert!(
        fs::read_to_string(&lock_path)
            .expect("read refreshed lockfile")
            .contains("version = \"1.3.0\"")
    );

    project.write(
        "registry/utility/1.3.0/src/lib.loom",
        "module utility\n\npub fn answer() Int {\n    99\n}\n",
    );
    let tampered = loomc()
        .arg("check")
        .arg(&root)
        .output()
        .expect("reject mutable registry package");
    assert_eq!(tampered.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&tampered.stderr).contains("checksum differs"));
}

#[test]
fn fmt_check_and_write_form_an_idempotent_real_file_flow() {
    let project = TestProject::new("module demo\r\n\r\nfn main() Unit {\r\n\tUnit   \r\n}\r\n\r\n");
    let first = loomc()
        .args(["fmt", "--check"])
        .arg(&project.0)
        .output()
        .expect("run fmt check");
    assert_eq!(first.status.code(), Some(1));

    let write = loomc()
        .arg("fmt")
        .arg(&project.0)
        .output()
        .expect("run fmt");
    assert_eq!(write.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(project.0.join("main.loom")).expect("read formatted source"),
        "module demo\n\nfn main() Unit {\n    Unit\n}\n"
    );

    let second = loomc()
        .args(["fmt", "--check"])
        .arg(&project.0)
        .output()
        .expect("run second fmt check");
    assert_eq!(second.status.code(), Some(0));
}

#[test]
fn test_and_run_execute_native_code() {
    let project = TestProject::new(
        "module demo\n\npub fn main() Unit {\n    Unit\n}\n\ntest fn passes() {\n    assert true\n    Unit\n}\n",
    );
    let test = loomc()
        .arg("test")
        .arg(&project.0)
        .output()
        .expect("run loomc test");
    assert_eq!(test.status.code(), Some(0));
    let stdout = String::from_utf8(test.stdout).expect("UTF-8 stdout");
    assert!(stdout.contains("passed demo.passes"), "{stdout}");

    let run = loomc()
        .arg("run")
        .arg(&project.0)
        .output()
        .expect("run loomc run");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(run.stdout).expect("UTF-8 stdout"),
        "Unit\n"
    );
}

#[test]
fn range_and_growable_list_run_on_both_backends() {
    let project = TestProject::new(
        "module dynamic\n\nimport standard.int.parse_int\nimport standard.process.arguments\nimport standard.process.environment\n\nasync fn worker(value Int) Int {\n    value * 2\n}\n\npub async fn main() Unit {\n    let processArguments = arguments()\n    let argumentCount = processArguments.length()\n    assert argumentCount == 5\n    let count = match environment(\"LOOM_WORKERS\") {\n        Some(text) => {\n            match parse_int(text) {\n                Ok(value) => value\n                Err(ParseIntError.InvalidSyntax) => 0\n                Err(ParseIntError.OutOfRange) => 0\n            }\n        }\n        None => 0\n    }\n    assert count == 5\n    match environment(\"LOOM_TEST_ENV\") {\n        Some(value) => {\n            assert value == \"visible\"\n            Unit\n        }\n        None => {\n            assert false\n            Unit\n        }\n    }\n    var tasks = List[Task[Int]]()\n    for i in 0..count {\n        tasks.add(worker(i))\n        Unit\n    }\n    let values = Task.all(tasks).await\n    let length = values.length()\n    assert length == count\n    let selected = values.get(3)\n    match selected {\n        Some(value) => {\n            assert value == 6\n            Unit\n        }\n        None => {\n            assert false\n            Unit\n        }\n    }\n    let missing = values.get(-1)\n    match missing {\n        Some(_) => {\n            assert false\n            Unit\n        }\n        None => Unit\n    }\n    Unit\n}\n",
    );
    for backend in ["interpreter", "llvm"] {
        let output = loomc()
            .args(["--backend", backend, "run"])
            .arg(&project.0)
            .arg("--")
            .args(["one", "two", "three", "four", "five"])
            .env("LOOM_TEST_ENV", "visible")
            .env("LOOM_WORKERS", "5")
            .output()
            .expect("run range/List program");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{backend}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "Unit\n");

        let artifact = project.0.join(format!("dynamic-{backend}.artifact"));
        let build = loomc()
            .args(["--backend", backend, "build", "--output"])
            .arg(&artifact)
            .arg(&project.0)
            .output()
            .expect("build range/List artifact");
        assert_eq!(
            build.status.code(),
            Some(0),
            "{backend} build: stdout={} stderr={}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let artifact_output = loomc()
            .args(["--backend", backend, "run", "--artifact"])
            .arg(&artifact)
            .arg("--")
            .args(["one", "two", "three", "four", "five"])
            .env("LOOM_TEST_ENV", "visible")
            .env("LOOM_WORKERS", "5")
            .output()
            .expect("run range/List artifact");
        assert_eq!(
            artifact_output.status.code(),
            Some(0),
            "{backend} artifact: stdout={} stderr={}",
            String::from_utf8_lossy(&artifact_output.stdout),
            String::from_utf8_lossy(&artifact_output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&artifact_output.stdout), "Unit\n");
    }
}

#[test]
fn build_writes_a_runnable_native_artifact() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");
    let artifact = project.0.join("out.native");
    let mut build = loomc();
    build
        .args(["build", "--output"])
        .arg(&artifact)
        .arg(&project.0);
    let output = build.output().expect("run loomc build");
    assert_eq!(output.status.code(), Some(0));
    assert!(artifact.exists());
    assert!(
        !fs::read(&artifact)
            .expect("read artifact")
            .starts_with(b"{")
    );

    let run = loomc()
        .args(["run", "--artifact"])
        .arg(&artifact)
        .output()
        .expect("run built artifact");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(run.stdout).expect("UTF-8 stdout"),
        "Unit\n"
    );
}

#[test]
fn core_examples_close_check_build_test_and_run() {
    for (version, source) in [
        ("core01", include_str!("../../../examples/core01/shop.loom")),
        (
            "core02",
            include_str!("../../../examples/core02/concepts.loom"),
        ),
        (
            "core03",
            include_str!("../../../examples/core03/tasks.loom"),
        ),
    ] {
        let project = TestProject::new(source);
        for command in ["check", "test", "run"] {
            let output = loomc()
                .arg(command)
                .arg(&project.0)
                .output()
                .expect("run loomc command");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{version} {command}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let artifact = project.0.join(format!("{version}.native"));
        let mut command = loomc();
        command
            .args(["build", "--output"])
            .arg(&artifact)
            .arg(&project.0);
        let build = command.output().expect("build version artifact");
        assert_eq!(build.status.code(), Some(0), "{version} build");
        let run = loomc()
            .args(["run", "--artifact"])
            .arg(artifact)
            .output()
            .expect("run version artifact");
        assert_eq!(run.status.code(), Some(0), "{version} artifact run");
    }
}

#[test]
fn run_rejects_an_incompatible_artifact_version() {
    let project = TestProject::new("module demo\n");
    let artifact = project.0.join("old.loomi");
    fs::write(
        &artifact,
        br#"{"format":"loom.interpreted-mir","version":999,"program":{},"floatBits":[]}"#,
    )
    .expect("write incompatible artifact");
    let output = loomc()
        .args(["--json", "--backend", "interpreter", "run", "--artifact"])
        .arg(&artifact)
        .output()
        .expect("run incompatible artifact");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    assert!(stdout.contains("ArtifactVersionMismatch"), "{stdout}");
}
