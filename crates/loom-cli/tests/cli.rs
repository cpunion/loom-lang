use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use sha2::{Digest as _, Sha256};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
static TEST_RUNTIME: OnceLock<PathBuf> = OnceLock::new();

macro_rules! fixture_project {
    ($name:literal) => {{
        let project = TestProject::new(include_str!(concat!(
            "../../../fixtures/",
            $name,
            "/main.loom"
        )));
        project.write(
            "main_test.loom",
            include_str!(concat!("../../../fixtures/", $name, "/main_test.loom")),
        );
        project
    }};
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

fn loom() -> Command {
    let runtime = test_runtime_bundle_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_loom"));
    command.env("LOOM_RUNTIME_BUNDLE", runtime);
    command
}

fn loom_without_test_runtime() -> Command {
    Command::new(env!("CARGO_BIN_EXE_loom"))
}

fn run_git(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git fixture output is UTF-8")
        .trim()
        .to_owned()
}

fn commit_git_fixture(repository: &Path, message: &str) -> String {
    run_git(repository, &["add", "."]);
    run_git(
        repository,
        &[
            "-c",
            "user.name=Loom Tests",
            "-c",
            "user.email=loom-tests@example.invalid",
            "commit",
            "-m",
            message,
        ],
    );
    run_git(repository, &["rev-parse", "HEAD"])
}

fn git_file_url(repository: &Path) -> String {
    let path = fs::canonicalize(repository)
        .expect("canonical git fixture")
        .to_string_lossy()
        .replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn assert_git_lock_pin(
    lock: &str,
    module: &str,
    git_url: &str,
    selector: &str,
    commit: &str,
) -> String {
    let lock = toml::from_str::<toml::Value>(lock).expect("parse generated git lockfile");
    let locked = lock
        .get("module")
        .and_then(toml::Value::as_array)
        .expect("lockfile module array")
        .iter()
        .find(|entry| entry.get("name").and_then(toml::Value::as_str) == Some(module))
        .unwrap_or_else(|| panic!("lockfile contains module {module}"));
    let source = locked
        .get("source")
        .and_then(toml::Value::as_str)
        .expect("git lock source");
    assert_eq!(source, format!("git+{git_url}#{commit}"));
    assert_eq!(
        locked.get("selector").and_then(toml::Value::as_str),
        Some(selector)
    );
    let checksum = locked
        .get("checksum")
        .and_then(toml::Value::as_str)
        .expect("git lock checksum");
    assert_eq!(checksum.len(), 64, "{locked:#?}");
    assert!(
        checksum.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{locked:#?}"
    );
    checksum.to_owned()
}

#[cfg(unix)]
fn test_runtime_archive() -> PathBuf {
    let target = loom_codegen_llvm::native_target_identity().expect("load host target identity");
    loom_codegen_llvm::RuntimeBundle::load(test_runtime_bundle_root(), &target)
        .expect("load explicit CLI test runtime bundle")
        .archive()
        .to_path_buf()
}

fn test_runtime_bundle_root() -> &'static PathBuf {
    TEST_RUNTIME.get_or_init(|| {
        let root = std::env::var_os("LOOM_TEST_RUNTIME_BUNDLE")
            .or_else(|| std::env::var_os("LOOM_RUNTIME_BUNDLE"))
            .map_or_else(
                || {
                    panic!(
                        "native CLI tests require LOOM_TEST_RUNTIME_BUNDLE or \
                     LOOM_RUNTIME_BUNDLE; prepare one with `loom runtime pack`"
                    )
                },
                PathBuf::from,
            );
        let target =
            loom_codegen_llvm::native_target_identity().expect("load host target identity");
        loom_codegen_llvm::RuntimeBundle::load(&root, &target)
            .expect("validate explicit CLI test runtime bundle");
        root
    })
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
runtime_copy=${{LOOM_FAKE_RUNTIME_COPY:?}}
payload=${{LOOM_FAKE_LINK_PAYLOAD:?}}
: > "$log"
cp "$1" "$object_copy"
cp "$2" "$runtime_copy"
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
    let output = loom()
        .args(["--json", "check"])
        .arg(&project.0)
        .output()
        .expect("run loom");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("UnexpectedEndOfFile"), "{stdout}");
    assert!(!stdout.contains("CompilerPipelineIncomplete"), "{stdout}");
}

#[test]
fn human_code_frames_do_not_change_the_json_diagnostic_contract() {
    let project = TestProject::new("pub fn main() {\n    missing()\n}\n");
    let human = loom()
        .arg("check")
        .arg(&project.0)
        .output()
        .expect("run human check");
    assert_eq!(human.status.code(), Some(1));
    let stderr = String::from_utf8(human.stderr).expect("UTF-8 human diagnostics");
    assert!(stderr.contains("2 |     missing()"), "{stderr}");
    assert!(stderr.contains('^'), "{stderr}");

    let machine = loom()
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
    let project = TestProject::new("pub fn main() {\n    assert false\n}\n");
    let failures = ["interpreter", "llvm"].map(|backend| {
        let output = loom()
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
        "schema = 2\nlanguage = \"0.4\"\n[module]\nname = \"future\"\nversion = \"1.0.0\"\n",
    );
    project.write("main.loom", "");
    let output = loom()
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
    let project = TestProject::new("fn main() {\n}\n");
    let output = loom()
        .args(["check", "--json"])
        .arg(&project.0)
        .output()
        .expect("run loom");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("\"status\":\"ok\""), "{stdout}");
    assert!(!stdout.contains("CompilerPipelineIncomplete"), "{stdout}");
}

#[test]
fn embedded_loom_std_source_checks_builds_and_closes_only_reachable_definitions() {
    let project = TestProject::new(
        r"import std.int.minimum

pub fn main() {
    let value = minimum(9, 4)
    assert value == 4
}
",
    );

    for command in ["check", "build"] {
        let output = loom_without_test_runtime()
            .args(["--backend", "interpreter", "--no-cache", command])
            .arg(&project.0)
            .output()
            .expect("run std-source command");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{command}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let snapshot = loom_driver::AnalysisHost::new(&project.0)
        .expect("open embedded-std client")
        .snapshot()
        .expect("compile embedded-std client");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("checked MIR");
    let minimum = program
        .functions
        .iter()
        .find(|function| function.name == "std.int.minimum")
        .expect("minimum source definition")
        .id;
    let maximum = program
        .functions
        .iter()
        .find(|function| function.name == "std.int.maximum")
        .expect("maximum source definition")
        .id;
    let parse_int = program
        .functions
        .iter()
        .find(|function| function.name == "std.int.parse_int")
        .expect("parse_int source definition")
        .id;
    let roots = loom_codegen_ir::SourceRoots::for_entry(program, "main").expect("main root");
    let reachable = loom_codegen_ir::analyze_source_reachability(program, &roots)
        .expect("close source call graph");
    assert!(reachable.functions.contains(&minimum));
    assert!(!reachable.functions.contains(&maximum));
    assert!(!reachable.functions.contains(&parse_int));
    assert_eq!(reachable.functions.len(), 2);
}

#[test]
#[allow(clippy::too_many_lines)]
fn interpreted_executable_artifact_closes_to_one_entry_without_narrowing_driver_cache() {
    const INITIAL_SOURCE: &str = r"fn mainHelper() Int { 1 }

fn alternateHelper() Int { 2 }

pub fn main() {
    let value = mainHelper()
    assert value == 1
}

pub fn alternate() {
    let value = alternateHelper()
    assert value == 2
}

record DeadRecord {
    value Int
}

fn dead() DeadRecord {
    DeadRecord { value = 10 }
}
";
    const CHANGED_DEAD_SOURCE: &str = r"fn mainHelper() Int { 1 }

fn alternateHelper() Int { 2 }

pub fn main() {
    let value = mainHelper()
    assert value == 1
}

pub fn alternate() {
    let value = alternateHelper()
    assert value == 2
}

record DeadRecord {
    value Int
}

fn dead() DeadRecord {
    DeadRecord { value = 20 }
}
";

    let project = TestProject::new(INITIAL_SOURCE);
    let main_artifact = project.0.join("main.loomi");
    let build = |entry: &str, artifact: &std::path::Path, no_cache: bool| {
        let mut command = loom_without_test_runtime();
        command.args(["--json", "--backend", "interpreter"]);
        if no_cache {
            command.arg("--no-cache");
        }
        command
            .args(["build", "--entry", entry, "--output"])
            .arg(artifact)
            .arg(&project.0)
            .output()
            .expect("build interpreted executable closure")
    };

    let first = build("main", &main_artifact, false);
    assert_eq!(
        first.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        cache_status(&first.stdout, "checked_mir").as_deref(),
        Some("miss")
    );
    let main_bytes = fs::read(&main_artifact).expect("read main artifact");
    let (main_program, main_entry) = loom_mir::decode_interpreted_executable_artifact(&main_bytes)
        .expect("decode closed main artifact");
    assert_eq!(main_entry, "main");
    assert_eq!(
        main_program
            .exports
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["main"]
    );
    let mut main_functions = main_program
        .functions
        .iter()
        .map(|function| function.name.as_str())
        .collect::<Vec<_>>();
    main_functions.sort_unstable();
    assert_eq!(main_functions, ["standalone.main", "standalone.mainHelper"]);
    assert!(
        !main_program
            .types
            .iter()
            .any(|definition| definition.name == "DeadRecord")
    );
    assert!(main_program.tests.is_empty());

    let alternate_artifact = project.0.join("alternate.loomi");
    let alternate = build("alternate", &alternate_artifact, false);
    assert_eq!(
        alternate.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&alternate.stdout),
        String::from_utf8_lossy(&alternate.stderr)
    );
    assert_eq!(
        cache_status(&alternate.stdout, "checked_mir").as_deref(),
        Some("hit"),
        "the generic driver cache must retain exports outside the first executable closure"
    );
    let alternate_bytes = fs::read(&alternate_artifact).expect("read alternate artifact");
    let (alternate_program, alternate_entry) =
        loom_mir::decode_interpreted_executable_artifact(&alternate_bytes)
            .expect("decode closed alternate artifact");
    assert_eq!(alternate_entry, "alternate");
    assert_eq!(
        alternate_program
            .exports
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["alternate"]
    );
    let mut alternate_functions = alternate_program
        .functions
        .iter()
        .map(|function| function.name.as_str())
        .collect::<Vec<_>>();
    alternate_functions.sort_unstable();
    assert_eq!(
        alternate_functions,
        ["standalone.alternate", "standalone.alternateHelper"]
    );

    project.write("main.loom", CHANGED_DEAD_SOURCE);
    let changed_artifact = project.0.join("main-after-dead-edit.loomi");
    let changed = build("main", &changed_artifact, true);
    assert_eq!(
        changed.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&changed.stdout),
        String::from_utf8_lossy(&changed.stderr)
    );
    assert_eq!(
        fs::read(changed_artifact).expect("read artifact after dead edit"),
        main_bytes,
        "an unreachable definition edit must not perturb final executable bytes"
    );

    let run = loom_without_test_runtime()
        .args(["--backend", "interpreter", "run", "--artifact"])
        .arg(main_artifact)
        .output()
        .expect("run closed interpreted executable");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn persistent_cache_hits_content_keys_and_final_artifacts() {
    let project = TestProject::new("pub fn main() {\n}\n");

    let check = |extra: &[&str]| {
        loom()
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

    project.write("main.loom", "pub fn main() {\n    assert true\n}\n");
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
    let first_build = loom()
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
    let second_build = loom()
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
    let project = TestProject::new("pub fn main() {\n}\n");
    let interpreter = loom()
        .args(["--json", "--backend", "interpreter", "check"])
        .arg(&project.0)
        .output()
        .expect("check through interpreter frontend");
    assert_eq!(interpreter.status.code(), Some(0));
    assert_eq!(
        cache_status(&interpreter.stdout, "checked_mir").as_deref(),
        Some("miss")
    );

    let llvm = loom()
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
    let project = TestProject::new("pub fn main() {\n}\n");
    let host_triple = loom_codegen_llvm::target_identity(
        None,
        loom_codegen_llvm::OptimizationProfile::Development,
    )
    .expect("host target identity")
    .triple;
    let build = |name: &str, target: Option<&str>, release: bool| {
        let mut command = loom();
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
        "fn choose(flag Bool) Int { if flag { 1 } else { 2 } }\n\npub fn main() {\n    discard choose(true)\n}\n",
    );
    scalar.write(
        "main_test.loom",
        "test fn scalar() {\n    discard choose(false)\n}\n",
    );
    let scalar_object = scalar.0.join("scalar.o");
    let build = loom()
        .args(["build", "--emit", "object", "--output"])
        .arg(&scalar_object)
        .arg(&scalar.0)
        .output()
        .expect("build scalar object through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(&scalar_object).expect("read scalar object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    assert!(!contains_bytes(&object, b"loom.fn."));

    let run = loom()
        .arg("run")
        .arg(&scalar.0)
        .output()
        .expect("run typed artifact through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
    let tests = loom()
        .arg("test")
        .arg(&scalar.0)
        .output()
        .expect("run scalar tests through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");

    let managed = TestProject::new("pub fn main() {\n    discard \"left\".concat(\"right\")\n}\n");
    let managed_object = managed.0.join("managed-text.o");
    let build = loom()
        .args(["build", "--emit", "object", "--output"])
        .arg(&managed_object)
        .arg(&managed.0)
        .output()
        .expect("build managed Text object through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(&managed_object).expect("read managed Text object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    assert!(!contains_bytes(&object, b"loom.fn."));
    assert!(contains_bytes(
        &object,
        b"loom_runtime_text_concat_typed_v1"
    ));
}

#[test]
fn async_managed_collections_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-async-managed-collections");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check managed-collection coroutine source");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("async-managed-collections.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build managed-collection coroutine source");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read managed-collection coroutine object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_gc_typed_repeated_alloc_v1",
        b"loom_typed_task_create_v1",
        b"loom_typed_task_set_root_state_v1",
        b"loom_task_suspend_join",
    ] {
        assert!(
            contains_bytes(&object, required),
            "managed-collection coroutine object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_runtime_list_add",
        b"loom_runtime_list_get",
        b"loom_runtime_text_map_insert",
        b"loom_runtime_text_map_get",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "managed-collection coroutine object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test managed-collection coroutine source");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout)
            .contains("passed standalone.managedCollectionsCrossAwait"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run managed-collection coroutine source");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn typed_sleep_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-typed-sleep");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check typed-sleep source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("typed-sleep.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build typed-sleep source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read typed-sleep object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_wait_now_ns",
        b"loom_typed_timer_task_create_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "typed-sleep object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_task_from_wait_source",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "typed-sleep object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test typed-sleep source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout)
            .contains("passed standalone.sleepsWithIntAndDuration"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run typed-sleep source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn synchronous_task_helpers_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-sync-task-helpers");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check synchronous Task-helper source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("sync-task-helpers.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build synchronous Task-helper source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read synchronous Task-helper object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_typed_task_create_v1",
        b"loom_typed_task_publish_adopting_v1",
        b"loom_typed_timer_task_create_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "synchronous Task-helper object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [b"loom.fn.".as_slice(), b"loom.Value", b"ValueNode"] {
        assert!(
            !contains_bytes(&object, forbidden),
            "synchronous Task-helper object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test synchronous Task-helper source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout)
            .contains("passed standalone.synchronousTaskHelpersBorrowTheCurrentExecutor"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run synchronous Task-helper source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn typed_task_all_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-typed-task-all");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check typed Task.all source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("typed-task-all.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build typed Task.all source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read typed Task.all object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_typed_task_publish_adopting_v1",
        b"loom_task_prepare_join",
        b"loom_task_add_join_child",
        b"loom_task_suspend_join",
        b"loom_typed_task_take_result_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "typed Task.all object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_join_create",
        b"loom_join_add_task",
        b"loom_task_write_join_result",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "typed Task.all object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test typed Task.all source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout)
            .contains("passed standalone.staticTaskAllUsesDirectAndFirstClassPaths"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run typed Task.all source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn typed_task_any_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-typed-task-any");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check typed Task.any source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("typed-task-any.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build typed Task.any source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read typed Task.any object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_task_prepare_join",
        b"loom_task_add_join_child",
        b"loom_task_suspend_join",
        b"loom_task_join_step",
        b"loom_task_join_winner",
        b"loom_typed_task_publish_adopting_v1",
        b"loom_typed_task_take_result_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "typed Task.any object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_join_create",
        b"loom_join_add_task",
        b"loom_task_write_join_result",
        b"loom_task_join_result",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "typed Task.any object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test typed Task.any source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout)
            .contains("passed standalone.fixedTaskAnySelectsTheSecondManagedWinnerRepeatedly"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run typed Task.any source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn typed_task_outcomes_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-typed-task-outcomes");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check typed Task outcome source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("typed-task-outcomes.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build typed Task outcome source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read typed Task outcome object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_task_prepare_join",
        b"loom_task_add_join_child",
        b"loom_task_suspend_join",
        b"loom_task_join_step",
        b"loom_task_join_winner",
        b"loom_typed_task_publish_adopting_v1",
        b"loom_typed_task_take_outcome_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "typed Task outcome object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_join_create",
        b"loom_join_add_task",
        b"loom_task_write_join_result",
        b"loom_task_join_result",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "typed Task outcome object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test typed Task outcome source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout)
            .contains("passed standalone.fixedSettledAndRaceUseTypedOutcomes"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run typed Task outcome source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn typed_runtime_width_task_lists_close_real_commands_and_faults() {
    let project = fixture_project!("lcir-typed-task-lists");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check runtime-width Task lists through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("typed-task-lists.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build runtime-width Task lists through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read runtime-width Task-list object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_gc_typed_repeated_alloc_v1",
        b"loom_typed_task_publish_v1",
        b"loom_typed_task_publish_adopting_v1",
        b"loom_task_prepare_join",
        b"loom_task_add_join_child",
        b"loom_task_suspend_join",
        b"loom_task_join_step",
        b"loom_task_join_winner",
        b"loom_typed_task_take_result_v1",
        b"loom_typed_task_take_outcome_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "runtime-width Task-list object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_join_create",
        b"loom_join_add_task",
        b"loom_task_write_join_result",
        b"loom_task_join_result",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "runtime-width Task-list object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test runtime-width Task lists through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout)
            .contains("passed standalone.runtimeWidthTaskListsUseTypedComposites"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run runtime-width Task lists through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");

    assert_runtime_width_task_list_faults(&project.0);
}

#[test]
fn affine_task_carriers_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-affine-task-carriers");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check affine Task carriers through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("affine-task-carriers.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build affine Task carriers through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read affine Task-carrier object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_typed_task_create_v1",
        b"loom_typed_task_take_result_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "affine Task-carrier object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [b"loom.fn.".as_slice(), b"loom.Value", b"ValueNode"] {
        assert!(
            !contains_bytes(&object, forbidden),
            "affine Task-carrier object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test affine Task carriers through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert_eq!(
        tests.stdout,
        b"passed standalone.affineTaskCarriersMoveOnce\n"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run affine Task carriers through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

fn assert_runtime_width_task_list_faults(project: &Path) {
    for (entry, code, message) in [
        (
            "emptyAny",
            "EmptyTaskJoin",
            "Task.any and Task.race require a non-empty task list",
        ),
        (
            "emptyRace",
            "EmptyTaskJoin",
            "Task.any and Task.race require a non-empty task list",
        ),
        (
            "failedAny",
            "TaskAnyFailed",
            "Task.any completed without a successful task",
        ),
    ] {
        let failure = loom()
            .args(["--json", "--no-cache", "run", "--entry", entry])
            .arg(project)
            .output()
            .expect("run a runtime-width Task-list fault entry");
        assert_eq!(failure.status.code(), Some(1), "{failure:?}");
        let record = json_record(&failure.stdout, "run_failure")
            .unwrap_or_else(|| panic!("{entry} omitted its structured run failure: {failure:?}"));
        assert_eq!(
            record.pointer("/failure/fault/code"),
            Some(&serde_json::json!(code))
        );
        assert_eq!(
            record.pointer("/failure/fault/message"),
            Some(&serde_json::json!(message))
        );
    }
}

#[test]
fn typed_async_cleanup_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-async-cleanup");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check async-cleanup source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("async-cleanup.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build async-cleanup source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read async-cleanup object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom.lcir.coroutine.resume",
        b"loom_typed_task_is_cancel_requested_v1",
        b"loom_task_join_step",
        b"loom_typed_task_take_result_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "typed async-cleanup object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_join_create",
        b"loom_task_write_join_result",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "typed async-cleanup object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test async-cleanup source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout)
            .contains("passed standalone.cleanupAcrossSuspension"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run async-cleanup source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");

    let cancellation = loom()
        .args(["--json", "--no-cache", "run", "--entry", "cancellationMain"])
        .arg(&project.0)
        .output()
        .expect("run cancellation cleanup through the production CLI");
    assert_eq!(cancellation.status.code(), Some(1), "{cancellation:?}");
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&cancellation.stdout),
        String::from_utf8_lossy(&cancellation.stderr)
    );
    assert!(diagnostics.contains("AssertionFault"), "{cancellation:?}");
    assert!(
        !diagnostics.contains("LOOM_RUNTIME_TYPED_"),
        "{cancellation:?}"
    );
}

#[test]
fn typed_async_writeback_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-async-writeback");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check async-writeback source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("async-writeback.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build async-writeback source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read async-writeback object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom.lcir.coroutine.resume",
        b"loom_gc_typed_alloc_v1",
        b"loom_task_join_step",
        b"loom_typed_task_take_result_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "typed async-writeback object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_witness_",
        b"loom_join_create",
        b"loom_task_write_join_result",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "typed async-writeback object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test async-writeback source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout)
            .contains("passed standalone.synchronousWritebackInsideCoroutine"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run async-writeback source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn generic_native_commands_close_check_build_test_and_run() {
    let project = fixture_project!("lcir-generics");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check the generic fixture through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("generics.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build the generic fixture through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(&object_path).expect("read generic object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    assert!(!contains_bytes(&object, b"loom.fn."));

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test the generic fixture through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("identityInstance"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run the generic fixture through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn generic_products_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-generic-products");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check generic-product source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("generic-products.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build generic-product source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read generic-product object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_runtime_text_concat_typed_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "generic-product object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "generic-product object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test generic-product source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.genericProducts"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run generic-product source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn scalar_std_apis_close_both_backends_and_typed_object_surface() {
    let project = fixture_project!("lcir-scalar-builtins");

    for backend in ["interpreter", "llvm"] {
        for command in ["check", "test", "run"] {
            let output = loom()
                .args(["--backend", backend, "--no-cache", command])
                .arg(&project.0)
                .output()
                .expect("run scalar std APIs through the production CLI");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{backend} {command}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if command == "test" {
                assert!(
                    String::from_utf8_lossy(&output.stdout)
                        .contains("passed standalone.typedScalarBuiltins"),
                    "{backend}: {output:?}"
                );
            } else if command == "run" {
                assert_eq!(output.stdout, b"Unit\n", "{backend} run");
            }
        }

        let artifact = project.0.join(format!("scalar-{backend}.artifact"));
        let build = loom()
            .args(["--backend", backend, "--no-cache", "build", "--output"])
            .arg(&artifact)
            .arg(&project.0)
            .output()
            .expect("build scalar std API artifact");
        assert_eq!(build.status.code(), Some(0), "{backend}: {build:?}");

        let run = loom()
            .args(["--backend", backend, "run", "--artifact"])
            .arg(&artifact)
            .output()
            .expect("run scalar std API artifact");
        assert_eq!(run.status.code(), Some(0), "{backend}: {run:?}");
        assert_eq!(run.stdout, b"Unit\n", "{backend} artifact run");
    }

    let object_path = project.0.join("scalar-builtins.o");
    let build = loom()
        .args([
            "--backend",
            "llvm",
            "--no-cache",
            "build",
            "--emit",
            "object",
            "--output",
        ])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build scalar-builtin source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read scalar-builtin object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_runtime_parse_float",
        b"loom_runtime_format_float_typed_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "scalar-builtin object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_runtime_format_float(",
        b"loom_runtime_parse_int",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "scalar-builtin object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }
}

#[test]
fn structural_equality_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-structural-equality");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check structural-equality source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("structural-equality.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build structural-equality source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read structural-equality object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_runtime_text_concat_typed_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "structural-equality object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "structural-equality object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test structural-equality source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.structuralEquality"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run structural-equality source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn static_concepts_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-static-concepts");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check static-concept source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("static-concepts.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build static-concept source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read static-concept object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_gc_",
        b"loom_executor_",
        b"loom_witness_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "static proof erasure exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test static-concept source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.staticConcepts"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run static-concept source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn unique_dynamic_concepts_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-dyn-unique");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check unique-dyn source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("unique-dyn.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build unique-dyn source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read unique-dyn object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_executor_",
        b"loom_witness_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "unique dyn object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test unique-dyn source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.uniqueDynamicWitness"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run unique-dyn source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn finite_dynamic_concepts_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-dyn-finite");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check finite-dyn source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("finite-dyn.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build finite-dyn source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read finite-dyn object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_gc_typed_alloc_v1".as_slice(),
    ] {
        assert!(
            contains_bytes(&object, required),
            "finite dyn object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_executor_",
        b"loom_witness_",
        b"WitnessInstance",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "finite dyn object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test finite-dyn source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.finiteDynamicWitnesses"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run finite-dyn source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn concepts_polymorphism_uses_the_unique_witness_lcir_route() {
    let project = TestProject::new(include_str!(
        "../../../examples/concepts-polymorphism/concepts.loom"
    ));
    project.write(
        "main_test.loom",
        include_str!("../../../examples/concepts-polymorphism/concepts_test.loom"),
    );
    let object_path = project.0.join("concepts-polymorphism-unique-dyn.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build Core02 main through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read Core02 object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_executor_",
        b"loom_witness_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "Core02 object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run Core02 main through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("run Core02 tests through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    let stdout = String::from_utf8_lossy(&tests.stdout);
    for expected in [
        "static_and_readonly_dynamic_dispatch",
        "mutable_dynamic_dispatch_writes_the_owner",
        "dynamic_interfaces_are_first_class_values",
    ] {
        assert!(stdout.contains(expected), "missing `{expected}`: {tests:?}");
    }
}

#[test]
fn lexical_cleanup_and_source_contracts_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-lexical-cleanup");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check lexical-cleanup source through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("lexical-cleanup.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build lexical-cleanup source through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read lexical-cleanup object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    assert!(contains_bytes(&object, b"loom_context_raise_fault_v1"));
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_gc_",
        b"loom_executor_",
        b"loom_io_close",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "lexical cleanup exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test lexical-cleanup source through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.lexicalCleanup"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run lexical-cleanup source through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn projected_places_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-projected-places");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check projected-place source through the CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("projected-places.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build projected-place source through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read projected-place object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    assert!(!contains_bytes(&object, b"loom.fn."));

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test projected-place source through the CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.projectedPlaces"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run projected-place source through the CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn immortal_text_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-text");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check immortal-Text source through the CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("immortal-text.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build immortal-Text source through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read immortal-Text object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    assert!(contains_bytes(&object, b"loom_runtime_text_contains"));
    assert!(contains_bytes(&object, b"loom_layout_text_v1"));
    assert!(!contains_bytes(&object, b"loom.fn."));
    assert!(!contains_bytes(&object, b"loom.Value"));
    assert!(!contains_bytes(&object, b"ValueNode"));
    assert!(!contains_bytes(&object, b"loom_gc_"));
    assert!(!contains_bytes(&object, b"loom_executor_"));

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test immortal-Text source through the CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.immortalText"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run immortal-Text source through the CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn managed_text_concat_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-managed-text");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check managed-Text source through the CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("managed-text.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build managed-Text source through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read managed-Text object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_layout_text_v1",
        b"loom_runtime_text_concat_typed_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "managed Text object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "managed Text object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test managed-Text source through the CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.managedText"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run managed-Text source through the CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn managed_product_leaves_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-managed-products");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check managed-product source through the CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("managed-products.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build managed-product source through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read managed-product object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_runtime_text_concat_typed_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "managed-product object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "managed-product object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test managed-product source through the CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.managedProducts"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run managed-product source through the CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn managed_sum_leaves_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-managed-sums");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check managed-sum source through the CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("managed-sums.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build managed-sum source through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read managed-sum object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_runtime_text_concat_typed_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "managed-sum object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "managed-sum object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test managed-sum source through the CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.managedSums"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run managed-sum source through the CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn managed_lists_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-managed-lists");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check managed-List source through the CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("managed-lists.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build managed-List source through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read managed-List object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_gc_typed_repeated_alloc_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "managed-List object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_runtime_list_add",
        b"loom_runtime_list_get",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "managed-List object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test managed-List source through the CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.managedLists"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run managed-List source through the CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn typed_text_maps_close_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-typed-textmap");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check typed TextMap source through the CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("typed-text-map.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build typed TextMap source through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read typed TextMap object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_gc_typed_repeated_alloc_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
        b"memcmp",
    ] {
        assert!(
            contains_bytes(&object, required),
            "typed TextMap object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_runtime_text_map_insert",
        b"loom_runtime_text_map_get",
        b"loom_runtime_text_map_remove",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "typed TextMap object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test typed TextMap source through the CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.typedTextMap"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run typed TextMap source through the CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn typed_logging_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-typed-logging");
    let expected = include_bytes!("../../../fixtures/lcir-typed-logging/expected.stderr");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check typed logging source through the CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");
    assert!(check.stderr.is_empty(), "{check:?}");

    let object_path = project.0.join("typed-logging.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build typed logging source through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read typed logging object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_runtime_log_typed_v1",
        b"loom_gc_typed_repeated_alloc_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "typed logging object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_runtime_log\0",
        b"loom_runtime_text_map_",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "typed logging object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test typed logging source through the CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert_eq!(tests.stdout, b"passed standalone.typedLogging\n");
    assert_eq!(tests.stderr, expected);

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run typed logging source through the CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
    assert_eq!(run.stderr, expected);
}

#[test]
fn typed_io_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-typed-io");

    let check = loom()
        .args(["--no-cache", "check", "."])
        .current_dir(&project.0)
        .output()
        .expect("check typed I/O source through the CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("typed-io.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(".")
        .current_dir(&project.0)
        .output()
        .expect("build typed I/O source through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read typed I/O object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_typed_io_task_create_v1",
        b"loom_typed_io_poll_v1",
        b"loom_typed_io_cancel_v1",
        b"loom_typed_resource_close_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "typed I/O object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"loom_file_",
        b"loom_socket_",
        b"loom_io_close",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "typed I/O object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test", "."])
        .current_dir(&project.0)
        .output()
        .expect("test typed I/O source through the CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert_eq!(tests.stdout, b"passed standalone.typedIo\n");
    assert_eq!(
        fs::read(project.0.join("round-trip.txt")).expect("read test I/O result"),
        b"direct typed I/O"
    );

    let run = loom()
        .args(["--no-cache", "run", "."])
        .current_dir(&project.0)
        .output()
        .expect("run typed I/O source through the CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
    assert_eq!(
        fs::read(project.0.join("round-trip.txt")).expect("read run I/O result"),
        b"direct typed I/O"
    );
}

#[test]
fn typed_json_format_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-json-format");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check typed JSON formatting source through the CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");
    assert!(check.stderr.is_empty(), "{check:?}");

    let object_path = project.0.join("typed-json-format.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build typed JSON formatting source through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read typed JSON formatting object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_runtime_json_format_typed_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "typed JSON formatting object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_runtime_json_format\0",
        b"loom_runtime_list_",
        b"loom_runtime_text_map_",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "typed JSON formatting object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test typed JSON formatting source through the CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert_eq!(tests.stdout, b"passed standalone.typedJsonFormat\n");
    assert!(tests.stderr.is_empty(), "{tests:?}");

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run typed JSON formatting source through the CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
    assert!(run.stderr.is_empty(), "{run:?}");
}

#[test]
fn source_json_parse_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("lcir-json-parse");

    for backend in ["interpreter", "llvm"] {
        for command in ["check", "test", "run"] {
            let output = loom()
                .args(["--backend", backend, "--no-cache", command])
                .arg(&project.0)
                .output()
                .expect("run source JSON parsing through the production CLI");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{backend} {command}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            if command == "test" {
                assert_eq!(
                    output.stdout, b"passed standalone.source_json_parse\n",
                    "{backend} test",
                );
            } else if command == "run" {
                assert_eq!(output.stdout, b"Unit\n", "{backend} run");
            }
            assert!(output.stderr.is_empty(), "{backend} {command}: {output:?}");
        }

        let artifact = project
            .0
            .join(format!("source-json-parse-{backend}.artifact"));
        let build = loom()
            .args(["--backend", backend, "--no-cache", "build", "--output"])
            .arg(&artifact)
            .arg(&project.0)
            .output()
            .expect("build source JSON parser artifact");
        assert_eq!(build.status.code(), Some(0), "{backend}: {build:?}");
        assert!(build.stderr.is_empty(), "{backend}: {build:?}");

        let run = loom()
            .args(["--backend", backend, "run", "--artifact"])
            .arg(&artifact)
            .output()
            .expect("run source JSON parser artifact");
        assert_eq!(run.status.code(), Some(0), "{backend}: {run:?}");
        assert_eq!(run.stdout, b"Unit\n", "{backend} artifact run");
        assert!(run.stderr.is_empty(), "{backend}: {run:?}");
    }

    let object_path = project.0.join("source-json-parse.o");
    let build = loom()
        .args([
            "--backend",
            "llvm",
            "--no-cache",
            "build",
            "--emit",
            "object",
            "--output",
        ])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build source JSON parsing through the CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(object_path).expect("read source JSON parser object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_runtime_parse_float",
        b"loom_gc_typed_repeated_alloc_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "source JSON parser object omitted `{}`",
            String::from_utf8_lossy(required)
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "source JSON parser object exposed `{}`",
            String::from_utf8_lossy(forbidden)
        );
    }
}

#[test]
fn cache_stat_and_prune_have_stable_json_reports() {
    let project = TestProject::new("pub fn main() {}\n");
    let check = loom()
        .args(["--json", "--backend", "interpreter", "check"])
        .arg(&project.0)
        .output()
        .expect("populate cache");
    assert_eq!(check.status.code(), Some(0));

    let stat = loom()
        .args(["--json", "cache", "stat"])
        .arg(&project.0)
        .output()
        .expect("inspect cache");
    assert_eq!(stat.status.code(), Some(0));
    let stdout = String::from_utf8(stat.stdout).expect("UTF-8 stat output");
    assert!(stdout.contains("\"category\":\"cache_stat\""), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "\"schema_version\":{}",
            loom_driver::CACHE_SCHEMA_VERSION
        )),
        "{stdout}"
    );

    let prune = loom()
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
    let project = TestProject::new("pub fn main() {\n}\n\nfn dead() Int {\n    1\n}\n");
    let first_artifact = project.0.join("first.native");
    let first = loom()
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
        "pub fn main() {\n}\n\nfn dead() Int {\n    2\n}\n",
    );
    let second_artifact = project.0.join("second.native");
    let second = loom()
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

    let third = loom()
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
#[allow(
    clippy::too_many_lines,
    reason = "one CLI scenario verifies check, run, build, test, and both artifact backends"
)]
fn manifest_targets_and_path_dependencies_drive_cli_roots() {
    let project = TestProject::empty();
    project.write(
        "utility/loom.toml",
        "schema = 2\n[module]\nname = \"utility\"\nversion = \"1.1.0\"\n",
    );
    project.write(
        "utility/math/increment.loom",
        "pub fn increment(value Int) Int {\n    value + 1\n}\n",
    );
    project.write(
        "application/loom.toml",
        "schema = 2\n[module]\nname = \"application\"\nversion = \"0.1.0\"\n[dependencies]\nutility = { path = \"../utility\", version = \"^1\" }\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"application.start\"\n",
    );
    project.write(
        "application/main.loom",
        "import utility.math.increment\n\npub fn start() {\n    let value = increment(1)\n    assert value == 2\n}\n",
    );
    project.write(
        "application/main_test.loom",
        "import utility.math.increment\n\ntest fn dependency_works() {\n    let value = increment(2)\n    assert value == 3\n}\n",
    );
    let root = project.0.join("application");

    for arguments in [
        vec!["check", "--target", "app"],
        vec!["run", "--target", "app"],
    ] {
        let output = loom()
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

    let tests = loom()
        .arg("test")
        .arg(&root)
        .output()
        .expect("run package tests without a target selector");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed application.dependency_works"),
        "{tests:?}"
    );

    let artifact = project.0.join("application.native");
    let build = loom()
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

    let artifact_run = loom()
        .args(["run", "--artifact"])
        .arg(&artifact)
        .output()
        .expect("run materialized cached native artifact");
    assert_eq!(artifact_run.status.code(), Some(0));
    assert_eq!(artifact_run.stdout, b"Unit\n");

    let interpreted_artifact = project.0.join("application.loomi");
    let interpreted_build = loom()
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
    let interpreted_run = loom()
        .args(["--backend", "interpreter", "run", "--artifact"])
        .arg(&interpreted_artifact)
        .output()
        .expect("run interpreted manifest artifact");
    assert_eq!(interpreted_run.status.code(), Some(0));
    assert_eq!(interpreted_run.stdout, b"Unit\n");

    let test_target = loom()
        .args(["test", "--target", "app"])
        .arg(&root)
        .output()
        .expect("reject target selection for package tests");
    assert_eq!(test_target.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&test_target.stderr)
            .contains("--target is only valid for source check/build/run/debug"),
        "{test_target:?}"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn library_targets_build_portable_validated_artifacts() {
    let project = TestProject::empty();
    project.write(
        "loom.toml",
        "schema = 2\n[module]\nname = \"sample\"\nversion = \"0.1.0\"\n[[target]]\nname = \"api\"\nkind = \"lib\"\n",
    );
    project.write("lib.loom", "pub fn answer() Int {\n    42\n}\n");
    let first_artifact = project.0.join("sample.loomlib");
    let first = loom()
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
    let artifact_bytes = fs::read(&first_artifact).expect("read portable library");
    let checked = loom_driver::decode_library_artifact(&artifact_bytes)
        .expect("decode and validate portable library");
    assert_eq!(checked.root_package().name(), "sample");
    assert_eq!(checked.root_package().language(), "0.3");
    assert_eq!(checked.interfaces().len(), 1);

    let second_artifact = project.0.join("sample-copy.loomlib");
    let second = loom()
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

    let rejected = loom()
        .args(["--json", "run", "--target", "api"])
        .arg(&project.0)
        .output()
        .expect("reject library as an executable target");
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stdout).contains("TargetKindMismatch"));

    let test_target = loom()
        .args(["test", "--target", "api"])
        .arg(&project.0)
        .output()
        .expect("reject target selection for package tests");
    assert_eq!(test_target.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&test_target.stderr)
            .contains("--target is only valid for source check/build/run/debug"),
        "{test_target:?}"
    );

    project.write(
        "consumer/loom.toml",
        "schema = 2\nlanguage = \"0.3\"\n[module]\nname = \"consumer\"\nversion = \"0.1.0\"\n[dependencies]\nsample = { artifact = \"../sample.loomlib\", version = \"^0.1\" }\n[[target]]\nname = \"consumer\"\nkind = \"bin\"\nentry = \"consumer.main\"\n",
    );
    project.write(
        "consumer/main.loom",
        "import sample.answer\n\npub fn main() {\n    let value = answer()\n    assert value == 42\n}\n",
    );
    project.write(
        "consumer/main_test.loom",
        "test fn artifact_dependency_works() {\n    main()\n}\n",
    );
    fs::remove_file(project.0.join("lib.loom")).expect("remove producer source");
    let consumed = loom()
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
    assert_eq!(consumed.stdout, b"Unit\n");

    for command in ["check", "test"] {
        let verified = loom()
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
    let native = loom()
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
fn git_fork_dependencies_lock_run_offline_and_update_as_one_cli_flow() {
    let project = TestProject::empty();
    let repository = project.0.join("utility-fork");
    fs::create_dir_all(&repository).expect("create git dependency fixture");
    project.write(
        "utility-fork/loom.toml",
        "schema = 2\nlanguage = \"0.3\"\n[module]\nname = \"upstream_utility\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "utility-fork/lib.loom",
        "pub fn answer() Int {\n    42\n}\n",
    );
    run_git(&repository, &["init"]);
    let first_commit = commit_git_fixture(&repository, "initial utility");
    run_git(&repository, &["branch", "-M", "main"]);
    let git_url = git_file_url(&repository);

    let old_field_manifest = format!(
        "schema = 2\nlanguage = \"0.3\"\n[module]\nname = \"consumer\"\nversion = \"1.0.0\"\n[dependencies]\nutility = {{ git = {git_url:?}, branch = \"main\", package = \"upstream_utility\" }}\n"
    );
    project.write("old-field/loom.toml", &old_field_manifest);
    project.write("old-field/main.loom", "fn local() {}\n");
    let old_field = loom_without_test_runtime()
        .args(["--json", "check"])
        .arg(project.0.join("old-field"))
        .output()
        .expect("reject the retired dependency package field");
    assert_eq!(old_field.status.code(), Some(2), "{old_field:?}");
    let old_field_output = format!(
        "{}{}",
        String::from_utf8_lossy(&old_field.stdout),
        String::from_utf8_lossy(&old_field.stderr)
    );
    assert!(old_field_output.contains("package"), "{old_field_output}");
    assert!(old_field_output.contains("module"), "{old_field_output}");

    let consumer_manifest = format!(
        "schema = 2\nlanguage = \"0.3\"\n[module]\nname = \"consumer\"\nversion = \"1.0.0\"\n[dependencies]\nutility = {{ git = {git_url:?}, branch = \"main\", module = \"upstream_utility\" }}\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"consumer.main\"\n"
    );
    assert!(!consumer_manifest.contains("replace"));
    assert!(!consumer_manifest.contains("package ="));
    project.write("consumer/loom.toml", &consumer_manifest);
    project.write(
        "consumer/main.loom",
        "import utility.answer\n\npub fn main() {\n    let value = answer()\n    assert value > 0\n}\n",
    );
    project.write(
        "consumer/main_test.loom",
        "import utility.answer\n\ntest fn fork_dependency_works() {\n    let value = answer()\n    assert value == 42\n}\n",
    );
    let consumer = project.0.join("consumer");

    let resolved = loom_without_test_runtime()
        .args(["--json", "resolve"])
        .arg(&consumer)
        .output()
        .expect("resolve direct git fork URL");
    assert_eq!(
        resolved.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&resolved.stdout),
        String::from_utf8_lossy(&resolved.stderr)
    );
    let lock_path = consumer.join("loom.lock");
    let first_lock = fs::read_to_string(&lock_path).expect("read initial git lockfile");
    let first_checksum = assert_git_lock_pin(
        &first_lock,
        "upstream_utility",
        &git_url,
        "branch:main",
        &first_commit,
    );

    let locked = loom_without_test_runtime()
        .args(["--locked", "--backend", "interpreter", "check"])
        .arg(&consumer)
        .output()
        .expect("check the pinned git dependency");
    assert_eq!(
        locked.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&locked.stdout),
        String::from_utf8_lossy(&locked.stderr)
    );
    let tests = loom_without_test_runtime()
        .args(["--backend", "interpreter", "test"])
        .arg(&consumer)
        .output()
        .expect("test through the git dependency");
    assert_eq!(
        tests.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&tests.stdout),
        String::from_utf8_lossy(&tests.stderr)
    );
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed consumer.fork_dependency_works"),
        "{tests:?}"
    );

    project.write(
        "utility-fork/lib.loom",
        "pub fn answer() Int {\n    84\n}\n",
    );
    let second_commit = commit_git_fixture(&repository, "update utility");
    assert_ne!(first_commit, second_commit);
    let pinned = loom_without_test_runtime()
        .arg("resolve")
        .arg(&consumer)
        .output()
        .expect("keep the locked branch revision without update");
    assert_eq!(
        pinned.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&pinned.stdout),
        String::from_utf8_lossy(&pinned.stderr)
    );
    assert_eq!(
        fs::read_to_string(&lock_path).expect("read unchanged git lockfile"),
        first_lock
    );

    let updated = loom_without_test_runtime()
        .args(["resolve", "--update"])
        .arg(&consumer)
        .output()
        .expect("refresh the git branch revision");
    assert_eq!(
        updated.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&updated.stdout),
        String::from_utf8_lossy(&updated.stderr)
    );
    let second_lock = fs::read_to_string(&lock_path).expect("read refreshed git lockfile");
    assert_ne!(first_lock, second_lock);
    let second_checksum = assert_git_lock_pin(
        &second_lock,
        "upstream_utility",
        &git_url,
        "branch:main",
        &second_commit,
    );
    assert_ne!(first_checksum, second_checksum);

    project.write(
        "consumer/main.loom",
        "import utility.answer\n\npub fn main() {\n    let value = answer()\n    assert value == 84\n}\n",
    );
    fs::rename(&repository, project.0.join("utility-fork-unavailable"))
        .expect("make git remote unavailable");
    let offline = loom_without_test_runtime()
        .args([
            "--offline",
            "--locked",
            "--backend",
            "interpreter",
            "run",
            "--target",
            "app",
        ])
        .arg(&consumer)
        .output()
        .expect("run from the pinned offline git cache");
    assert_eq!(
        offline.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&offline.stdout),
        String::from_utf8_lossy(&offline.stderr)
    );
    assert_eq!(offline.stdout, b"Unit\n");
}

#[test]
#[allow(clippy::too_many_lines)]
fn loopback_http_registry_publish_fetch_lock_and_offline_cache_close_the_loop() {
    let fixture = RegistryFixture::spawn(5);
    let project = TestProject::empty();
    project.write(
        "plaintext-token/loom.toml",
        &format!(
            "schema = 2\nlanguage = \"0.3\"\n[module]\nname = \"utility\"\nversion = \"1.2.0\"\n[registries]\nremote = {{ url = {:?}, token-env = \"LOOM_TEST_REGISTRY_TOKEN\" }}\n",
            fixture.url
        ),
    );
    project.write("plaintext-token/lib.loom", "pub fn answer() Int { 42 }\n");
    let plaintext_token = loom()
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
            "schema = 2\nlanguage = \"0.3\"\n[module]\nname = \"utility\"\nversion = \"1.2.0\"\n[registries]\nremote = {{ url = {:?} }}\n",
            fixture.url
        ),
    );
    project.write("producer/lib.loom", "pub fn answer() Int { 42 }\n");
    let published = loom()
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
        "schema = 2\nlanguage = \"0.3\"\n[module]\nname = \"consumer\"\nversion = \"1.0.0\"\n[registries]\nremote = {{ url = {:?} }}\n[dependencies]\nutility = {{ registry = \"remote\", version = \"^1\" }}\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"consumer.main\"\n",
        fixture.url
    );
    let consumer_source = "import utility.answer\n\npub fn main() {\n    let value = answer()\n    assert value == 42\n}\n";
    project.write("consumer/loom.toml", &consumer_manifest);
    project.write("consumer/main.loom", consumer_source);
    let resolved = loom()
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
    project.write("locked/main.loom", consumer_source);
    fs::copy(
        project.0.join("consumer/loom.lock"),
        project.0.join("locked/loom.lock"),
    )
    .expect("copy lock into cold project");
    let locked = loom()
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
    assert_eq!(published_paths, ["lib.loom", "loom.toml"]);

    let offline = loom()
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
        .find(|path| path.ends_with("lib.loom"))
        .expect("materialized cached source");
    fs::write(&cached_source, "").expect("tamper registry cache source");
    let tampered = loom()
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
    project.write("cold/main.loom", consumer_source);
    let cold = loom()
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
            &format!("schema = 2\n[module]\nname = \"utility\"\nversion = \"{version}\"\n"),
        );
        project.write(
            &format!("registry/utility/{version}/lib.loom"),
            &format!("pub fn answer() Int {{\n    {answer}\n}}\n"),
        );
    };
    write_registry("1.0.0", 10);
    write_registry("1.2.0", 12);
    project.write(
        "app/loom.toml",
        "schema = 2\n[module]\nname = \"app\"\nversion = \"0.1.0\"\n[registries]\nlocal = \"../registry\"\n[dependencies]\nutility = { registry = \"local\", version = \"^1\", optional = true }\n[features]\ndefault = [\"utilities\"]\nutilities = [\"dep:utility\"]\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"app.main\"\n",
    );
    project.write(
        "app/main.loom",
        "import utility.answer\n\npub fn main() {\n    let value = answer()\n    assert value > 0\n}\n",
    );
    let root = project.0.join("app");

    let resolved = loom()
        .args(["--json", "resolve"])
        .arg(&root)
        .output()
        .expect("resolve registry graph");
    assert_eq!(resolved.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&resolved.stdout).contains("dependency_resolution"));
    let lock_path = root.join("loom.lock");
    let first_lock = fs::read_to_string(&lock_path).expect("read generated lockfile");
    assert!(first_lock.contains("version = \"1.2.0\""), "{first_lock}");

    let locked_check = loom()
        .args(["--locked", "check"])
        .arg(&root)
        .output()
        .expect("check locked graph");
    assert_eq!(locked_check.status.code(), Some(0));

    let without_default = loom()
        .args(["--no-default-features", "check"])
        .arg(&root)
        .output()
        .expect("disable optional registry dependency");
    assert_eq!(without_default.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&without_default.stderr).contains("utility"));
    let explicit_feature = loom()
        .args(["--no-default-features", "--features", "utilities", "check"])
        .arg(&root)
        .output()
        .expect("explicitly activate optional dependency");
    assert_eq!(explicit_feature.status.code(), Some(0));

    write_registry("1.3.0", 13);
    let pinned = loom()
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
    let updated = loom()
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
        "registry/utility/1.3.0/lib.loom",
        "pub fn answer() Int {\n    99\n}\n",
    );
    let tampered = loom()
        .arg("check")
        .arg(&root)
        .output()
        .expect("reject mutable registry package");
    assert_eq!(tampered.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&tampered.stderr).contains("checksum differs"));
}

#[test]
fn fmt_check_and_write_form_an_idempotent_real_file_flow() {
    let project = TestProject::new("fn main() {\r\n}\r\n\r\n");
    let first = loom()
        .args(["fmt", "--check"])
        .arg(&project.0)
        .output()
        .expect("run fmt check");
    assert_eq!(first.status.code(), Some(1));

    let write = loom().arg("fmt").arg(&project.0).output().expect("run fmt");
    assert_eq!(write.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(project.0.join("main.loom")).expect("read formatted source"),
        "fn main() {\n}\n"
    );

    let second = loom()
        .args(["fmt", "--check"])
        .arg(&project.0)
        .output()
        .expect("run second fmt check");
    assert_eq!(second.status.code(), Some(0));
}

#[test]
fn fmt_never_writes_dependency_sources() {
    let project = TestProject::empty();
    let dependency_source = "pub fn value() Int {\n\t1   \n}\n";
    project.write(
        "utility/loom.toml",
        "schema = 2\n[module]\nname = \"utility\"\nversion = \"1.0.0\"\n",
    );
    project.write("utility/lib.loom", dependency_source);
    project.write(
        "application/loom.toml",
        "schema = 2\n[module]\nname = \"application\"\nversion = \"1.0.0\"\n[dependencies]\nutility = { path = \"../utility\" }\n",
    );
    project.write("application/main.loom", "fn local() {\n}\n");

    let output = loom()
        .arg("fmt")
        .arg(project.0.join("application"))
        .output()
        .expect("format root package");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        fs::read_to_string(project.0.join("utility/lib.loom")).expect("read dependency source"),
        dependency_source
    );
    assert_eq!(
        fs::read_to_string(project.0.join("application/main.loom")).expect("read root source"),
        "fn local() {\n}\n"
    );
}

#[test]
fn configured_entries_use_one_strict_signature_check() {
    for (name, declaration) in [
        ("parameters", "pub fn main(value Int) {}"),
        ("generic", "pub fn main[T]() {}"),
        ("return", "pub fn main() Int { 1 }"),
    ] {
        let project = TestProject::empty();
        project.write(
            "loom.toml",
            "schema = 2\n[module]\nname = \"sample\"\nversion = \"1.0.0\"\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"sample.main\"\n",
        );
        project.write("main.loom", &format!("{declaration}\n"));
        let output = loom()
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
        "schema = 2\n[module]\nname = \"dependency\"\nversion = \"1.0.0\"\n",
    );
    project.write("dependency/lib.loom", "pub fn main() {}\n");
    project.write(
        "application/loom.toml",
        "schema = 2\n[module]\nname = \"application\"\nversion = \"1.0.0\"\n[dependencies]\ndependency = { path = \"../dependency\" }\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"dependency.main\"\n",
    );
    project.write("application/main.loom", "");
    let output = loom()
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
    let project = TestProject::new("pub fn main() {\n}\n");
    project.write("main_test.loom", "test fn passes() {\n    assert true\n}\n");
    let test = loom()
        .arg("test")
        .arg(&project.0)
        .output()
        .expect("run loom test");
    assert_eq!(test.status.code(), Some(0));
    let stdout = String::from_utf8(test.stdout).expect("UTF-8 stdout");
    assert!(stdout.contains("passed standalone.passes"), "{stdout}");

    let run = loom()
        .arg("run")
        .arg(&project.0)
        .output()
        .expect("run loom run");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn check_rejects_test_declarations_outside_test_files() {
    let project = TestProject::empty();
    project.write("main.loom", "test fn misplaced() {\n    assert true\n}\n");
    let output = loom_without_test_runtime()
        .args(["--json", "--no-cache", "--backend", "interpreter", "check"])
        .arg(&project.0)
        .output()
        .expect("reject a test declaration in an ordinary source file");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    assert!(
        stdout.contains("TestDeclarationOutsideTestFile"),
        "{stdout}"
    );
    assert!(project.0.join("main.loom").is_file());
    assert!(!project.0.join("main_test.loom").exists());
}

#[test]
fn test_selects_one_directory_package_or_the_recursive_root_module() {
    let project = TestProject::empty();
    project.write(
        "loom.toml",
        "schema = 2\n[module]\nname = \"application\"\nversion = \"1.0.0\"\n",
    );
    project.write("main.loom", "pub fn main() {}\n");
    project.write(
        "main_test.loom",
        "test fn root_package_test() { assert true }\n",
    );
    project.write(
        "math/math_test.loom",
        "test fn math_package_test() { assert true }\n",
    );

    let run = |path: &Path| {
        loom_without_test_runtime()
            .args(["--backend", "interpreter", "test"])
            .arg(path)
            .output()
            .expect("run package tests")
    };

    let root = run(&project.0);
    assert_eq!(root.status.code(), Some(0), "{root:?}");
    let root_stdout = String::from_utf8(root.stdout).expect("UTF-8 root test output");
    assert!(root_stdout.contains("root_package_test"), "{root_stdout}");
    assert!(!root_stdout.contains("math_package_test"), "{root_stdout}");

    let math = run(&project.0.join("math"));
    assert_eq!(math.status.code(), Some(0), "{math:?}");
    let math_stdout = String::from_utf8(math.stdout).expect("UTF-8 math test output");
    assert!(!math_stdout.contains("root_package_test"), "{math_stdout}");
    assert!(math_stdout.contains("math_package_test"), "{math_stdout}");

    let recursive = run(&project.0.join("..."));
    assert_eq!(recursive.status.code(), Some(0), "{recursive:?}");
    let recursive_stdout =
        String::from_utf8(recursive.stdout).expect("UTF-8 recursive test output");
    assert!(
        recursive_stdout.contains("root_package_test"),
        "{recursive_stdout}"
    );
    assert!(
        recursive_stdout.contains("math_package_test"),
        "{recursive_stdout}"
    );
}

#[test]
fn explicit_discard_closes_both_backend_cli_loops() {
    let project = TestProject::new(
        r"fn answer() Int {
    42
}

async fn asynchronous_answer() Int {
    Task.sleep(1).await
    answer()
}

pub fn main() {
    discard answer()
}
",
    );
    project.write(
        "main_test.loom",
        r"test async fn discards_awaited_value() {
    discard asynchronous_answer().await
}
",
    );

    for backend in ["interpreter", "llvm"] {
        for command in ["check", "test", "run"] {
            let output = loom()
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
        let build = loom()
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

        let output = loom()
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
        "async fn worker(value Int) Int {\n    value * 2\n}\n\npub async fn main() {\n    let count = 5\n    var tasks = List[Task[Int]]()\n    for i in 0..count {\n        tasks.add(worker(i))\n        Unit\n    }\n    let values = Task.all(tasks).await\n    let length = values.length()\n    assert length == count\n    let selected = values.get(3)\n    match selected {\n        Some(value) => {\n            assert value == 6\n            Unit\n        }\n        None => {\n            assert false\n            Unit\n        }\n    }\n    let missing = values.get(-1)\n    match missing {\n        Some(_) => {\n            assert false\n            Unit\n        }\n        None => Unit\n    }\n}\n",
    );
    for backend in ["interpreter", "llvm"] {
        let output = loom()
            .args(["--backend", backend, "run"])
            .arg(&project.0)
            .output()
            .expect("run range/List program");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{backend}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"Unit\n");

        let artifact = project.0.join(format!("dynamic-{backend}.artifact"));
        let build = loom()
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
        let artifact_output = loom()
            .args(["--backend", backend, "run", "--artifact"])
            .arg(&artifact)
            .output()
            .expect("run range/List artifact");
        assert_eq!(
            artifact_output.status.code(),
            Some(0),
            "{backend} artifact: stdout={} stderr={}",
            String::from_utf8_lossy(&artifact_output.stdout),
            String::from_utf8_lossy(&artifact_output.stderr)
        );
        assert_eq!(artifact_output.stdout, b"Unit\n");
    }
}

#[test]
fn source_backed_process_closes_both_backend_cli_loops() {
    let project = TestProject::new(
        r#"import std.process.arguments
import std.process.environment

fn assertEnvironment() {
    match environment("LOOM_SOURCE_PROCESS_TEST") {
        Some(value) => {
            assert value == "visible"
        }
        None => {
            assert false
        }
    }
}

pub fn main() {
    let values = arguments()
    let count = values.length()
    assert count == 2
    assertEnvironment()
}
"#,
    );
    project.write(
        "main_test.loom",
        "test fn readsEnvironment() {\n    assertEnvironment()\n}\n",
    );

    for backend in ["interpreter", "llvm"] {
        for command in ["check", "test"] {
            let output = loom()
                .args(["--no-cache", "--backend", backend, command])
                .arg(&project.0)
                .env("LOOM_SOURCE_PROCESS_TEST", "visible")
                .output()
                .expect("execute source-backed process command");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{backend} {command}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let run = loom()
            .args(["--no-cache", "--backend", backend, "run"])
            .arg(&project.0)
            .arg("--")
            .args(["alpha", "beta"])
            .env("LOOM_SOURCE_PROCESS_TEST", "visible")
            .output()
            .expect("run source-backed process project");
        assert_eq!(
            run.status.code(),
            Some(0),
            "{backend} run: stdout={} stderr={}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.stdout, b"Unit\n", "{backend} run");

        let artifact = project.0.join(format!("source-process-{backend}.artifact"));
        let build = loom()
            .args(["--no-cache", "--backend", backend, "build", "--output"])
            .arg(&artifact)
            .arg(&project.0)
            .output()
            .expect("build source-backed process artifact");
        assert_eq!(
            build.status.code(),
            Some(0),
            "{backend} build: stdout={} stderr={}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );

        let artifact_run = loom()
            .args(["--backend", backend, "run", "--artifact"])
            .arg(&artifact)
            .arg("--")
            .args(["alpha", "beta"])
            .env("LOOM_SOURCE_PROCESS_TEST", "visible")
            .output()
            .expect("run source-backed process artifact");
        assert_eq!(
            artifact_run.status.code(),
            Some(0),
            "{backend} artifact: stdout={} stderr={}",
            String::from_utf8_lossy(&artifact_run.stdout),
            String::from_utf8_lossy(&artifact_run.stderr)
        );
        assert_eq!(artifact_run.stdout, b"Unit\n", "{backend} artifact");
    }
}

#[test]
fn build_writes_a_runnable_native_artifact() {
    let project = TestProject::new("pub fn main() {\n}\n");
    let artifact = project.0.join("out.native");
    let mut build = loom();
    build
        .args(["build", "--output"])
        .arg(&artifact)
        .arg(&project.0);
    let output = build.output().expect("run loom build");
    assert_eq!(output.status.code(), Some(0));
    assert!(artifact.exists());
    assert!(
        !fs::read(&artifact)
            .expect("read artifact")
            .starts_with(b"{")
    );

    let run = loom()
        .args(["run", "--artifact"])
        .arg(&artifact)
        .output()
        .expect("run built artifact");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(run.stdout, b"Unit\n");
}

#[cfg(unix)]
#[test]
fn debug_builds_source_mapped_native_code_and_launches_a_debugger() {
    let project = TestProject::new("pub fn main() {\n}\n");
    project.write(
        "debug-wrapper",
        "#!/bin/sh\nexecutable=$1\nshift\ntest -x \"$executable\" || exit 91\ntest \"$1\" = \"--\" || exit 92\nshift\ntest \"$1\" = \"alpha\" || exit 93\ntest \"$2\" = \"beta gamma\" || exit 94\ncp \"$executable\" \"$LOOM_DEBUG_COPY\" || exit 95\nprintf 'debug-wrapper:%s:%s\\n' \"$1\" \"$2\"\n\"$executable\" \"$@\"\n",
    );
    project.make_executable("debug-wrapper");
    let debug_copy = project.0.join("debug-program-copy");
    let output = loom()
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
fn debug_routes_text_selection_through_typed_lcir_codegen() {
    let project = TestProject::new("pub fn main() {\n    discard \"value\".get(0)\n}\n");
    project.write(
        "debug-wrapper",
        "#!/bin/sh\nexecutable=$1\nshift\ntest -x \"$executable\" || exit 91\ntest \"$1\" = \"--\" || exit 92\nshift\ncp \"$executable\" \"$LOOM_DEBUG_COPY\" || exit 93\n\"$executable\" \"$@\"\n",
    );
    project.make_executable("debug-wrapper");
    let debug_copy = project.0.join("text-get-debug-program-copy");
    let output = loom()
        .env("LOOM_DEBUG_COPY", &debug_copy)
        .args(["debug", "--debugger"])
        .arg(project.0.join("debug-wrapper"))
        .arg(&project.0)
        .args(["--"])
        .output()
        .expect("launch debugger wrapper for typed Text selection");
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
    let debug_image = fs::read(debug_copy).expect("debug wrapper copied typed executable");
    assert!(!contains_bytes(&debug_image, b"loom.fn."));
    assert!(contains_bytes(&debug_image, b"loom.lcir.fn"));
    assert!(contains_bytes(
        &debug_image,
        b"loom_runtime_text_get_typed_v1"
    ));
    assert!(contains_bytes(&debug_image, b"main.loom"));
}

#[test]
fn debug_rejects_non_native_noninteractive_and_release_modes() {
    let project = TestProject::new("pub fn main() {\n}\n");
    for (arguments, expected) in [
        (
            vec!["--backend", "interpreter", "debug"],
            "require the LLVM backend",
        ),
        (vec!["--release", "debug"], "does not accept --release"),
        (vec!["--json", "debug"], "does not accept --json"),
    ] {
        let output = loom()
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
    let project = TestProject::new("pub fn main() {\n}\n");
    let release_object = project.0.join("release-aarch64.o");
    let release = loom()
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
    let cached = loom()
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
    let development = loom()
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

    let cross_link = loom()
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
    let project = TestProject::new("pub fn main() {}\n");
    let output = loom()
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

#[test]
fn non_linking_commands_do_not_resolve_the_native_runtime_bundle() {
    let project = TestProject::new("pub fn main() {}\n");
    project.write("main_test.loom", "test fn passes() {}\n");
    let unavailable = project.0.join("unavailable-runtime-bundle");

    let checked = loom_without_test_runtime()
        .args(["--json", "check"])
        .arg(&project.0)
        .env("LOOM_RUNTIME_BUNDLE", &unavailable)
        .output()
        .expect("check without a native runtime bundle");
    assert_eq!(checked.status.code(), Some(0), "{checked:?}");

    let object = project.0.join("program-object");
    let built_object = loom_without_test_runtime()
        .args(["--json", "build", "--emit", "object", "--output"])
        .arg(&object)
        .arg(&project.0)
        .env("LOOM_RUNTIME_BUNDLE", &unavailable)
        .output()
        .expect("emit an object without a native runtime bundle");
    assert_eq!(built_object.status.code(), Some(0), "{built_object:?}");
    assert!(
        loom_codegen_llvm::native_artifact_path(
            &object,
            None,
            loom_codegen_llvm::NativeArtifactKind::Object,
        )
        .is_file()
    );

    let interpreted = loom_without_test_runtime()
        .args(["--json", "--backend", "interpreter", "test"])
        .arg(&project.0)
        .env("LOOM_RUNTIME_BUNDLE", &unavailable)
        .output()
        .expect("run interpreter tests without a native runtime bundle");
    assert_eq!(interpreted.status.code(), Some(0), "{interpreted:?}");
}

#[cfg(unix)]
#[test]
fn packed_host_runtime_bundle_builds_and_target_mismatch_fails_closed() {
    let project = TestProject::new("pub fn main() {\n}\n");
    let bundle = project.0.join("host-runtime");
    let packed = loom()
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
    let built = loom()
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

    let mismatch = loom()
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

    let missing_output = loom()
        .args(["runtime", "pack", "--archive"])
        .arg(test_runtime_archive())
        .output()
        .expect("reject runtime pack without an output");
    assert_eq!(missing_output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing_output.stderr).contains("requires --output DIR"));
}

#[test]
fn invalid_explicit_runtime_bundle_does_not_fall_back_to_the_environment() {
    let project = TestProject::new("pub fn main() {}\n");
    let invalid_explicit_bundle = project.0.join("invalid-explicit-runtime");
    fs::write(&invalid_explicit_bundle, b"not a runtime directory")
        .expect("write invalid explicit bundle");

    let no_fallback = loom()
        .args(["--json", "--runtime-bundle"])
        .arg(&invalid_explicit_bundle)
        .args(["build", "--output"])
        .arg(project.0.join("must-not-link"))
        .arg(&project.0)
        .output()
        .expect("invalid explicit bundle must not fall back to the valid environment bundle");

    assert_eq!(no_fallback.status.code(), Some(2), "{no_fallback:?}");
    assert!(String::from_utf8_lossy(&no_fallback.stdout).contains("RuntimeBundleInvalid"));
}

#[cfg(unix)]
#[test]
fn host_linker_resolution_uses_environment_and_prefers_explicit_cli() {
    let project = TestProject::new("pub fn main() {}\n");
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
    let environment = loom()
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
    let explicit = loom()
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
    let project = TestProject::new("pub fn main() {\n}\n");
    let bundle_one = project.0.join("runtime-one");
    write_fake_runtime_bundle(&bundle_one, b"foreign runtime archive one");
    write_fake_linker(&project, "linker identity one");
    let linker = project.0.join("fake-linker");
    let link_log = project.0.join("link-arguments");
    let object_copy = project.0.join("linked-target-object");
    let runtime_copy = project.0.join("linked-runtime-archive");
    let link_payload = project.0.join("link-payload");
    fs::write(&link_payload, b"payload one\n").expect("first linker payload");

    let build = |bundle: &std::path::Path, output: &std::path::Path| {
        loom()
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
            .env("LOOM_FAKE_RUNTIME_COPY", &runtime_copy)
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
    let staged_runtime = std::path::Path::new(arguments[1]);
    assert_eq!(staged_runtime.parent(), first_output.parent());
    assert!(
        staged_runtime
            .file_stem()
            .is_some_and(|name| name.to_string_lossy().starts_with(".loom-runtime-link-")),
        "{arguments:#?}"
    );
    assert_eq!(
        staged_runtime.extension().and_then(|value| value.to_str()),
        Some("a")
    );
    assert!(!staged_runtime.exists(), "runtime snapshot must be removed");
    assert_eq!(
        fs::read(&runtime_copy).expect("copied runtime snapshot"),
        b"foreign runtime archive one"
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
    let project = TestProject::new("pub fn main() {\n}\n");
    let release_output = project.0.join("release-native");
    let native = loom()
        .args(["--release", "build", "--output"])
        .arg(&release_output)
        .arg(&project.0)
        .output()
        .expect("build release native executable");
    assert_eq!(native.status.code(), Some(0));
    let executed = loom()
        .args(["run", "--artifact"])
        .arg(native_executable(&release_output))
        .output()
        .expect("run release executable");
    assert_eq!(executed.status.code(), Some(0));
    assert_eq!(executed.stdout, b"Unit\n");
}

#[test]
fn source_backed_std_closes_real_check_build_test_and_run_commands() {
    let project = fixture_project!("std-source");

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(&project.0)
        .output()
        .expect("check source-backed std calls through the production CLI");
    assert_eq!(check.status.code(), Some(0), "{check:?}");

    let object_path = project.0.join("std-source.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(&project.0)
        .output()
        .expect("build source-backed std calls through the production CLI");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    let object = fs::read(&object_path).expect("read source-backed std object");
    assert!(contains_bytes(&object, b"loom.lcir.fn"));
    assert!(!contains_bytes(&object, b"loom.fn."));

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(&project.0)
        .output()
        .expect("test source-backed std calls through the production CLI");
    assert_eq!(tests.status.code(), Some(0), "{tests:?}");
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.sourceBackedAlgorithms"),
        "{tests:?}"
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(&project.0)
        .output()
        .expect("run source-backed std calls through the production CLI");
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert_eq!(run.stdout, b"Unit\n");
}

#[test]
fn core_examples_close_check_build_test_and_run() {
    for (fixture, source, tests) in [
        (
            "constraints-contracts",
            include_str!("../../../examples/constraints-contracts/shop.loom"),
            include_str!("../../../examples/constraints-contracts/shop_test.loom"),
        ),
        (
            "concepts-polymorphism",
            include_str!("../../../examples/concepts-polymorphism/concepts.loom"),
            include_str!("../../../examples/concepts-polymorphism/concepts_test.loom"),
        ),
        (
            "async-resources",
            include_str!("../../../examples/async-resources/tasks.loom"),
            include_str!("../../../examples/async-resources/tasks_test.loom"),
        ),
    ] {
        let project = TestProject::new(source);
        project.write("main_test.loom", tests);
        for command in ["check", "test", "run"] {
            let output = loom()
                .arg(command)
                .arg(&project.0)
                .output()
                .expect("run loom command");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{fixture} {command}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let artifact = project.0.join(format!("{fixture}.native"));
        let mut command = loom();
        command
            .args(["build", "--output"])
            .arg(&artifact)
            .arg(&project.0);
        let build = command.output().expect("build fixture artifact");
        assert_eq!(build.status.code(), Some(0), "{fixture} build");
        let run = loom()
            .args(["run", "--artifact"])
            .arg(artifact)
            .output()
            .expect("run fixture artifact");
        assert_eq!(run.status.code(), Some(0), "{fixture} artifact run");
    }
}

#[test]
fn must_scope_identity_closes_cached_check_build_test_run_and_artifact_decode() {
    let project = TestProject::new(
        r"import std.resource.Dispose
import std.resource.MustScope

record Resource {
    value Int
}

impl Dispose for Resource {
    method dispose(mut self) {
        self.value = 0
    }
}

impl MustScope for Resource {}

pub fn main() {
    scoped resource = Resource { value = 1 }
}
",
    );
    project.write(
        "main_test.loom",
        r"test fn resource_identity() {
    scoped resource = Resource { value = 2 }
}
",
    );

    let check = loom_without_test_runtime()
        .args(["--json", "--backend", "interpreter", "check"])
        .arg(&project.0)
        .output()
        .expect("check MustScope identity source");
    assert_eq!(check.status.code(), Some(0), "{check:?}");
    assert_eq!(
        cache_status(&check.stdout, "checked_mir").as_deref(),
        Some("miss")
    );

    let artifact = project.0.join("resource.loomi");
    let build = loom_without_test_runtime()
        .args(["--json", "--backend", "interpreter", "build", "--output"])
        .arg(&artifact)
        .arg(&project.0)
        .output()
        .expect("build MustScope identity artifact");
    assert_eq!(build.status.code(), Some(0), "{build:?}");
    assert_eq!(
        cache_status(&build.stdout, "checked_mir").as_deref(),
        Some("hit")
    );

    let artifact_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&artifact).expect("read MustScope identity artifact"))
            .expect("decode MustScope artifact JSON");
    let concepts = artifact_json["program"]["concepts"]
        .as_array()
        .expect("concept array");
    let (marker_index, marker) = concepts
        .iter()
        .enumerate()
        .find(|(_, concept)| concept["module"] == "std.resource" && concept["name"] == "MustScope")
        .expect("canonical MustScope concept");
    assert_eq!(marker["identity"], "mustScope");
    assert_eq!(
        artifact_json["program"]["prelude"]["must_scope_concept"],
        serde_json::json!(marker_index)
    );

    for (command, expected_cache) in [("test", "miss"), ("run", "hit")] {
        let output = loom_without_test_runtime()
            .args(["--json", "--backend", "interpreter", command])
            .arg(&project.0)
            .output()
            .expect("execute MustScope identity source");
        assert_eq!(output.status.code(), Some(0), "{command}: {output:?}");
        assert_eq!(
            cache_status(&output.stdout, "checked_mir").as_deref(),
            Some(expected_cache),
            "{command}: {output:?}"
        );
    }

    let artifact_run = loom_without_test_runtime()
        .args(["--json", "--backend", "interpreter", "run", "--artifact"])
        .arg(&artifact)
        .output()
        .expect("run the checked MustScope artifact");
    assert_eq!(artifact_run.status.code(), Some(0), "{artifact_run:?}");
}

#[test]
fn run_rejects_an_incompatible_artifact_format_version() {
    let project = TestProject::new("");
    let artifact = project.0.join("incompatible-version.loomi");
    let unsupported_version = loom_mir::INTERPRETED_ARTIFACT_VERSION
        .checked_sub(1)
        .expect("artifact format version must be positive");
    fs::write(
        &artifact,
        serde_json::to_vec(&serde_json::json!({
            "format": loom_mir::INTERPRETED_ARTIFACT_FORMAT,
            "version": unsupported_version,
            "unexpected": true,
        }))
        .expect("encode incompatible artifact"),
    )
    .expect("write incompatible artifact");
    let output = loom_without_test_runtime()
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
    let project = TestProject::new("pub fn main() {}\n");
    let artifact = project.0.join("future.loomi");
    let build = loom()
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

    let output = loom()
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
