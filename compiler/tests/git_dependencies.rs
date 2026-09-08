use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::SystemTime,
};

mod common;
use common::success;

const URL: &str = "https://fixture.invalid/seed";
const SENTINEL: &str = "LOOM_GIT_REMOTE_SENTINEL_secret";

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
        let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
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
        command.output().unwrap()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root().join("git.log")).unwrap_or_default()
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
