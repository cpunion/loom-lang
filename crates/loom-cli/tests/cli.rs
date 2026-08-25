use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
const CROSS_TRIPLE: &str = "x86_64-unknown-linux-gnu";
#[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
const CROSS_TRIPLE: &str = "aarch64-unknown-linux-gnu";

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

    #[cfg(unix)]
    fn make_executable(&self, relative: &str) {
        use std::os::unix::fs::PermissionsExt as _;

        let path = self.0.join(relative);
        let mut permissions = fs::metadata(&path).expect("script metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("make script executable");
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

fn json_record(output: &[u8], category: &str) -> Option<serde_json::Value> {
    String::from_utf8_lossy(output).lines().find_map(|line| {
        let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
        (value.get("category")?.as_str()? == category).then_some(value)
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
fn human_code_frames_do_not_change_the_json_diagnostic_contract() {
    let project =
        TestProject::new("module demo\n\npub fn main() Unit {\n    missing()\n    Unit\n}\n");
    let human = loomc()
        .arg("check")
        .arg(&project.0)
        .output()
        .expect("run human check");
    assert_eq!(human.status.code(), Some(1));
    let stderr = String::from_utf8(human.stderr).expect("UTF-8 human diagnostics");
    assert!(stderr.contains("4 |     missing()"), "{stderr}");
    assert!(stderr.contains('^'), "{stderr}");

    let machine = loomc()
        .args(["--json", "check"])
        .arg(&project.0)
        .output()
        .expect("run JSON check");
    assert_eq!(machine.status.code(), Some(1));
    assert!(machine.stderr.is_empty(), "{:?}", machine.stderr);
    let record = json_record(&machine.stdout, "diagnostic").expect("JSON diagnostic record");
    let mut fields = record
        .as_object()
        .expect("diagnostic object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "category",
            "code",
            "details",
            "message",
            "notes",
            "primary_span",
            "related",
            "schema_version",
            "severity",
        ]
    );
    assert_eq!(record.get("schema_version"), Some(&serde_json::json!(1)));
}

#[test]
fn native_and_interpreter_failures_share_the_structured_json_schema() {
    let project = TestProject::new(
        "module fault_parity\n\npub fn main() Unit {\n    assert false\n    Unit\n}\n",
    );
    let failures = ["interpreter", "llvm"].map(|backend| {
        let output = loomc()
            .args(["--json", "--no-cache", "--backend", backend, "run"])
            .arg(&project.0)
            .output()
            .expect("run failing Loom program");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{backend}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let record = json_record(&output.stdout, "run_failure").unwrap_or_else(|| {
            panic!(
                "{backend} did not emit run_failure: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
        assert_eq!(
            record.get("entry").and_then(serde_json::Value::as_str),
            Some("main")
        );
        record["failure"].clone()
    });
    assert_eq!(failures[0], failures[1]);
}

#[test]
fn check_rejects_an_unknown_language_version() {
    let project = TestProject::empty();
    project.write(
        "loom.toml",
        "schema = 1\nlanguage = \"0.4\"\n[package]\nname = \"future\"\nversion = \"1.0.0\"\n",
    );
    project.write("src/main.loom", "module future\n");
    let output = loomc()
        .args(["--json", "check"])
        .arg(&project.0)
        .output()
        .expect("check future language");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("UnsupportedLanguageVersion"), "{stdout}");
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
fn cache_stat_and_prune_have_stable_json_reports() {
    let project = TestProject::new("module demo\n\npub fn main() Unit { Unit }\n");
    let check = loomc()
        .args(["--json", "--backend", "interpreter", "check"])
        .arg(&project.0)
        .output()
        .expect("populate cache");
    assert_eq!(check.status.code(), Some(0));

    let stat = loomc()
        .args(["--json", "cache", "stat"])
        .arg(&project.0)
        .output()
        .expect("inspect cache");
    assert_eq!(stat.status.code(), Some(0));
    let stdout = String::from_utf8(stat.stdout).expect("UTF-8 stat output");
    assert!(stdout.contains("\"category\":\"cache_stat\""), "{stdout}");
    assert!(stdout.contains("\"schema_version\":2"), "{stdout}");

    let prune = loomc()
        .args(["--json", "cache", "prune"])
        .arg(&project.0)
        .output()
        .expect("prune cache");
    assert_eq!(prune.status.code(), Some(0));
    let stdout = String::from_utf8(prune.stdout).expect("UTF-8 prune output");
    assert!(stdout.contains("\"category\":\"cache_prune\""), "{stdout}");
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
            .join("target/loom/cache/v2/refs/artifact")
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
    let checked = loom_driver::decode_library_artifact(
        &fs::read(&first_artifact).expect("read portable library"),
    )
    .expect("decode and validate portable library");
    assert!(
        checked
            .program()
            .as_program()
            .exports
            .contains_key("sample.answer")
    );
    assert_eq!(checked.root_package().name(), "sample");
    assert_eq!(checked.root_package().language(), "0.3");
    assert_eq!(checked.interfaces().len(), 1);

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

    project.write(
        "consumer/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n[dependencies]\nsample = { artifact = \"../sample.loomlib\", version = \"^0.1\" }\n[[target]]\nname = \"consumer\"\nkind = \"bin\"\nentry = \"consumer.main\"\n[[target]]\nname = \"consumer-tests\"\nkind = \"test\"\n",
    );
    project.write(
        "consumer/src/main.loom",
        "module consumer\n\nimport sample.answer\n\npub fn main() Unit {\n    let value = answer()\n    assert value == 42\n    Unit\n}\n\ntest fn artifact_dependency_works() {\n    main()\n}\n",
    );
    fs::remove_dir_all(project.0.join("src")).expect("remove producer sources");
    let consumed = loomc()
        .args(["--backend", "interpreter", "run"])
        .arg(project.0.join("consumer"))
        .output()
        .expect("run artifact consumer without producer sources");
    assert_eq!(
        consumed.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&consumed.stdout),
        String::from_utf8_lossy(&consumed.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&consumed.stdout), "Unit\n");

    for command in ["check", "test"] {
        let verified = loomc()
            .arg(command)
            .arg(project.0.join("consumer"))
            .output()
            .expect("verify artifact consumer with LLVM backend");
        assert_eq!(
            verified.status.code(),
            Some(0),
            "command={command} stdout={} stderr={}",
            String::from_utf8_lossy(&verified.stdout),
            String::from_utf8_lossy(&verified.stderr)
        );
    }
    let native = loomc()
        .args(["run", "--target", "consumer"])
        .arg(project.0.join("consumer"))
        .output()
        .expect("run artifact consumer with LLVM backend");
    assert_eq!(
        native.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&native.stdout),
        String::from_utf8_lossy(&native.stderr)
    );
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
fn fmt_never_writes_dependency_sources() {
    let project = TestProject::empty();
    let dependency_source = "module utility\n\npub fn value() Int {\n\t1   \n}\n";
    project.write(
        "utility/loom.toml",
        "schema = 1\n[package]\nname = \"utility\"\nversion = \"1.0.0\"\n",
    );
    project.write("utility/src/lib.loom", dependency_source);
    project.write(
        "application/loom.toml",
        "schema = 1\n[package]\nname = \"application\"\nversion = \"1.0.0\"\n[dependencies]\nutility = { path = \"../utility\" }\n",
    );
    project.write(
        "application/src/main.loom",
        "module application\n\nfn local() Unit {\n\tUnit   \n}\n",
    );

    let output = loomc()
        .arg("fmt")
        .arg(project.0.join("application"))
        .output()
        .expect("format root package");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        fs::read_to_string(project.0.join("utility/src/lib.loom")).expect("read dependency source"),
        dependency_source
    );
    assert_eq!(
        fs::read_to_string(project.0.join("application/src/main.loom")).expect("read root source"),
        "module application\n\nfn local() Unit {\n    Unit\n}\n"
    );
}

#[test]
fn configured_entries_use_one_strict_signature_check() {
    for (name, declaration) in [
        ("parameters", "pub fn main(value Int) Unit { Unit }"),
        ("generic", "pub fn main[T]() Unit { Unit }"),
        ("return", "pub fn main() Int { 1 }"),
    ] {
        let project = TestProject::empty();
        project.write(
            "loom.toml",
            "schema = 1\n[package]\nname = \"sample\"\nversion = \"1.0.0\"\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"sample.main\"\n",
        );
        project.write(
            "src/main.loom",
            &format!("module sample\n\n{declaration}\n"),
        );
        let output = loomc()
            .args(["--json", "check"])
            .arg(&project.0)
            .output()
            .unwrap_or_else(|error| panic!("run {name} entry check: {error}"));
        assert_eq!(output.status.code(), Some(1), "{name}: {output:?}");
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
        assert!(stdout.contains("InvalidEntrySignature"), "{name}: {stdout}");
    }
}

#[test]
fn dependency_public_functions_cannot_be_selected_as_root_entries() {
    let project = TestProject::empty();
    project.write(
        "dependency/loom.toml",
        "schema = 1\n[package]\nname = \"dependency\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "dependency/src/lib.loom",
        "module dependency\n\npub fn main() Unit { Unit }\n",
    );
    project.write(
        "application/loom.toml",
        "schema = 1\n[package]\nname = \"application\"\nversion = \"1.0.0\"\n[dependencies]\ndependency = { path = \"../dependency\" }\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"dependency.main\"\n",
    );
    project.write("application/src/main.loom", "module application\n");
    let output = loomc()
        .args(["--json", "check"])
        .arg(project.0.join("application"))
        .output()
        .expect("check dependency entry");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("UnknownEntry"), "{stdout}");
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

#[cfg(unix)]
#[test]
fn debug_builds_source_mapped_native_code_and_launches_a_debugger() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");
    project.write(
        "debug-wrapper",
        "#!/bin/sh\nexecutable=$1\nshift\ntest -x \"$executable\" || exit 91\ntest \"$1\" = \"--\" || exit 92\nshift\ntest \"$1\" = \"alpha\" || exit 93\ntest \"$2\" = \"beta gamma\" || exit 94\nprintf 'debug-wrapper:%s:%s\\n' \"$1\" \"$2\"\n\"$executable\" \"$@\"\n",
    );
    project.make_executable("debug-wrapper");
    let output = loomc()
        .args(["debug", "--debugger"])
        .arg(project.0.join("debug-wrapper"))
        .arg(&project.0)
        .args(["--", "alpha", "beta gamma"])
        .output()
        .expect("launch source debugger wrapper");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("debugging "), "{stdout}");
    assert!(
        stdout.contains("debug-wrapper:alpha:beta gamma"),
        "{stdout}"
    );
    assert!(stdout.contains("Unit"), "{stdout}");
}

#[test]
fn debug_rejects_non_native_noninteractive_and_release_modes() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");
    for (arguments, expected) in [
        (
            vec!["--backend", "interpreter", "debug"],
            "require the LLVM backend",
        ),
        (vec!["--release", "debug"], "does not accept --release"),
        (vec!["--json", "debug"], "does not accept --json"),
    ] {
        let output = loomc()
            .args(arguments)
            .arg(&project.0)
            .output()
            .expect("reject invalid debug mode");
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn release_and_cross_target_object_builds_are_distinct_and_cached() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");
    let release_object = project.0.join("release-aarch64.o");
    let release = loomc()
        .args([
            "--json",
            "--release",
            "--target-triple",
            CROSS_TRIPLE,
            "build",
            "--emit",
            "object",
            "--output",
        ])
        .arg(&release_object)
        .arg(&project.0)
        .output()
        .expect("emit release cross object");
    assert_eq!(
        release.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&release.stdout),
        String::from_utf8_lossy(&release.stderr)
    );
    assert!(
        fs::read(&release_object)
            .expect("read release object")
            .starts_with(b"\x7fELF")
    );
    assert_eq!(
        cache_status(&release.stdout, "target_object").as_deref(),
        Some("miss")
    );

    let cached_object = project.0.join("release-aarch64-copy.o");
    let cached = loomc()
        .args([
            "--json",
            "--release",
            "--target-triple",
            CROSS_TRIPLE,
            "build",
            "--emit",
            "object",
            "--output",
        ])
        .arg(&cached_object)
        .arg(&project.0)
        .output()
        .expect("restore cross object cache");
    assert_eq!(cached.status.code(), Some(0));
    assert_eq!(
        cache_status(&cached.stdout, "target_object").as_deref(),
        Some("hit")
    );
    assert_eq!(
        fs::read(&release_object).expect("read first cross object"),
        fs::read(&cached_object).expect("read cached cross object")
    );

    let development_object = project.0.join("development-aarch64.o");
    let development = loomc()
        .args([
            "--json",
            "--target-triple",
            CROSS_TRIPLE,
            "build",
            "--emit",
            "object",
            "--output",
        ])
        .arg(&development_object)
        .arg(&project.0)
        .output()
        .expect("emit development cross object");
    assert_eq!(development.status.code(), Some(0));
    assert_eq!(
        cache_status(&development.stdout, "target_object").as_deref(),
        Some("miss")
    );
    assert_ne!(
        cache_key(&release.stdout, "target_object"),
        cache_key(&development.stdout, "target_object")
    );

    let cross_link = loomc()
        .args([
            "--json",
            "--target-triple",
            CROSS_TRIPLE,
            "build",
            "--output",
        ])
        .arg(project.0.join("invalid-cross-executable"))
        .arg(&project.0)
        .output()
        .expect("reject cross executable link");
    assert_eq!(cross_link.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&cross_link.stdout).contains("CrossLinkUnavailable"));
}

#[test]
fn release_build_produces_a_runnable_native_executable() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");
    let release_executable = project.0.join("release-native");
    let native = loomc()
        .args(["--release", "build", "--output"])
        .arg(&release_executable)
        .arg(&project.0)
        .output()
        .expect("build release native executable");
    assert_eq!(native.status.code(), Some(0));
    let executed = loomc()
        .args(["run", "--artifact"])
        .arg(&release_executable)
        .output()
        .expect("run release executable");
    assert_eq!(executed.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&executed.stdout), "Unit\n");
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

#[test]
fn run_rejects_an_incompatible_artifact_language_version() {
    let project = TestProject::new("module demo\n\npub fn main() Unit { Unit }\n");
    let artifact = project.0.join("future.loomi");
    let build = loomc()
        .args(["--backend", "interpreter", "build", "--output"])
        .arg(&artifact)
        .arg(&project.0)
        .output()
        .expect("build interpreted artifact");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let bytes = fs::read(&artifact).expect("read artifact");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("artifact JSON");
    value["languageVersion"] = serde_json::json!("0.4");
    fs::write(
        &artifact,
        serde_json::to_vec(&value).expect("artifact JSON"),
    )
    .expect("tamper language version");

    let output = loomc()
        .args(["--json", "--backend", "interpreter", "run", "--artifact"])
        .arg(&artifact)
        .output()
        .expect("run future-language artifact");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    assert!(
        stdout.contains("ArtifactLanguageVersionMismatch"),
        "{stdout}"
    );
}
