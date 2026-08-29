use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_driver::{AnalysisHost, LockMode, ProjectGraph, ProjectOptions};

struct Fixture {
    root: tempfile::TempDir,
    remote: PathBuf,
    application: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("create test directory");
        let remote = root.path().join("fork");
        let application = root.path().join("application");
        fs::create_dir_all(remote.join("math")).expect("create remote source directory");
        fs::create_dir_all(&application).expect("create application directory");
        write(
            &remote.join("loom.toml"),
            "schema = 2\n[module]\nname = \"utility\"\nversion = \"1.0.0\"\n",
        );
        write(
            &remote.join("math/value.loom"),
            "pub fn value() Int { 1 }\n",
        );
        write(&remote.join(".gitignore"), "generated.loom\ngenerated/\n");
        git(&remote, ["init", "-q"]);
        git(&remote, ["checkout", "-q", "-b", "main"]);
        git(&remote, ["config", "user.name", "Loom Tests"]);
        git(&remote, ["config", "user.email", "loom@example.invalid"]);
        git(&remote, ["add", "."]);
        git(&remote, ["commit", "-q", "-m", "initial"]);
        write(
            &application.join("main.loom"),
            "import forked.math.value\n\npub fn main() {\n    let observed = value()\n    discard observed\n}\n",
        );
        Self {
            root,
            remote,
            application,
        }
    }

    fn url(&self) -> String {
        file_url(&self.remote)
    }

    fn write_application(&self, dependency: &str) {
        write(
            &self.application.join("loom.toml"),
            &format!(
                "schema = 2\n[module]\nname = \"application\"\nversion = \"1.0.0\"\n[dependencies]\nforked = {{ {dependency} }}\n"
            ),
        );
    }

    fn commit_source(&self, value: i64, message: &str) -> String {
        write(
            &self.remote.join("math/value.loom"),
            &format!("pub fn value() Int {{ {value} }}\n"),
        );
        git(&self.remote, ["add", "."]);
        git(&self.remote, ["commit", "-q", "-m", message]);
        git(&self.remote, ["rev-parse", "HEAD"])
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("path has parent")).expect("create parent");
    fs::write(path, text).expect("write fixture");
}

fn git<const N: usize>(directory: &Path, arguments: [&str; N]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.hooksPath=.git/loom-disabled-hooks",
        ])
        .args(arguments)
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git fixture command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn file_url(path: &Path) -> String {
    let path = fs::canonicalize(path)
        .expect("canonical repository")
        .to_string_lossy()
        .replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn git_package(graph: &ProjectGraph) -> &loom_driver::Package {
    graph
        .packages()
        .find(|package| package.source().starts_with("git+"))
        .expect("resolved git module")
}

#[test]
fn selectors_resolve_exact_commits_and_alias_preserves_nominal_module() {
    let fixture = Fixture::new();
    let first = git(&fixture.remote, ["rev-parse", "HEAD"]);
    git(&fixture.remote, ["tag", "v1"]);
    let second = fixture.commit_source(2, "second");
    let url = fixture.url();

    for (selector, expected) in [
        (String::new(), second.as_str()),
        (", branch = \"main\"".to_owned(), second.as_str()),
        (", tag = \"v1\"".to_owned(), first.as_str()),
        (format!(", rev = \"{first}\""), first.as_str()),
    ] {
        fixture.write_application(&format!("git = \"{url}\", module = \"utility\"{selector}"));
        let graph = ProjectGraph::load(&fixture.application).expect("resolve Git selector");
        let dependency = git_package(&graph);
        assert_eq!(dependency.id().name(), "utility");
        assert!(
            dependency.source().ends_with(expected),
            "{}",
            dependency.source()
        );
    }

    fixture.write_application(&format!(
        "git = \"{url}\", module = \"utility\", branch = \"main\""
    ));
    let snapshot = AnalysisHost::new(&fixture.application)
        .expect("load aliased Git graph")
        .snapshot()
        .expect("analyze aliased Git import");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
}

#[test]
fn lock_modes_pin_then_refresh_git_commits_and_checksums() {
    let fixture = Fixture::new();
    let first = git(&fixture.remote, ["rev-parse", "HEAD"]);
    git(&fixture.remote, ["tag", "stable"]);
    let url = fixture.url();
    fixture.write_application(&format!(
        "git = \"{url}\", module = \"utility\", branch = \"main\""
    ));

    let initial = ProjectGraph::load(&fixture.application).expect("initial Git resolution");
    assert!(initial.write_lockfile().expect("write initial lock"));
    assert!(git_package(&initial).source().ends_with(&first));
    assert_eq!(git_package(&initial).checksum().map(str::len), Some(64));

    let second = fixture.commit_source(2, "second");
    let pinned = ProjectGraph::load(&fixture.application).expect("reuse Git lock pin");
    assert!(git_package(&pinned).source().ends_with(&first));

    fixture.write_application(&format!(
        "git = \"{url}\", module = \"utility\", tag = \"stable\""
    ));
    let selector_error = ProjectGraph::load_with_options(
        &fixture.application,
        &ProjectOptions {
            lock_mode: LockMode::Locked,
            ..ProjectOptions::default()
        },
    )
    .expect_err("changing a selector makes the lock out of date")
    .to_string();
    assert!(
        selector_error.contains("missing or out of date"),
        "{selector_error}"
    );
    fixture.write_application(&format!(
        "git = \"{url}\", module = \"utility\", branch = \"main\""
    ));

    let refreshed = ProjectGraph::load_with_options(
        &fixture.application,
        &ProjectOptions {
            lock_mode: LockMode::Refresh,
            ..ProjectOptions::default()
        },
    )
    .expect("refresh Git selector");
    assert!(git_package(&refreshed).source().ends_with(&second));
    assert!(refreshed.write_lockfile().expect("write refreshed lock"));
    let lock = fs::read_to_string(fixture.application.join("loom.lock")).expect("read lock");
    assert!(lock.contains(&format!("git+{url}#{second}")), "{lock}");
    assert!(lock.contains("selector = \"branch:main\""), "{lock}");
    assert!(lock.contains("checksum = "), "{lock}");

    let checksum = git_package(&refreshed)
        .checksum()
        .expect("Git module has checksum");
    let corrupted_lock = lock.replacen(checksum, &"0".repeat(64), 1);
    write(&fixture.application.join("loom.lock"), &corrupted_lock);
    let checksum_error = ProjectGraph::load(&fixture.application)
        .expect_err("lock checksum cannot replace source verification")
        .to_string();
    assert!(
        checksum_error.contains("checksum differs"),
        "{checksum_error}"
    );
    write(&fixture.application.join("loom.lock"), &lock);

    ProjectGraph::load_with_options(
        &fixture.application,
        &ProjectOptions {
            lock_mode: LockMode::Locked,
            ..ProjectOptions::default()
        },
    )
    .expect("locked Git graph is current");

    fixture.write_application(&format!(
        "git = \"{url}\", module = \"utility\", rev = \"{first}\""
    ));
    let exact = ProjectGraph::load(&fixture.application)
        .expect("explicit rev overrides a different lock pin");
    assert!(git_package(&exact).source().ends_with(&first));
    let locked_error = ProjectGraph::load_with_options(
        &fixture.application,
        &ProjectOptions {
            lock_mode: LockMode::Locked,
            ..ProjectOptions::default()
        },
    )
    .expect_err("different explicit rev makes the lock out of date")
    .to_string();
    assert!(
        locked_error.contains("missing or out of date"),
        "{locked_error}"
    );
    let refreshed_exact = ProjectGraph::load_with_options(
        &fixture.application,
        &ProjectOptions {
            lock_mode: LockMode::Refresh,
            ..ProjectOptions::default()
        },
    )
    .expect("refresh preserves an explicit rev");
    assert!(git_package(&refreshed_exact).source().ends_with(&first));
}

#[test]
fn offline_git_cache_verifies_head_and_compiler_inputs_only() {
    let fixture = Fixture::new();
    let url = fixture.url();
    fixture.write_application(&format!(
        "git = \"{url}\", module = \"utility\", branch = \"main\""
    ));
    let graph = ProjectGraph::load(&fixture.application).expect("populate Git cache");
    graph.write_lockfile().expect("write Git lock");
    let checkout = git_package(&graph).root().to_path_buf();
    let unavailable = fixture.root.path().join("fork-unavailable");
    fs::rename(&fixture.remote, &unavailable).expect("hide remote repository");

    write(&checkout.join(".DS_Store"), "irrelevant metadata");
    write(
        &checkout.join("target/loom/registry/http/cache.loom"),
        "registry cache is outside the package source set",
    );
    write(
        &checkout.join("local_test.loom"),
        "pub fn cache_only_test() Int { 1 }\n",
    );
    let offline = ProjectOptions {
        offline: true,
        ..ProjectOptions::default()
    };
    ProjectGraph::load_with_options(&fixture.application, &offline)
        .expect("irrelevant untracked file does not invalidate cache");

    write(
        &checkout.join("generated.loom"),
        "pub fn generated() Int { 1 }\n",
    );
    let ignored_error = ProjectGraph::load_with_options(&fixture.application, &offline)
        .expect_err("ignored production source invalidates cache")
        .to_string();
    assert!(
        ignored_error.contains("verified cached checkout"),
        "{ignored_error}"
    );
    fs::remove_file(checkout.join("generated.loom")).expect("remove ignored source");

    write(
        &checkout.join("generated/nested.loom"),
        "pub fn nested() Int { 1 }\n",
    );
    ProjectGraph::load_with_options(&fixture.application, &offline)
        .expect_err("production source in an ignored directory invalidates cache");
    fs::remove_dir_all(checkout.join("generated")).expect("remove ignored directory");

    write(
        &checkout.join("math/value.loom"),
        "pub fn value() Int { 999 }\n",
    );
    let modified_error = ProjectGraph::load_with_options(&fixture.application, &offline)
        .expect_err("modified compiler input invalidates cache")
        .to_string();
    assert!(
        modified_error.contains("verified cached checkout"),
        "{modified_error}"
    );

    git(&checkout, ["checkout", "--", "math/value.loom"]);
    git(
        &checkout,
        ["update-index", "--skip-worktree", "math/value.loom"],
    );
    write(
        &checkout.join("math/value.loom"),
        "pub fn value() Int { 1234 }\n",
    );
    ProjectGraph::load_with_options(&fixture.application, &offline)
        .expect_err("skip-worktree cannot hide a modified compiler input");
}

#[test]
fn invalid_sources_selectors_and_legacy_rename_are_rejected_without_secrets() {
    let fixture = Fixture::new();
    let url = fixture.url();
    let invalid = [
        "git = \"http://plain-secret@example.invalid/repo\", module = \"utility\"".to_owned(),
        "git = \"https://user:credential-secret@example.invalid/repo\", module = \"utility\""
            .to_owned(),
        "git = \"https://example.invalid/repo?token=query-secret\", module = \"utility\""
            .to_owned(),
        "git = \"ext::remote-helper-secret\", module = \"utility\"".to_owned(),
        "git = \"helper::remote-helper-secret\", module = \"utility\"".to_owned(),
        "git = \"user@-oProxyCommand=remote-option-secret:path\", module = \"utility\"".to_owned(),
        format!("git = \"{url}#fragment-secret\", module = \"utility\""),
        format!(
            "git = \"{}/remote-output-secret\", module = \"utility\"",
            file_url(fixture.root.path())
        ),
        format!("git = \"{url}\", path = \"../fork\", module = \"utility\""),
        format!("git = \"{url}\", module = \"utility\", branch = \"main\", tag = \"v1\""),
        format!("git = \"{url}\", module = \"utility\", rev = \"abc123\""),
        "path = \"../fork\", package = \"utility\"".to_owned(),
    ];

    for dependency in invalid {
        fixture.write_application(&dependency);
        let error = ProjectGraph::load(&fixture.application)
            .expect_err("invalid Git dependency is rejected")
            .to_string();
        for secret in [
            "plain-secret",
            "credential-secret",
            "query-secret",
            "remote-helper-secret",
            "remote-option-secret",
            "fragment-secret",
            "remote-output-secret",
        ] {
            assert!(
                !error.contains(secret),
                "secret leaked in diagnostic: {error}"
            );
        }
    }
}

#[test]
fn git_modules_cannot_escape_checkout_through_local_dependencies() {
    let fixture = Fixture::new();
    let url = fixture.url();
    fixture.write_application(&format!(
        "git = \"{url}\", module = \"utility\", branch = \"main\""
    ));
    write(
        &fixture.remote.join("loom.toml"),
        "schema = 2\n[module]\nname = \"utility\"\nversion = \"1.0.0\"\n[dependencies]\nlocal = { path = \"../local\" }\n",
    );
    fixture.commit_source(2, "add local dependency");
    let error = ProjectGraph::load(&fixture.application)
        .expect_err("Git module path dependency is rejected")
        .to_string();
    assert!(error.contains("cannot use path or artifact"), "{error}");

    write(
        &fixture.remote.join("loom.toml"),
        "schema = 2\n[module]\nname = \"utility\"\nversion = \"1.0.0\"\n[registries]\nlocal = \"../registry\"\n",
    );
    fixture.commit_source(3, "add path registry");
    let error = ProjectGraph::load(&fixture.application)
        .expect_err("Git module path registry is rejected")
        .to_string();
    assert!(
        error.contains("cannot configure path registries"),
        "{error}"
    );

    #[cfg(unix)]
    {
        let external_manifest = fixture.root.path().join("outside.toml");
        write(
            &external_manifest,
            "schema = 2\n[module]\nname = \"utility\"\nversion = \"1.0.0\"\n",
        );
        fs::remove_file(fixture.remote.join("loom.toml")).expect("remove regular manifest");
        std::os::unix::fs::symlink(&external_manifest, fixture.remote.join("loom.toml"))
            .expect("create manifest symlink");
        git(&fixture.remote, ["add", "-A"]);
        git(&fixture.remote, ["commit", "-q", "-m", "symlink manifest"]);
        let error = ProjectGraph::load(&fixture.application)
            .expect_err("Git module symlinked root manifest is rejected")
            .to_string();
        assert!(
            error.contains("unverifiable dependency checkout"),
            "{error}"
        );
    }
}
