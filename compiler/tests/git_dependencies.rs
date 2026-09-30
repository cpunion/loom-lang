use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::SystemTime,
};

mod common;
#[path = "fixtures/git_https.rs"]
mod git_https;
use common::success;

const URL: &str = "https://fixture.invalid/seed";
const SENTINEL: &str = "LOOM_GIT_REMOTE_SENTINEL_secret";
const AUTH_HEADER: &str = "Authorization: Basic bG9vbTpMT09NX0dJVF9SRU1PVEVfU0VOVElORUxfc2VjcmV0";

fn write(root: &Path, name: &str, contents: impl AsRef<[u8]>) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn program(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| common::executable(&directory, name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("integration fixture requires {name} on PATH"))
        .canonicalize()
        .unwrap()
}

struct Fixture {
    temporary: tempfile::TempDir,
    git: PathBuf,
    tool: PathBuf,
    revisions: [String; 2],
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let git = program("git");
        let tool = common::executable(&root.join("tools"), "git");
        fs::create_dir_all(tool.parent().unwrap()).unwrap();
        fs::create_dir(root.join("setup-home")).unwrap();
        fs::create_dir(root.join("setup-hooks")).unwrap();
        fs::write(root.join("setup-config"), "").unwrap();
        let mut rustc = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
        // Direct rustc does not consume Cargo's linker setting. In Git Bash,
        // PATH may otherwise select its unrelated coreutils link.exe.
        #[cfg(all(windows, target_env = "msvc", target_arch = "x86_64"))]
        if let Some(linker) = std::env::var_os("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER") {
            let mut option = std::ffi::OsString::from("linker=");
            option.push(linker);
            rustc.arg("-C").arg(option);
        }
        let output = rustc
            .arg(common::root().join("compiler/tests/fixtures/git_forwarder.rs"))
            .args(["--edition=2021", "-o"])
            .arg(&tool)
            .env("LOOM_GIT_FIXTURE_ROOT", root.canonicalize().unwrap())
            .env("LOOM_GIT_FIXTURE_REAL_GIT", &git)
            .output()
            .unwrap();
        success(&output);
        let mut fixture = Self {
            temporary,
            git,
            tool,
            revisions: Default::default(),
        };
        fixture.git(&["init", "--template=", "--object-format=sha1", "remote"]);
        write(
            fixture.root(),
            "remote/loom.toml",
            "[module]\nname='seed'\n",
        );
        write(
            fixture.root(),
            "remote/.gitattributes",
            "main.loom export-ignore\n*.bin -text\n",
        );
        write(
            fixture.root(),
            "remote/data.bin",
            (0..=255).collect::<Vec<u8>>(),
        );
        write(
            fixture.root(),
            "remote/poison_test.loom",
            "this dependency test file must never be parsed !!!",
        );
        for (index, value) in [11, 29].into_iter().enumerate() {
            write(
                fixture.root(),
                "remote/main.loom",
                format!(
                    "pub fn value() Int {{ {value} }}\ntest fn excluded_dependency_test() {{ assert 9001 == 0 }}\n"
                ),
            );
            fixture.git(&["-C", "remote", "add", "."]);
            fixture.git(&[
                "-C",
                "remote",
                "commit",
                "--quiet",
                "-m",
                "fixture revision",
            ]);
            let output = fixture.git(&["-C", "remote", "rev-parse", "HEAD"]);
            fixture.revisions[index] = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        }
        fixture
    }

    fn root(&self) -> &Path {
        self.temporary.path()
    }

    fn git(&self, arguments: &[&str]) -> Output {
        let mut command = Command::new(&self.git);
        command.current_dir(self.root()).env_clear();
        for name in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP", "TMPDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let output = command
            .env("HOME", self.root().join("setup-home"))
            .env("GIT_CONFIG_GLOBAL", self.root().join("setup-config"))
            .env("GIT_CONFIG_SYSTEM", self.root().join("setup-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "-c",
                "core.autocrlf=false",
                "-c",
            ])
            .arg(format!(
                "core.hooksPath={}",
                self.root().join("setup-hooks").display()
            ))
            .args(arguments)
            .output()
            .unwrap();
        success(&output);
        output
    }

    fn command(&self, mode: &str, package: &Path) -> Command {
        let mut command = common::command(&[mode, package.to_str().unwrap()]);
        // Accidental Git invocations during offline commands hit the fixture as
        // well. Native compiler/linker lookup remains available after it.
        let mut paths = vec![self.tool.parent().unwrap().to_path_buf()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        command.env("PATH", std::env::join_paths(paths).unwrap());
        command
    }

    fn resolve(&self, package: &Path, tests: bool) -> Output {
        self.resolve_with_credentials(package, tests, false)
    }

    fn resolve_with_credentials(&self, package: &Path, tests: bool, credentials: bool) -> Output {
        let hostile = self.root().join("hostile-home");
        write(
            &hostile,
            ".gitconfig",
            "[include]\npath = /unreadable-fixture-config\n[http]\nsslVerify = false\n[credential]\nhelper = !echo leaked\n",
        );
        write(
            &hostile,
            ".netrc",
            format!("machine fixture.invalid login private password {SENTINEL}\n"),
        );
        let mut command = self.command("resolve", package);
        command
            .arg("--std")
            .arg(common::root().join("compiler/std"))
            .arg("--git-tool")
            .arg(&self.tool)
            .env("HOME", &hostile)
            .env("USERPROFILE", &hostile)
            .env("LOOM_GIT_AMBIENT_SENTINEL", SENTINEL)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.sslVerify")
            .env("GIT_CONFIG_VALUE_0", "false")
            .env("GIT_SSL_NO_VERIFY", "1")
            .env("GIT_TRACE", "1")
            .env("HTTPS_PROXY", "http://invalid-proxy.example:1")
            .env("GIT_ASKPASS", "must-not-execute");
        if tests {
            command.arg("--tests");
        }
        if credentials {
            command.arg("--git-credential-tool").arg(&self.tool);
        }
        command.output().unwrap()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root().join("git.log")).unwrap_or_default()
    }

    fn import_completion(&self, package: &Path) -> String {
        let text = "import seed.va";
        let snapshot = self.root().join("import-overlay.loom");
        fs::write(&snapshot, text).unwrap();
        let output = self
            .command("editor-complete", package)
            .arg("--std")
            .arg(common::root().join("compiler/std"))
            .arg("--at")
            .arg(package.join("main.loom"))
            .arg(text.len().to_string())
            .arg("--overlay")
            .arg(package.join("main.loom"))
            .arg(snapshot)
            .output()
            .unwrap();
        success(&output);
        String::from_utf8(output.stdout).unwrap()
    }
}

fn rejected(output: &Output, expected: &str) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "{output:?}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(SENTINEL),
        "{output:?}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains(SENTINEL),
        "{output:?}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(AUTH_HEADER));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(AUTH_HEADER));
}

#[test]
fn explicit_credentials_are_fetch_scoped_redacted_and_unneeded_offline() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let server = git_https::Server::start(root, &fixture.git);
    let url = &server.url;
    let app = |name: &str, url: &str| {
        write(
            root,
            &format!("{name}/loom.toml"),
            format!(
                "[module]\nname='app'\n[dependencies.seed]\ngit='{url}'\nrev='{}'\n",
                fixture.revisions[0]
            ),
        );
        write(
            root,
            &format!("{name}/main.loom"),
            "import seed.value\nfn main() { assert value() == 11 }\n",
        );
        root.join(name)
    };
    fs::write(root.join("requires-auth"), "").unwrap();
    let anonymous = app("anonymous", url);
    rejected(
        &fixture.resolve(&anonymous, false),
        "remote output was suppressed",
    );
    assert!(!root.join("credential.log").exists());

    let authenticated = app("authenticated", url);
    let resolved = fixture.resolve_with_credentials(&authenticated, false, true);
    assert!(
        resolved.status.success(),
        "Loom: {}\nTest-only Git transport: {}",
        String::from_utf8_lossy(&resolved.stderr),
        fs::read_to_string(root.join("transport-error.log")).unwrap_or_default()
    );
    assert!(
        server
            .authenticated
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 2
    );
    let requests = fs::read(root.join("credential.log")).unwrap();
    assert_eq!(
        requests,
        format!(
            "protocol=https\nhost={}\npath=seed\n\n",
            url[8..].split('/').next().unwrap()
        )
        .as_bytes()
    );
    for (bytes, _) in cached_files(&authenticated).values() {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains(SENTINEL) && !text.contains(AUTH_HEADER));
    }
    let log = fixture.log();
    fs::write(root.join("offline"), "").unwrap();
    fs::write(root.join("credential-mode"), "fail").unwrap();
    success(&fixture.resolve_with_credentials(&authenticated, false, true));
    for command in ["check", "test", "run"] {
        success(&fixture.command(command, &authenticated).output().unwrap());
    }
    assert_eq!(fixture.log(), log);
    assert_eq!(fs::read(root.join("credential.log")).unwrap(), requests);
    fs::remove_file(root.join("offline")).unwrap();

    for (mode, expected) in [
        ("fail", "credential tool failed; output was suppressed"),
        (
            "scope",
            "credential tool changed the requested source scope",
        ),
        ("empty", "remote output was suppressed"),
    ] {
        fs::write(root.join("credential-mode"), mode).unwrap();
        let package = app(mode, url);
        rejected(
            &fixture.resolve_with_credentials(&package, false, true),
            expected,
        );
        assert!(!package.join("loom.lock").exists());
    }
    fs::write(root.join("credential-mode"), "ok").unwrap();
    fs::write(root.join("auth-echo"), "").unwrap();
    rejected(
        &fixture.resolve_with_credentials(&app("echo", url), false, true),
        "remote output was suppressed",
    );
    let requests = fs::read(root.join("credential.log")).unwrap();
    rejected(
        &fixture.resolve_with_credentials(&app("http", "http://fixture.invalid/seed"), false, true),
        "HTTPS",
    );
    assert_eq!(fs::read(root.join("credential.log")).unwrap(), requests);
    rejected(
        &fixture
            .command("check", &authenticated)
            .arg("--git-credential-tool")
            .arg(&fixture.tool)
            .output()
            .unwrap(),
        "only available with resolve",
    );
}

fn cached_files(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                visit(root, &entry.path(), files);
            } else {
                assert!(metadata.is_file());
                files.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    (
                        fs::read(entry.path()).unwrap(),
                        metadata.modified().unwrap(),
                    ),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn root_graph_forks_unify_declared_edges_without_granting_imports() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let app = root.join("app");
    let mut revisions = Vec::new();
    for value in [11, 29] {
        write(
            root,
            "remote/main.loom",
            format!(
                r#"pub record Token {{
    value Int
}}

pub fn make() Token {{
    Token {{ value = {value} }}
}}
"#
            ),
        );
        fixture.git(&["-C", "remote", "add", "."]);
        fixture.git(&[
            "-C",
            "remote",
            "commit",
            "--quiet",
            "-m",
            "nominal fork fixture",
        ]);
        let revision = fixture.git(&["-C", "remote", "rev-parse", "HEAD"]);
        revisions.push(
            String::from_utf8(revision.stdout)
                .unwrap()
                .trim()
                .to_owned(),
        );
    }
    for (name, revision) in ["left", "right"].into_iter().zip(&revisions) {
        write(
            root,
            &format!("{name}/loom.toml"),
            format!(
                r#"[module]
name = '{name}'
[dependencies.seed]
git = '{URL}'
rev = '{revision}'
scope = 'graph'
"#
            ),
        );
    }
    write(
        root,
        "left/main.loom",
        r#"import seed.Token
import seed.make

pub fn token() Token {
    make()
}
"#,
    );
    write(
        root,
        "right/main.loom",
        r#"import seed.Token

pub fn take(value Token) Int {
    value.value
}
"#,
    );
    write(
        root,
        "app/main.loom",
        r#"import left.token
import right.take

fn main() {
    assert take(token()) == 29
}

test fn selected_fork() {
    main()
}
"#,
    );
    let manifest = |selection: &str| {
        format!(
            r#"[module]
name = 'app'
[dependencies.left]
path = '../left'
[dependencies.right]
path = '../right'
[dependencies.seed]
{selection}
"#
        )
    };
    let git_selection = format!("git='{URL}'\nrev='{}'", revisions[1]);
    write(root, "app/loom.toml", manifest(&git_selection));
    success(&fixture.resolve(&app, false));
    // Transitive scope declarations have no authority over this root. Local
    // fork identities remain distinct even though both records have one field.
    assert!(
        !fixture
            .command("check", &app)
            .output()
            .unwrap()
            .status
            .success()
    );
    let fetched = fixture.log();
    assert_eq!(
        fetched
            .lines()
            .filter(|line| line.starts_with("fetch\t"))
            .count(),
        2
    );

    write(
        root,
        "app/loom.toml",
        manifest(&format!("{git_selection}\nscope='graph'")),
    );
    let original_lock = fs::read(app.join("loom.lock")).unwrap();
    rejected(
        &fixture.command("check", &app).output().unwrap(),
        "changed from loom.lock",
    );
    assert_eq!(fs::read(app.join("loom.lock")).unwrap(), original_lock);
    success(&fixture.resolve(&app, false));
    success(&fixture.command("check", &app).output().unwrap());
    success(&fixture.command("test", &app).output().unwrap());
    let artifact = common::executable(root, "fork-app");
    success(
        &fixture
            .command("build", &app)
            .arg("--output")
            .arg(&artifact)
            .output()
            .unwrap(),
    );
    success(&Command::new(&artifact).output().unwrap());
    success(&fixture.command("run", &app).output().unwrap());
    let lock = fs::read_to_string(app.join("loom.lock")).unwrap();
    assert_eq!(
        lock.lines()
            .filter(|line| line.contains(&revisions[1]))
            .count(),
        2
    );
    assert!(!lock.contains(&revisions[0]));
    assert_eq!(fixture.log(), fetched); // The chosen source was already verified.

    // A root path selector uses the root's directory, not each importing module.
    write(
        root,
        "app/loom.toml",
        manifest("scope='graph'\npath='../remote'"),
    );
    success(&fixture.command("run", &app).output().unwrap());
    assert_eq!(fixture.log(), fetched);
    write(root, "left/loom.toml", "[module]\nname='left'\n");
    rejected(
        &fixture.command("check", &app).output().unwrap(),
        "not a declared direct dependency",
    );
}

#[test]
fn pinned_git_instances_survive_native_commands_and_verified_offline_cache_repairs() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let app = root.join("app");
    write(
        root,
        "app/loom.toml",
        format!(
            "[module]\nname='app'\n[dependencies.left]\npath='../left'\n[dependencies.right]\npath='../right'\n[dependencies.seed]\ngit='{URL}'\nrev='{}'\n[dependencies.unused]\npath='../does-not-exist'\n[dependencies.unusedgit]\ngit='https://fixture.invalid/never'\nrev='{}'\n",
            fixture.revisions[0],
            "0".repeat(40)
        ),
    );
    write(
        root,
        "app/main.loom",
        "import left.answer\nimport right.answer\nfn main() { assert left.answer() == 11 && right.answer() == 29 }\n",
    );
    write(
        root,
        "app/main_test.loom",
        "import seed.value\ntest fn selected_root_test() { main()\nassert value() == 11 }\n",
    );
    for (name, revision) in ["left", "right"].into_iter().zip(&fixture.revisions) {
        write(
            root,
            &format!("{name}/loom.toml"),
            format!(
                "[module]\nname='{name}'\n[dependencies.seed]\ngit='{URL}'\nrev='{revision}'\n"
            ),
        );
        write(
            root,
            &format!("{name}/main.loom"),
            "import seed.value\npub fn answer() Int { value() }\n",
        );
    }
    rejected(
        &fixture.command("check", &app).output().unwrap(),
        "run loom resolve",
    );
    assert!(fixture.import_completion(&app).contains("\"items\":[]"));
    assert!(fixture.log().is_empty());
    success(&fixture.resolve(&app, false));
    let fetched = fixture.log();
    let fetches = fetched
        .lines()
        .filter(|line| line.starts_with("fetch\t"))
        .collect::<Vec<_>>();
    assert_eq!(fetches.len(), 2, "{fetched}");
    for revision in &fixture.revisions {
        assert!(fetches.iter().any(|line| line.ends_with(revision)));
    }
    assert!(!fetched.contains("fixture.invalid/never"));
    rejected(
        &fixture.command("test", &app).output().unwrap(),
        "not locked",
    );
    success(&fixture.resolve(&app, true));
    assert_eq!(
        fixture.log(),
        fetched,
        "test-only edge must reuse the identical pinned source"
    );

    let cache = app.join("target/loom-deps");
    let original = cached_files(&cache);
    let lock_path = app.join("loom.lock");
    let lock = fs::read(&lock_path).unwrap();
    let lock_modified = fs::metadata(&lock_path).unwrap().modified().unwrap();
    write(root, "offline", "no Git invocations allowed");
    assert!(
        fixture
            .import_completion(&app)
            .contains("\"label\":\"value\"")
    );
    success(&fixture.command("check", &app).output().unwrap());
    let artifact = common::executable(root, "git-dependencies");
    let ir = root.join("git-dependencies.ll");
    success(
        &fixture
            .command("build", &app)
            .arg("--output")
            .arg(&artifact)
            .arg("--emit-ir")
            .arg(&ir)
            .env("LOOM_OPT_LEVEL", "0")
            .output()
            .unwrap(),
    );
    success(&Command::new(&artifact).output().unwrap());
    success(&fixture.command("run", &app).output().unwrap());
    let output = fixture.command("test", &app).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"1 tests passed\n");
    assert!(!fs::read_to_string(ir).unwrap().contains("9001"));
    assert_eq!(cached_files(&cache), original);
    assert_eq!(fs::read(&lock_path).unwrap(), lock);
    assert_eq!(
        fs::metadata(&lock_path).unwrap().modified().unwrap(),
        lock_modified
    );
    success(&fixture.resolve(&app, true));
    assert_eq!(
        fixture.log(),
        fetched,
        "valid offline cache must not invoke Git"
    );
    assert_eq!(cached_files(&cache), original);
    fs::remove_file(root.join("offline")).unwrap();

    let selected = original
        .iter()
        .find(|(path, (bytes, _))| {
            path.file_name() == Some(OsStr::new("main.loom"))
                && String::from_utf8_lossy(bytes).contains("{ 11 }")
        })
        .unwrap()
        .0
        .parent()
        .unwrap();
    let selected = cache.join(selected);
    assert_eq!(
        fs::read(selected.join("data.bin")).unwrap(),
        (0..=255).collect::<Vec<u8>>()
    );
    for mutation in ["modify", "add", "delete"] {
        match mutation {
            "modify" => {
                fs::write(selected.join("main.loom"), "pub fn value() Int { 12 }\n").unwrap()
            }
            "add" => fs::write(selected.join("unselected.txt"), "unexpected member").unwrap(),
            "delete" => fs::remove_file(selected.join("data.bin")).unwrap(),
            _ => unreachable!(),
        }
        let before = fixture.log();
        rejected(
            &fixture.command("check", &app).output().unwrap(),
            "cache is missing or changed",
        );
        assert!(fixture.import_completion(&app).contains("\"items\":[]"));
        assert_eq!(
            fixture.log(),
            before,
            "ordinary commands must not repair or fetch"
        );
        success(&fixture.resolve(&app, true));
        let repaired = cached_files(&cache);
        assert_eq!(
            repaired
                .iter()
                .map(|(path, (bytes, _))| (path, bytes))
                .collect::<Vec<_>>(),
            original
                .iter()
                .map(|(path, (bytes, _))| (path, bytes))
                .collect::<Vec<_>>()
        );
        assert_eq!(fs::read(&lock_path).unwrap(), lock);
    }

    // Changing both the cached expectation and its repeated importer edge must
    // still fail against bytes reconstructed from the pinned Git commit.
    let mut altered = String::from_utf8(lock.clone())
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for line in altered.iter_mut().skip(1) {
        let mut fields = line.split('\t').map(str::to_owned).collect::<Vec<_>>();
        if fields[3] == fixture.revisions[0] {
            fields[4] = "0".repeat(64);
        }
        *line = fields.join("\t");
    }
    let altered = altered.join("\n") + "\n";
    fs::write(&lock_path, &altered).unwrap();
    rejected(
        &fixture.command("check", &app).output().unwrap(),
        "cache is missing or changed",
    );
    rejected(
        &fixture.resolve(&app, true),
        "does not match the locked content digest",
    );
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), altered);
    fs::write(&lock_path, lock).unwrap();
    success(&fixture.command("check", &app).output().unwrap());
}

#[test]
fn git_subdirectories_share_snapshots_but_keep_module_identities_and_locked_edges() {
    let fixture = Fixture::new();
    let root = fixture.root();
    // Selecting a nested module neither requires nor parses a root module.
    write(root, "remote/loom.toml", "not a module manifest");
    for (directory, value) in [("one", 11), ("two", 29)] {
        write(
            root,
            &format!("remote/packages/{directory}/loom.toml"),
            "[module]\nname='seed'\n[dependencies.helper]\npath='../helper'\n",
        );
        write(
            root,
            &format!("remote/packages/{directory}/main.loom"),
            format!(
                r#"import helper.identity
pub record Value {{
    number Int
}}
pub fn make() Value {{
    Value {{ number = identity({value}) }}
}}
pub fn take(value Value) Int {{
    value.number
}}
test fn excluded() {{
    assert false
}}
"#
            ),
        );
        write(
            root,
            &format!("remote/packages/{directory}/poison_test.loom"),
            "dependency tests must not be parsed !!!",
        );
    }
    write(
        root,
        "remote/packages/helper/loom.toml",
        "[module]\nname='helper'\n",
    );
    write(
        root,
        "remote/packages/helper/main.loom",
        "pub fn identity(value Int) Int { value }\n",
    );
    fixture.git(&["-C", "remote", "add", "."]);
    fixture.git(&["-C", "remote", "commit", "--quiet", "-m", "nested modules"]);
    let revision = String::from_utf8(fixture.git(&["-C", "remote", "rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    for (name, subdir) in [("left", "one"), ("right", "two")] {
        write(
            root,
            &format!("{name}/loom.toml"),
            format!(
                "[module]\nname='{name}'\n[dependencies.seed]\ngit='{URL}'\nrev='{revision}'\nsubdir='packages/{subdir}'\n"
            ),
        );
        write(
            root,
            &format!("{name}/main.loom"),
            r#"import seed.Value
import seed.make
import seed.take
pub fn make() Value {
    seed.make()
}
pub fn take(value Value) Int {
    seed.take(value)
}
"#,
        );
    }
    let manifest = |subdir: &str| {
        format!(
            "[module]\nname='app'\n[dependencies.left]\npath='../left'\n[dependencies.right]\npath='../right'\n[dependencies.seed]\ngit='{URL}'\nrev='{revision}'\nsubdir='{subdir}'\n"
        )
    };
    let source = r#"import left.make
import left.take
import right.make
import right.take
import seed.make
fn main() {
    assert left.take(left.make()) == 11
    assert right.take(right.make()) == 29
    assert left.take(seed.make()) == 11
}
test fn nested_modules() {
    main()
}
"#;
    let app = root.join("app");
    write(root, "app/loom.toml", manifest("packages/one"));
    write(root, "app/main.loom", source);
    success(&fixture.resolve(&app, true));
    let fetched = fixture.log();
    assert_eq!(
        fetched
            .lines()
            .filter(|line| line.starts_with("fetch\t"))
            .count(),
        1
    );
    let lock_path = app.join("loom.lock");
    let lock = fs::read_to_string(&lock_path).unwrap();
    assert_eq!(
        lock.lines()
            .filter(|line| line.ends_with("\tpackages/one"))
            .count(),
        2
    );
    assert_eq!(
        lock.lines()
            .filter(|line| line.ends_with("\tpackages/two"))
            .count(),
        1
    );
    assert_eq!(
        fs::read_dir(app.join("target/loom-deps")).unwrap().count(),
        1
    );
    write(root, "offline", "Git must not run after resolution");
    success(&fixture.command("check", &app).output().unwrap());
    success(&fixture.command("run", &app).output().unwrap());
    let tested = fixture.command("test", &app).output().unwrap();
    success(&tested);
    assert_eq!(tested.stdout, b"1 tests passed\n");

    write(
        root,
        "app/main.loom",
        source.replace("left.take(seed.make())", "left.take(right.make())"),
    );
    assert!(
        !fixture
            .command("check", &app)
            .output()
            .unwrap()
            .status
            .success(),
        "same named records in different source subdirectories must remain distinct"
    );
    write(root, "app/main.loom", source);
    write(root, "app/loom.toml", manifest("packages/two"));
    rejected(
        &fixture.command("check", &app).output().unwrap(),
        "changed from loom.lock",
    );
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), lock);
    write(
        root,
        "app/main.loom",
        source.replace(
            "left.take(seed.make()) == 11",
            "right.take(seed.make()) == 29",
        ),
    );
    success(&fixture.resolve(&app, true));
    success(&fixture.command("run", &app).output().unwrap());
    let updated = fs::read_to_string(&lock_path).unwrap();
    assert_ne!(updated, lock);
    for subdir in ["packages/missing", "Packages/two", "packages/two/main.loom"] {
        write(root, "app/loom.toml", manifest(subdir));
        rejected(
            &fixture.resolve(&app, true),
            "does not select an exact snapshot directory",
        );
        assert_eq!(fs::read_to_string(&lock_path).unwrap(), updated);
    }
    write(root, "app/loom.toml", manifest("packages/helper"));
    rejected(
        &fixture.resolve(&app, true),
        "dependency name does not match module.name",
    );
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), updated);
    assert_eq!(fixture.log(), fetched);
}

#[test]
fn failed_git_resolution_suppresses_echoed_remote_data_and_leaves_no_lock() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let app = root.join("app");
    write(
        root,
        "app/loom.toml",
        format!(
            "[module]\nname='app'\n[dependencies.seed]\ngit='https://fixture.invalid/failure/{SENTINEL}'\nrev='{}'\n",
            fixture.revisions[0]
        ),
    );
    write(
        root,
        "app/main.loom",
        "import seed.value\nfn main() { assert value() == 11 }\n",
    );
    rejected(
        &fixture.resolve(&app, false),
        "remote output was suppressed",
    );
    assert!(
        fixture
            .log()
            .lines()
            .any(|line| line.starts_with("fetch\t"))
    );
    assert!(!app.join("loom.lock").exists());
    assert!(
        fs::read_dir(app.join("target/loom-deps"))
            .unwrap()
            .next()
            .is_none()
    );
}
