use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use sha2::{Digest as _, Sha256};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
static TEST_RUNTIME: OnceLock<TestRuntimeBundle> = OnceLock::new();

struct TestRuntimeBundle {
    _directory: tempfile::TempDir,
    root: PathBuf,
}

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
    let runtime = TEST_RUNTIME.get_or_init(|| {
        let archive = test_runtime_archive();
        assert!(
            archive.is_file(),
            "loom-runtime dev-dependency did not produce {}",
            archive.display()
        );
        let directory = tempfile::tempdir().expect("create CLI test runtime directory");
        let root = directory.path().join("runtime");
        loom_codegen_llvm::pack_native_runtime_bundle(&archive, &root)
            .expect("pack CLI test runtime");
        TestRuntimeBundle {
            _directory: directory,
            root,
        }
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_loomc"));
    command.env("LOOM_RUNTIME_BUNDLE", &runtime.root);
    command
}

fn test_runtime_archive() -> PathBuf {
    let compiler = PathBuf::from(env!("CARGO_BIN_EXE_loomc"));
    let profile = compiler
        .parent()
        .expect("loomc test binary is in the Cargo profile directory");
    profile.join(loom_codegen_llvm::native_runtime_archive_name(None))
}

fn native_executable(path: impl AsRef<std::path::Path>) -> PathBuf {
    loom_codegen_llvm::native_artifact_path(
        path,
        None,
        loom_codegen_llvm::NativeArtifactKind::Executable,
    )
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(unix)]
fn write_fake_runtime_bundle(root: &std::path::Path, archive: &[u8]) {
    fs::create_dir_all(root).expect("create fake runtime bundle");
    let target = loom_codegen_llvm::target_identity(
        Some(CROSS_TRIPLE),
        loom_codegen_llvm::OptimizationProfile::Development,
    )
    .expect("cross target identity");
    let archive_sha256 = format!("{:x}", Sha256::digest(archive));
    fs::write(root.join("runtime.a"), archive).expect("write fake runtime archive");
    fs::write(
        root.join(loom_codegen_llvm::RUNTIME_BUNDLE_MANIFEST),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": loom_codegen_llvm::RUNTIME_BUNDLE_SCHEMA_VERSION,
            "target_triple": target.triple,
            "data_layout": target.data_layout,
            "runtime_cpu": loom_codegen_llvm::RUNTIME_CPU,
            "runtime_cpu_features": loom_codegen_llvm::RUNTIME_CPU_FEATURES,
            "runtime_abi": loom_codegen_llvm::NATIVE_RUNTIME_ABI,
            "archive": "runtime.a",
            "archive_sha256": archive_sha256,
            "link_args": ["-ldl", "-lpthread", "-lm", "-lrt", "-lutil"],
        }))
        .expect("encode fake runtime manifest"),
    )
    .expect("write fake runtime manifest");
}

#[cfg(unix)]
fn write_fake_linker(project: &TestProject, marker: &str) {
    project.write(
        "fake-linker",
        &format!(
            r#"#!/bin/sh
# {marker}
set -eu
if [ "${{1-}}" = "--version" ]; then
    printf 'loom fake linker v1\n'
    exit 0
fi
log=${{LOOM_FAKE_LINK_LOG:?}}
object_copy=${{LOOM_FAKE_OBJECT_COPY:?}}
payload=${{LOOM_FAKE_LINK_PAYLOAD:?}}
: > "$log"
cp "$1" "$object_copy"
output=
while [ "$#" -gt 0 ]; do
    argument=$1
    shift
    printf '%s\n' "$argument" >> "$log"
    if [ "$argument" = "-o" ]; then
        output=$1
        shift
        printf '%s\n' "$output" >> "$log"
    fi
done
[ -n "$output" ]
printf '#!/bin/sh\n# %s\nexit 0\n' "$(cat "$payload")" > "$output"
chmod 755 "$output"
"#,
        ),
    );
    project.make_executable("fake-linker");
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: std::collections::BTreeMap<String, String>,
    body: Vec<u8>,
}

struct RegistryFixture {
    url: String,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
    handle: thread::JoinHandle<()>,
}

impl RegistryFixture {
    fn spawn(expected_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind registry fixture");
        listener
            .set_nonblocking(true)
            .expect("nonblocking registry fixture");
        let address = listener.local_addr().expect("registry fixture address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut bundle = None::<(Vec<u8>, String)>;
            while observed.lock().expect("request log").len() < expected_requests {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "registry fixture timed out");
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept registry request: {error}"),
                };
                stream
                    .set_nonblocking(false)
                    .expect("blocking registry connection");
                let request = read_http_request(&mut stream);
                assert_eq!(
                    request.headers.get("authorization").map(String::as_str),
                    None,
                    "{request:#?}"
                );
                let response = match (request.method.as_str(), request.path.as_str()) {
                    ("PUT", "/registry/v1/packages/utility/versions/1.2.0") => {
                        let sha256 = request
                            .headers
                            .get("x-loom-sha256")
                            .expect("publish checksum")
                            .clone();
                        assert_eq!(sha256.len(), 64);
                        bundle = Some((request.body.clone(), sha256));
                        (201, Vec::new())
                    }
                    ("GET", "/registry/v1/packages/utility") => {
                        let (_, sha256) = bundle.as_ref().expect("package was published first");
                        (
                            200,
                            serde_json::to_vec(&serde_json::json!({
                                "schema_version": 1,
                                "versions": [{"version": "1.2.0", "sha256": sha256}]
                            }))
                            .expect("encode registry index"),
                        )
                    }
                    ("GET", "/registry/v1/packages/utility/versions/1.2.0") => {
                        (200, bundle.as_ref().expect("published bundle").0.clone())
                    }
                    _ => panic!("unexpected registry request: {request:#?}"),
                };
                observed.lock().expect("request log").push(request);
                write_http_response(&mut stream, response.0, &response.1);
            }
        });
        Self {
            url: format!("http://{address}/registry"),
            requests,
            handle,
        }
    }

    fn finish(self) -> Vec<HttpRequest> {
        self.handle.join().expect("registry fixture thread");
        Arc::try_unwrap(self.requests)
            .expect("only test owns request log")
            .into_inner()
            .expect("request log")
    }
}

fn read_http_request(stream: &mut TcpStream) -> HttpRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("registry read timeout");
    let mut bytes = Vec::new();
    let mut scratch = [0_u8; 4096];
    let (header_end, content_length) = loop {
        let count = stream.read(&mut scratch).expect("read registry request");
        assert_ne!(count, 0, "request ended before headers");
        bytes.extend_from_slice(&scratch[..count]);
        if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..header_end]).expect("UTF-8 request headers");
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            break (header_end, content_length);
        }
    };
    let body_start = header_end + 4;
    while bytes.len() < body_start + content_length {
        let count = stream.read(&mut scratch).expect("read registry body");
        assert_ne!(count, 0, "request ended before body");
        bytes.extend_from_slice(&scratch[..count]);
    }
    let headers = std::str::from_utf8(&bytes[..header_end]).expect("UTF-8 request headers");
    let mut lines = headers.lines();
    let request_line = lines.next().expect("request line");
    let mut fields = request_line.split_whitespace();
    let method = fields.next().expect("request method").to_owned();
    let path = fields.next().expect("request path").to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    HttpRequest {
        method,
        path,
        headers,
        body: bytes[body_start..body_start + content_length].to_vec(),
    }
}

fn write_http_response(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/vnd.loom.registry+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .expect("write registry response headers");
    stream.write_all(body).expect("write registry response");
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
fn backend_switch_reuses_the_same_checked_mir_identity() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");
    let interpreter = loomc()
        .args(["--json", "--backend", "interpreter", "check"])
        .arg(&project.0)
        .output()
        .expect("check through interpreter frontend");
    assert_eq!(interpreter.status.code(), Some(0));
    assert_eq!(
        cache_status(&interpreter.stdout, "checked_mir").as_deref(),
        Some("miss")
    );

    let llvm = loomc()
        .args(["--json", "--backend", "llvm", "check"])
        .arg(&project.0)
        .output()
        .expect("check through LLVM frontend");
    assert_eq!(llvm.status.code(), Some(0));
    assert_eq!(
        cache_status(&llvm.stdout, "checked_mir").as_deref(),
        Some("hit")
    );
    assert_eq!(
        cache_key(&interpreter.stdout, "checked_mir"),
        cache_key(&llvm.stdout, "checked_mir")
    );
}

#[test]
fn target_and_optimization_split_object_cache_but_reuse_checked_mir() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");
    let host_triple = loom_codegen_llvm::target_identity(
        None,
        loom_codegen_llvm::OptimizationProfile::Development,
    )
    .expect("host target identity")
    .triple;
    let build = |name: &str, target: Option<&str>, release: bool| {
        let mut command = loomc();
        command.arg("--json");
        if release {
            command.arg("--release");
        }
        if let Some(target) = target {
            command.args(["--target-triple", target]);
        }
        command
            .args(["build", "--emit", "object", "--output"])
            .arg(project.0.join(name))
            .arg(&project.0)
            .output()
            .expect("build cached native object")
    };

    let implicit_development = build("implicit-development.o", None, false);
    assert_eq!(implicit_development.status.code(), Some(0));
    assert_eq!(
        cache_status(&implicit_development.stdout, "checked_mir").as_deref(),
        Some("miss")
    );
    assert_eq!(
        cache_status(&implicit_development.stdout, "target_object").as_deref(),
        Some("miss")
    );

    let explicit_development = build("explicit-development.o", Some(&host_triple), false);
    assert_eq!(explicit_development.status.code(), Some(0));
    assert_eq!(
        cache_status(&explicit_development.stdout, "checked_mir").as_deref(),
        Some("hit")
    );
    assert_eq!(
        cache_key(&implicit_development.stdout, "checked_mir"),
        cache_key(&explicit_development.stdout, "checked_mir")
    );
    assert_eq!(
        cache_status(&explicit_development.stdout, "target_object").as_deref(),
        Some("miss")
    );
    assert_ne!(
        cache_key(&implicit_development.stdout, "target_object"),
        cache_key(&explicit_development.stdout, "target_object"),
        "implicit host tuning and an explicit generic host target need distinct objects"
    );

    let repeated_development = build("explicit-development-copy.o", Some(&host_triple), false);
    assert_eq!(repeated_development.status.code(), Some(0));
    assert_eq!(
        cache_status(&repeated_development.stdout, "checked_mir").as_deref(),
        Some("hit")
    );
    assert_eq!(
        cache_status(&repeated_development.stdout, "target_object").as_deref(),
        Some("hit")
    );
    assert_eq!(
        cache_key(&explicit_development.stdout, "target_object"),
        cache_key(&repeated_development.stdout, "target_object")
    );

    let explicit_release = build("explicit-release.o", Some(&host_triple), true);
    assert_eq!(explicit_release.status.code(), Some(0));
    assert_eq!(
        cache_status(&explicit_release.stdout, "checked_mir").as_deref(),
        Some("hit")
    );
    assert_eq!(
        cache_key(&explicit_development.stdout, "checked_mir"),
        cache_key(&explicit_release.stdout, "checked_mir")
    );
    assert_eq!(
        cache_status(&explicit_release.stdout, "target_object").as_deref(),
        Some("miss")
    );
    assert_ne!(
        cache_key(&explicit_development.stdout, "target_object"),
        cache_key(&explicit_release.stdout, "target_object")
    );

    let repeated_release = build("explicit-release-copy.o", Some(&host_triple), true);
    assert_eq!(repeated_release.status.code(), Some(0));
    assert_eq!(
        cache_status(&repeated_release.stdout, "target_object").as_deref(),
        Some("hit")
    );
    assert_eq!(
        cache_key(&explicit_release.stdout, "target_object"),
        cache_key(&repeated_release.stdout, "target_object")
    );
}

#[test]
fn ordinary_native_commands_use_the_atomic_automatic_route() {
    let scalar = TestProject::new(
        "module automatic_scalar\n\nfn choose(flag Bool) Int { if flag { 1 } else { 2 } }\n\npub fn main() Unit {\n    discard choose(true)\n    Unit\n}\n\ntest fn scalar() Unit {\n    discard choose(false)\n    Unit\n}\n",
    );
    let scalar_object = scalar.0.join("scalar.o");
    let build = loomc()
        .args(["build", "--emit", "object", "--output"])
        .arg(&scalar_object)
        .arg(&scalar.0)
        .output()
        .expect("build scalar object through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(&scalar_object).expect("read scalar object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    assert!(!contains_bytes(&object, b"loom.fn."));

    let run = loomc()
        .arg("run")
        .arg(&scalar.0)
        .output()
        .expect("run typed artifact through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
    let tests = loomc()
        .arg("test")
        .arg(&scalar.0)
        .output()
        .expect("run scalar tests through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");

    let unsupported = TestProject::new(
        "module automatic_legacy\n\npub fn main() Unit {\n    discard \"legacy\"\n    Unit\n}\n",
    );
    let legacy_object = unsupported.0.join("legacy.o");
    let build = loomc()
        .args(["build", "--emit", "object", "--output"])
        .arg(&legacy_object)
        .arg(&unsupported.0)
        .output()
        .expect("build unsupported object through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(&legacy_object).expect("read legacy object");
    assert!(contains_bytes(&object, b"loom.fn."));
    assert!(!contains_bytes(&object, b"loom.lcir.fn"));
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
fn unreachable_private_body_edits_reuse_native_object_and_relink() {
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
    assert_eq!(
        cache_status(&first.stdout, "final_artifact").as_deref(),
        Some("disabled")
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
        .expect("reuse DCE-safe target object");
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(
        cache_status(&second.stdout, "checked_mir").as_deref(),
        Some("miss")
    );
    assert_eq!(
        cache_status(&second.stdout, "final_artifact").as_deref(),
        Some("disabled")
    );
    assert_eq!(
        cache_status(&second.stdout, "target_object").as_deref(),
        Some("hit")
    );
    assert!(first_artifact.is_file());
    assert!(second_artifact.is_file());

    let third = loomc()
        .args(["--json", "build", "--output"])
        .arg(project.0.join("third.native"))
        .arg(&project.0)
        .output()
        .expect("relink cached target object");
    assert_eq!(third.status.code(), Some(0));
    assert_eq!(
        cache_status(&third.stdout, "final_artifact").as_deref(),
        Some("disabled")
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
        Some("disabled")
    );
    assert_eq!(
        cache_status(&build.stdout, "target_object").as_deref(),
        Some("hit"),
        "the preceding source run and build share a target object key"
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
#[allow(clippy::too_many_lines)]
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
#[allow(clippy::too_many_lines)]
fn loopback_http_registry_publish_fetch_lock_and_offline_cache_close_the_loop() {
    let fixture = RegistryFixture::spawn(5);
    let project = TestProject::empty();
    project.write(
        "plaintext-token/loom.toml",
        &format!(
            "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"utility\"\nversion = \"1.2.0\"\n[registries]\nremote = {{ url = {:?}, token-env = \"LOOM_TEST_REGISTRY_TOKEN\" }}\n",
            fixture.url
        ),
    );
    project.write(
        "plaintext-token/src/lib.loom",
        "module utility\n\npub fn answer() Int { 42 }\n",
    );
    let plaintext_token = loomc()
        .args(["--json", "publish", "--registry", "remote"])
        .arg(project.0.join("plaintext-token"))
        .env("LOOM_TEST_REGISTRY_TOKEN", "fixture-token")
        .output()
        .expect("reject a token over plaintext HTTP");
    assert_eq!(plaintext_token.status.code(), Some(2));
    let plaintext_output = String::from_utf8_lossy(&plaintext_token.stdout);
    assert!(
        plaintext_output.contains("tokens require HTTPS"),
        "{plaintext_output}"
    );
    assert!(
        !plaintext_output.contains("fixture-token"),
        "{plaintext_output}"
    );

    project.write(
        "producer/loom.toml",
        &format!(
            "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"utility\"\nversion = \"1.2.0\"\n[registries]\nremote = {{ url = {:?} }}\n",
            fixture.url
        ),
    );
    project.write(
        "producer/src/lib.loom",
        "module utility\n\npub fn answer() Int { 42 }\n",
    );
    let published = loomc()
        .args(["--json", "publish", "--registry", "remote"])
        .arg(project.0.join("producer"))
        .output()
        .expect("publish registry package");
    assert_eq!(
        published.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&published.stdout),
        String::from_utf8_lossy(&published.stderr)
    );
    let publish_record =
        json_record(&published.stdout, "registry_publish").expect("structured publish result");
    assert_eq!(
        publish_record.get("package"),
        Some(&serde_json::json!("utility"))
    );
    assert_eq!(
        publish_record
            .get("sha256")
            .and_then(serde_json::Value::as_str)
            .map(str::len),
        Some(64)
    );
    let published_sha256 = publish_record
        .get("sha256")
        .and_then(serde_json::Value::as_str)
        .expect("publish checksum")
        .to_owned();
    assert!(!String::from_utf8_lossy(&published.stdout).contains("fixture-token"));

    let consumer_manifest = format!(
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"consumer\"\nversion = \"1.0.0\"\n[registries]\nremote = {{ url = {:?} }}\n[dependencies]\nutility = {{ registry = \"remote\", version = \"^1\" }}\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"consumer.main\"\n",
        fixture.url
    );
    let consumer_source = "module consumer\n\nimport utility.answer\n\npub fn main() Unit {\n    let value = answer()\n    assert value == 42\n    Unit\n}\n";
    project.write("consumer/loom.toml", &consumer_manifest);
    project.write("consumer/src/main.loom", consumer_source);
    let resolved = loomc()
        .args(["--json", "resolve"])
        .arg(project.0.join("consumer"))
        .output()
        .expect("resolve HTTP dependency");
    assert_eq!(
        resolved.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&resolved.stdout),
        String::from_utf8_lossy(&resolved.stderr)
    );

    project.write("locked/loom.toml", &consumer_manifest);
    project.write("locked/src/main.loom", consumer_source);
    fs::copy(
        project.0.join("consumer/loom.lock"),
        project.0.join("locked/loom.lock"),
    )
    .expect("copy lock into cold project");
    let locked = loomc()
        .args(["--json", "--locked", "check"])
        .arg(project.0.join("locked"))
        .output()
        .expect("locked cold cache may fetch pinned package");
    assert_eq!(
        locked.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&locked.stdout),
        String::from_utf8_lossy(&locked.stderr)
    );
    let requests = fixture.finish();
    assert_eq!(requests.len(), 5, "{requests:#?}");
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(
        requests[0].headers.get("x-loom-sha256").map(String::as_str),
        Some(published_sha256.as_str())
    );
    let published_bundle =
        serde_json::from_slice::<serde_json::Value>(&requests[0].body).expect("published bundle");
    let published_paths = published_bundle
        .get("files")
        .and_then(serde_json::Value::as_array)
        .expect("published files")
        .iter()
        .map(|file| {
            file.get("path")
                .and_then(serde_json::Value::as_str)
                .expect("published file path")
        })
        .collect::<Vec<_>>();
    assert_eq!(published_paths, ["loom.toml", "src/lib.loom"]);

    let offline = loomc()
        .args([
            "--offline",
            "--backend",
            "interpreter",
            "run",
            "--target",
            "app",
        ])
        .arg(project.0.join("consumer"))
        .env_remove("LOOM_TEST_REGISTRY_TOKEN")
        .output()
        .expect("run from validated offline registry cache");
    assert_eq!(
        offline.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&offline.stdout),
        String::from_utf8_lossy(&offline.stderr)
    );

    let registry_cache_root = project.0.join("consumer/target/loom/registry/http");
    let cached_source = walk_regular_files(&registry_cache_root)
        .into_iter()
        .find(|path| path.ends_with("src/lib.loom"))
        .expect("materialized cached source");
    fs::write(&cached_source, "module utility\n").expect("tamper registry cache source");
    let tampered = loomc()
        .args(["--json", "--offline", "check"])
        .arg(project.0.join("consumer"))
        .output()
        .expect("reject a cache whose materialized source was changed");
    assert_eq!(tampered.status.code(), Some(2));
    let tampered_output = String::from_utf8_lossy(&tampered.stdout);
    assert!(
        tampered_output.contains("materialized files do not match"),
        "{tampered_output}"
    );

    project.write("cold/loom.toml", &consumer_manifest);
    project.write("cold/src/main.loom", consumer_source);
    let cold = loomc()
        .args(["--json", "--offline", "check"])
        .arg(project.0.join("cold"))
        .env_remove("LOOM_TEST_REGISTRY_TOKEN")
        .output()
        .expect("reject offline cache miss");
    assert_eq!(cold.status.code(), Some(2));
    let cold_record = json_record(&cold.stdout, "tool_error").expect("offline miss record");
    assert_eq!(
        cold_record.get("code"),
        Some(&serde_json::json!("OfflineRegistryMiss"))
    );

    let lock = fs::read(project.0.join("consumer/loom.lock")).expect("read network lock");
    let registry_cache = fs::read_dir(&registry_cache_root)
        .expect("registry cache exists")
        .flat_map(|entry| {
            let path = entry.expect("registry identity").path();
            walk_regular_files(&path)
        })
        .flat_map(|path| fs::read(path).expect("read registry cache file"))
        .collect::<Vec<_>>();
    assert!(!String::from_utf8_lossy(&lock).contains("fixture-token"));
    assert!(!String::from_utf8_lossy(&registry_cache).contains("fixture-token"));
}

fn walk_regular_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).expect("walk test tree") {
            let entry = entry.expect("walk test entry");
            if entry.file_type().expect("test entry type").is_dir() {
                pending.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    files
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
fn explicit_discard_closes_both_backend_cli_loops() {
    let project = TestProject::new(
        r"module discard_cli

fn answer() Int {
    42
}

async fn asynchronous_answer() Int {
    Task.sleep(1).await
    answer()
}

pub fn main() {
    discard answer()
}

test async fn discards_awaited_value() {
    discard asynchronous_answer().await
}
",
    );

    for backend in ["interpreter", "llvm"] {
        for command in ["check", "test", "run"] {
            let output = loomc()
                .args(["--no-cache", "--backend", backend, command])
                .arg(&project.0)
                .output()
                .expect("execute discard CLI command");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{backend} {command}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if command == "run" {
                assert_eq!(output.stdout, b"Unit\n", "{backend} run");
            }
        }

        let artifact = project.0.join(format!("discard-{backend}.artifact"));
        let build = loomc()
            .args(["--no-cache", "--backend", backend, "build", "--output"])
            .arg(&artifact)
            .arg(&project.0)
            .output()
            .expect("build discard artifact");
        assert_eq!(
            build.status.code(),
            Some(0),
            "{backend} build: stdout={} stderr={}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );

        let output = loomc()
            .args(["--backend", backend, "run", "--artifact"])
            .arg(&artifact)
            .output()
            .expect("run discard artifact");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{backend} artifact: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"Unit\n", "{backend} artifact run");
    }
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
        "#!/bin/sh\nexecutable=$1\nshift\ntest -x \"$executable\" || exit 91\ntest \"$1\" = \"--\" || exit 92\nshift\ntest \"$1\" = \"alpha\" || exit 93\ntest \"$2\" = \"beta gamma\" || exit 94\ncp \"$executable\" \"$LOOM_DEBUG_COPY\" || exit 95\nprintf 'debug-wrapper:%s:%s\\n' \"$1\" \"$2\"\n\"$executable\" \"$@\"\n",
    );
    project.make_executable("debug-wrapper");
    let debug_copy = project.0.join("debug-program-copy");
    let output = loomc()
        .env("LOOM_DEBUG_COPY", &debug_copy)
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
    let debug_image = fs::read(debug_copy).expect("debug wrapper copied executable");
    assert!(!contains_bytes(&debug_image, b"loom.fn."));
    assert!(contains_bytes(&debug_image, b"loom.lcir.fn"));
    assert!(contains_bytes(&debug_image, b"main.loom"));
}

#[cfg(unix)]
#[test]
fn debug_routes_one_reachable_unsupported_artifact_through_legacy_codegen() {
    let project = TestProject::new(
        "module debug_legacy\n\npub fn main() Unit {\n    discard \"legacy\"\n    Unit\n}\n",
    );
    project.write(
        "debug-wrapper",
        "#!/bin/sh\nexecutable=$1\nshift\ntest -x \"$executable\" || exit 91\ntest \"$1\" = \"--\" || exit 92\nshift\ncp \"$executable\" \"$LOOM_DEBUG_COPY\" || exit 93\n\"$executable\" \"$@\"\n",
    );
    project.make_executable("debug-wrapper");
    let debug_copy = project.0.join("legacy-debug-program-copy");
    let output = loomc()
        .env("LOOM_DEBUG_COPY", &debug_copy)
        .args(["debug", "--debugger"])
        .arg(project.0.join("debug-wrapper"))
        .arg(&project.0)
        .args(["--"])
        .output()
        .expect("launch debugger wrapper for reachable Text");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Unit"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let debug_image = fs::read(debug_copy).expect("debug wrapper copied legacy executable");
    assert!(contains_bytes(&debug_image, b"loom.fn."));
    assert!(!contains_bytes(&debug_image, b"loom.lcir.fn"));
    assert!(contains_bytes(&debug_image, b"main.loom"));
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
fn native_target_preparation_errors_are_usage_errors() {
    let project = TestProject::new("module invalid_target\n\npub fn main() Unit { Unit }\n");
    let output = loomc()
        .args([
            "--json",
            "--target-triple",
            "not-a-real-loom-target",
            "build",
            "--emit",
            "object",
            "--output",
        ])
        .arg(project.0.join("invalid.o"))
        .arg(&project.0)
        .output()
        .expect("reject unavailable LLVM target");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let error = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(error.contains("LlvmTargetUnavailable"), "{error}");
}

#[cfg(unix)]
#[test]
fn packed_host_runtime_bundle_builds_and_target_mismatch_fails_closed() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");
    let bundle = project.0.join("host-runtime");
    let packed = loomc()
        .args(["--json", "runtime", "pack", "--archive"])
        .arg(test_runtime_archive())
        .arg("--output")
        .arg(&bundle)
        .output()
        .expect("pack host runtime bundle");
    assert_eq!(
        packed.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&packed.stdout),
        String::from_utf8_lossy(&packed.stderr)
    );
    let export_record =
        json_record(&packed.stdout, "runtime_bundle_pack").expect("runtime pack record");
    assert_eq!(
        export_record
            .get("archive_sha256")
            .and_then(serde_json::Value::as_str)
            .map(str::len),
        Some(64)
    );
    assert_eq!(
        export_record.get("runtime_cpu"),
        Some(&serde_json::json!(loom_codegen_llvm::RUNTIME_CPU))
    );
    assert_eq!(
        export_record.get("runtime_cpu_features"),
        Some(&serde_json::json!(loom_codegen_llvm::RUNTIME_CPU_FEATURES))
    );

    let output = project.0.join("host-bundle-program");
    let built = loomc()
        .args(["--json", "--runtime-bundle"])
        .arg(&bundle)
        .arg("--linker")
        .arg(std::env::var_os("LOOM_CC").unwrap_or_else(|| "clang".into()))
        .args(["build", "--output"])
        .arg(&output)
        .arg(&project.0)
        .output()
        .expect("link host bundle with explicit linker");
    assert_eq!(
        built.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    assert!(
        Command::new(native_executable(&output))
            .output()
            .expect("run host bundle executable")
            .status
            .success()
    );

    let mismatch = loomc()
        .args([
            "--json",
            "--target-triple",
            CROSS_TRIPLE,
            "--runtime-bundle",
        ])
        .arg(&bundle)
        .arg("--linker")
        .arg(std::env::var_os("LOOM_CC").unwrap_or_else(|| "clang".into()))
        .args(["build", "--output"])
        .arg(project.0.join("mismatched-target"))
        .arg(&project.0)
        .output()
        .expect("reject host runtime for foreign object");
    assert_eq!(mismatch.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&mismatch.stdout).contains("RuntimeBundleTargetMismatch"));

    let missing_output = loomc()
        .args(["runtime", "pack", "--archive"])
        .arg(test_runtime_archive())
        .output()
        .expect("reject runtime pack without an output");
    assert_eq!(missing_output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing_output.stderr).contains("requires --output DIR"));

    let removed_export = loomc()
        .args(["runtime", "export"])
        .output()
        .expect("reject removed runtime export command");
    assert_eq!(removed_export.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&removed_export.stderr).contains("unknown runtime operation"));
}

#[cfg(unix)]
#[test]
fn host_linker_resolution_uses_environment_and_prefers_explicit_cli() {
    let project = TestProject::new("module demo\n\npub fn main() Unit { Unit }\n");
    project.write(
        "passthrough-linker",
        r#"#!/bin/sh
set -eu
if [ "${1-}" = "--version" ]; then
    printf 'loom passthrough linker v1\n'
    exit 0
fi
printf '%s\n' "$@" > "${LOOM_FAKE_LINK_LOG:?}"
exec "${LOOM_REAL_LINKER:?}" "$@"
"#,
    );
    project.make_executable("passthrough-linker");
    let linker = project.0.join("passthrough-linker");
    let link_log = project.0.join("link-arguments");
    let real_linker = std::env::var_os("LOOM_CC").unwrap_or_else(|| "clang".into());

    let environment_output = project.0.join("environment-linker");
    let environment = loomc()
        .args(["--json", "build", "--output"])
        .arg(&environment_output)
        .arg(&project.0)
        .env("LOOM_CC", &linker)
        .env("LOOM_FAKE_LINK_LOG", &link_log)
        .env("LOOM_REAL_LINKER", &real_linker)
        .output()
        .expect("link with LOOM_CC");
    assert_eq!(environment.status.code(), Some(0), "{environment:?}");
    assert!(environment_output.is_file());

    let explicit_output = project.0.join("explicit-linker");
    let explicit = loomc()
        .arg("--linker")
        .arg(&linker)
        .args(["--json", "build", "--output"])
        .arg(&explicit_output)
        .arg(&project.0)
        .env("LOOM_CC", project.0.join("unusable-environment-linker"))
        .env("LOOM_FAKE_LINK_LOG", &link_log)
        .env("LOOM_REAL_LINKER", &real_linker)
        .output()
        .expect("link with explicit --linker");
    assert_eq!(explicit.status.code(), Some(0), "{explicit:?}");
    assert!(explicit_output.is_file());
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn foreign_runtime_bundle_relinks_when_undeclared_tool_inputs_change() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");
    let bundle_one = project.0.join("runtime-one");
    write_fake_runtime_bundle(&bundle_one, b"foreign runtime archive one");
    write_fake_linker(&project, "linker identity one");
    let linker = project.0.join("fake-linker");
    let link_log = project.0.join("link-arguments");
    let object_copy = project.0.join("linked-target-object");
    let link_payload = project.0.join("link-payload");
    fs::write(&link_payload, b"payload one\n").expect("first linker payload");

    let build = |bundle: &std::path::Path, output: &std::path::Path| {
        loomc()
            .args([
                "--json",
                "--target-triple",
                CROSS_TRIPLE,
                "--runtime-bundle",
            ])
            .arg(bundle)
            .arg("--linker")
            .arg(&linker)
            .args(["build", "--output"])
            .arg(output)
            .arg(&project.0)
            .env("LOOM_FAKE_LINK_LOG", &link_log)
            .env("LOOM_FAKE_OBJECT_COPY", &object_copy)
            .env("LOOM_FAKE_LINK_PAYLOAD", &link_payload)
            .output()
            .expect("cross link with fake linker")
    };

    let first_output = project.0.join("foreign-one");
    let first = build(&bundle_one, &first_output);
    assert_eq!(
        first.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        cache_status(&first.stdout, "final_artifact").as_deref(),
        Some("disabled")
    );
    assert!(
        fs::read(&object_copy)
            .expect("cross object copy")
            .starts_with(b"\x7fELF")
    );
    let arguments = fs::read_to_string(&link_log).expect("deterministic linker arguments");
    let arguments = arguments.lines().collect::<Vec<_>>();
    assert_eq!(arguments.len(), 9, "{arguments:#?}");
    assert!(arguments[0].ends_with("loom-target.o"), "{arguments:#?}");
    assert_eq!(
        arguments[1],
        fs::canonicalize(bundle_one.join("runtime.a"))
            .expect("canonical runtime archive")
            .to_string_lossy()
    );
    assert_eq!(
        &arguments[2..8],
        ["-ldl", "-lpthread", "-lm", "-lrt", "-lutil", "-o"]
    );
    let staged_output = std::path::Path::new(arguments[8]);
    assert_eq!(staged_output.parent(), first_output.parent());
    assert!(
        staged_output
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".loom-link-")),
        "{arguments:#?}"
    );
    assert!(
        !staged_output.exists(),
        "staging path must be atomically moved"
    );
    assert!(
        first_output.exists(),
        "requested output must be materialized"
    );
    assert!(
        fs::read_to_string(&first_output)
            .expect("first linked output")
            .contains("payload one")
    );

    fs::write(&link_payload, b"payload two\n").expect("changed linker payload");
    let relinked = build(&bundle_one, &project.0.join("foreign-relinked"));
    assert_eq!(relinked.status.code(), Some(0));
    assert_eq!(
        cache_status(&relinked.stdout, "final_artifact").as_deref(),
        Some("disabled")
    );
    assert_eq!(
        cache_status(&relinked.stdout, "target_object").as_deref(),
        Some("hit")
    );
    assert!(
        fs::read_to_string(project.0.join("foreign-relinked"))
            .expect("relinked output")
            .contains("payload two"),
        "an undeclared linker input must not be hidden by a final-artifact hit"
    );

    write_fake_linker(&project, "linker identity two");
    let changed_linker = build(&bundle_one, &project.0.join("foreign-linker-two"));
    assert_eq!(changed_linker.status.code(), Some(0));
    assert_eq!(
        cache_status(&changed_linker.stdout, "final_artifact").as_deref(),
        Some("disabled")
    );

    let bundle_two = project.0.join("runtime-two");
    write_fake_runtime_bundle(&bundle_two, b"foreign runtime archive two");
    let changed_runtime = build(&bundle_two, &project.0.join("foreign-runtime-two"));
    assert_eq!(changed_runtime.status.code(), Some(0));
    assert_eq!(
        cache_status(&changed_runtime.stdout, "final_artifact").as_deref(),
        Some("disabled")
    );
}

#[test]
fn release_build_produces_a_runnable_native_executable() {
    let project = TestProject::new("module demo\n\npub fn main() Unit {\n    Unit\n}\n");
    let release_output = project.0.join("release-native");
    let native = loomc()
        .args(["--release", "build", "--output"])
        .arg(&release_output)
        .arg(&project.0)
        .output()
        .expect("build release native executable");
    assert_eq!(native.status.code(), Some(0));
    let executed = loomc()
        .args(["run", "--artifact"])
        .arg(native_executable(&release_output))
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
fn run_rejects_pre_raw_wait_removal_artifact_version() {
    let project = TestProject::new("module demo\n");
    let artifact = project.0.join("old.loomi");
    fs::write(
        &artifact,
        br#"{"format":"loom.interpreted-mir","version":17,"program":{},"floatBits":[]}"#,
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
