use std::fs;
use std::io;

#[cfg(target_os = "linux")]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;

#[allow(dead_code)]
#[path = "../../../build-support/fingerprint.rs"]
mod fingerprint;

use fingerprint::{BuildFingerprint, assert_no_local_feature_table};

fn tree_fingerprint(root: &std::path::Path) -> io::Result<String> {
    let mut fingerprint = BuildFingerprint::new("test-build-tree-v1");
    fingerprint.workspace_input(root, &root.join("src"))?;
    Ok(fingerprint.finish())
}

#[test]
fn workspace_tree_identity_is_relocation_and_creation_order_independent() {
    let first = tempfile::tempdir().expect("first workspace");
    let second = tempfile::tempdir().expect("second workspace");
    for root in [first.path(), second.path()] {
        fs::create_dir(root.join("src")).expect("source directory");
    }
    fs::write(first.path().join("src/z.rs"), b"z\n").expect("first z");
    fs::write(first.path().join("src/a.rs"), b"a\n").expect("first a");
    fs::write(second.path().join("src/a.rs"), b"a\n").expect("second a");
    fs::write(second.path().join("src/z.rs"), b"z\n").expect("second z");

    assert_eq!(
        tree_fingerprint(first.path()).expect("first identity"),
        tree_fingerprint(second.path()).expect("second identity")
    );
}

#[test]
fn workspace_tree_identity_tracks_content() {
    let root = tempfile::tempdir().expect("workspace");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/main.rs");
    fs::write(&source, b"before\n").expect("initial source");
    let before = tree_fingerprint(root.path()).expect("initial identity");
    fs::write(source, b"after\n").expect("changed source");
    let after = tree_fingerprint(root.path()).expect("changed identity");
    assert_ne!(before, after);
}

#[test]
fn cargo_feature_table_is_parsed_instead_of_text_matched() {
    let root = tempfile::tempdir().expect("workspace");
    let manifest = root.path().join("Cargo.toml");
    fs::write(
        &manifest,
        b"[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n\n[features ] # legal TOML spacing\nextra = []\n",
    )
    .expect("manifest");
    let error = assert_no_local_feature_table(&manifest).expect_err("features must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[cfg(unix)]
#[test]
fn workspace_tree_rejects_symlink_inputs() {
    let root = tempfile::tempdir().expect("workspace");
    fs::create_dir(root.path().join("src")).expect("source directory");
    fs::write(root.path().join("real.rs"), b"source\n").expect("source");
    std::os::unix::fs::symlink("../real.rs", root.path().join("src/link.rs"))
        .expect("source symlink");
    let error = tree_fingerprint(root.path()).expect_err("symlink must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[cfg(target_os = "linux")]
#[test]
fn workspace_tree_rejects_non_utf8_paths() {
    let root = tempfile::tempdir().expect("workspace");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let name = OsString::from_vec(vec![b'n', b'o', b'n', b'u', b't', b'f', b'8', 0xff]);
    fs::write(root.path().join("src").join(name), b"source\n").expect("source");
    let error = tree_fingerprint(root.path()).expect_err("non-UTF-8 path must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
