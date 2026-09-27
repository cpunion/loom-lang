use std::{fs, path::Path, process::Output};

mod common;
use common::success;

fn cached(mode: &str, package: &Path, cache: &Path, extra: &[&str]) -> Output {
    common::command(&[mode])
        .arg(package)
        .arg("--frontend-cache")
        .arg(cache)
        .args(extra)
        .env("LOOM_NATIVE_TIMINGS", "1")
        .output()
        .unwrap()
}

fn checked(output: &Output, hit: bool) {
    success(output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let status = if hit { "hit" } else { "miss" };
    assert_eq!(
        stderr.matches("loom cache: frontend ").count(),
        1,
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("loom cache: frontend {status}\n")),
        "{stderr}"
    );
}

#[test]
fn frontend_reuses_checked_artifacts_without_skipping_outputs_tests_or_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("source 雪");
    let cache = directory.path().join("cache");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("main.loom"), "fn answer() Int ensures result == 42 { 42 }\nfn main() { assert answer() == 42 }\ntest fn embedded() { assert answer() == 42 }").unwrap();
    fs::write(
        package.join("main_test.loom"),
        "test fn external() { assert answer() == 42 }",
    )
    .unwrap();
    for hit in [false, true] {
        checked(&cached("check", &package, &cache, &[]), hit);
    }
    let uncached = common::loom(&["emit-checked", package.to_str().unwrap()]);
    success(&uncached);
    for hit in [false, true] {
        let output = cached("emit-checked", &package, &cache, &[]);
        checked(&output, hit);
        assert_eq!(output.stdout, uncached.stdout);
    }
    let executable = common::executable(directory.path(), "app");
    let receipt = directory.path().join("build.receipt");
    checked(
        &cached(
            "build",
            &package,
            &cache,
            &[
                "--output",
                executable.to_str().unwrap(),
                "--receipt",
                receipt.to_str().unwrap(),
                "--object-cache",
                cache.to_str().unwrap(),
            ],
        ),
        true,
    );
    assert!(
        fs::read_to_string(&receipt)
            .unwrap()
            .contains("checked-sha256 ")
    );
    success(&std::process::Command::new(&executable).output().unwrap());
    fs::remove_file(&executable).unwrap();
    // Frontend and object hits still recreate the final artifact and receipt.
    let built = cached(
        "build",
        &package,
        &cache,
        &[
            "--output",
            executable.to_str().unwrap(),
            "--receipt",
            receipt.to_str().unwrap(),
            "--object-cache",
            cache.to_str().unwrap(),
        ],
    );
    checked(&built, true);
    assert!(String::from_utf8_lossy(&built.stderr).contains("loom cache: hit\n"));
    assert!(executable.is_file());
    let ir = directory.path().join("app.ll");
    checked(
        &cached(
            "build",
            &package,
            &cache,
            &["--emit-ir", ir.to_str().unwrap()],
        ),
        true,
    );
    assert!(fs::read_to_string(ir).unwrap().contains("@main("));
    checked(&cached("run", &package, &cache, &[]), true);
    for hit in [false, true] {
        let output = cached("test", &package, &cache, &[]);
        checked(&output, hit);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "2 tests passed\n"
        );
    }
    fs::write(
        package.join("main_test.loom"),
        "test fn external() { assert false }",
    )
    .unwrap();
    checked(&cached("build", &package, &cache, &[]), true);
    let failed = cached("test", &package, &cache, &[]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("frontend miss"));
}

#[test]
fn frontend_invalidates_real_source_membership_dependencies_and_proofs() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let dependency = directory.path().join("dep");
    let fork = directory.path().join("fork");
    let cache = directory.path().join("cache");
    for path in [&package, &dependency, &fork] {
        fs::create_dir(path).unwrap();
    }
    let manifest = |path: &str| {
        format!("[module]\nname = \"app\"\n[dependencies.dep]\npath = \"../{path}\"\n")
    };
    fs::write(package.join("loom.toml"), manifest("dep")).unwrap();
    for path in [&dependency, &fork] {
        fs::write(path.join("loom.toml"), "[module]\nname = \"dep\"\n").unwrap();
        fs::write(
            path.join("lib.loom"),
            "pub fn value() Int ensures result == 42 { 42 }",
        )
        .unwrap();
    }
    fs::write(
        package.join("main.loom"),
        "import dep.value\nfn main() { assert value() == 42 }",
    )
    .unwrap();
    checked(&cached("check", &package, &cache, &[]), false);
    checked(&cached("check", &package, &cache, &[]), true);
    // Same bytes at a different module instance are a different semantic basis.
    fs::write(package.join("loom.toml"), manifest("fork")).unwrap();
    checked(&cached("check", &package, &cache, &[]), false);
    fs::write(
        fork.join("lib.loom"),
        "pub fn value() Int ensures result == 42 { 41 }",
    )
    .unwrap();
    let failed = cached("check", &package, &cache, &[]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("frontend miss"));
    fs::write(
        fork.join("lib.loom"),
        "pub fn value() Int ensures result == 42 { 42 }",
    )
    .unwrap();
    checked(&cached("check", &package, &cache, &[]), true);
    let extra = package.join("extra.loom");
    fs::write(&extra, "fn invalid() Int { true }").unwrap();
    assert!(!cached("check", &package, &cache, &[]).status.success());
    fs::write(&extra, "fn extra() Int { 1 }").unwrap();
    checked(&cached("check", &package, &cache, &[]), false);
    fs::remove_file(extra).unwrap();
    checked(&cached("check", &package, &cache, &[]), true);
    // Loading cannot be bypassed even with a warm successful artifact.
    fs::write(fork.join("loom.toml"), "not a manifest").unwrap();
    let invalid = cached("check", &package, &cache, &[]);
    assert!(!invalid.status.success());
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("frontend hit"));
}

#[test]
fn frontend_rechecks_damaged_bundles_and_never_publishes_failed_checks() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("cache");
    fs::write(directory.path().join("main.loom"), "fn main() {}").unwrap();
    checked(
        &cached("emit-checked", directory.path(), &cache, &[]),
        false,
    );
    let entries: Vec<_> = fs::read_dir(cache.join("checked-v2"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let bundle = &entries[0];
    let original = fs::read(bundle).unwrap();
    let mut damaged = original.clone();
    let position = damaged.len() - 33;
    damaged[position] ^= 1;
    fs::write(bundle, damaged).unwrap();
    checked(
        &cached("emit-checked", directory.path(), &cache, &[]),
        false,
    );
    assert_eq!(fs::read(bundle).unwrap(), original);
    checked(&cached("emit-checked", directory.path(), &cache, &[]), true);
    fs::write(
        directory.path().join("main.loom"),
        "fn bad() Int ensures result == 1 { 2 }\nfn main() {}",
    )
    .unwrap();
    for _ in 0..2 {
        assert!(
            !cached("emit-checked", directory.path(), &cache, &[])
                .status
                .success()
        );
    }
    assert_eq!(fs::read_dir(cache.join("checked-v2")).unwrap().count(), 1);
    for extra in [
        vec!["--frontend-cache", ""],
        vec!["--frontend-cache", "other"],
    ] {
        let invalid = cached("check", directory.path(), &cache, &extra);
        assert!(!invalid.status.success());
        assert!(String::from_utf8_lossy(&invalid.stderr).contains("--frontend-cache"));
    }
    assert!(
        !cached("resolve", directory.path(), &cache, &[])
            .status
            .success()
    );
}
