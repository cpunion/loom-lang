use std::fs;

use loom_codegen_llvm::{
    NATIVE_RUNTIME_ABI, RUNTIME_BUNDLE_MANIFEST, RuntimeBundle, export_native_runtime_bundle,
    native_target_identity,
};

#[test]
fn exported_host_runtime_bundle_round_trips_with_exact_identity() {
    let temporary = tempfile::tempdir().expect("runtime bundle test directory");
    let output = temporary.path().join("runtime");
    let exported = export_native_runtime_bundle(&output).expect("export host runtime");
    let target = native_target_identity().expect("host target");
    let loaded = RuntimeBundle::load(&output, &target).expect("load exported runtime");

    assert_eq!(loaded.target_triple(), target.triple);
    assert_eq!(loaded.data_layout(), target.data_layout);
    assert_eq!(
        loaded.archive(),
        fs::canonicalize(&exported.archive).expect("canonical exported archive")
    );
    assert_eq!(loaded.archive_sha256(), exported.archive_sha256);
    assert!(loaded.identity().contains(loaded.archive_sha256()));
    assert_eq!(exported.runtime_abi, NATIVE_RUNTIME_ABI);
    assert_eq!(
        fs::read(&exported.manifest).expect("runtime manifest"),
        fs::read(output.join(RUNTIME_BUNDLE_MANIFEST)).expect("fixed manifest path")
    );
    assert_eq!(
        export_native_runtime_bundle(&output)
            .expect_err("export never overwrites an existing bundle")
            .code(),
        "RuntimeBundleWriteFailed"
    );
}

#[cfg(unix)]
fn write_linker(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::write(
        path,
        format!(
            "#!/bin/sh\nset -eu\nif [ \"${{1-}}\" = \"--version\" ]; then printf 'loom test linker v1\\n'; exit 0; fi\n{body}\n"
        ),
    )
    .expect("write test linker");
    let mut permissions = fs::metadata(path)
        .expect("test linker metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make test linker executable");
}

#[cfg(unix)]
fn directory_entries(path: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut entries = fs::read_dir(path)
        .expect("read output directory")
        .map(|entry| entry.expect("output entry").file_name())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

#[cfg(unix)]
#[test]
fn failed_or_invalid_link_preserves_output_and_cleans_adjacent_staging() {
    use loom_codegen_llvm::{RuntimeLinker, link_object_with_runtime_bundle};

    let temporary = tempfile::tempdir().expect("runtime link test directory");
    let bundle_path = temporary.path().join("runtime");
    export_native_runtime_bundle(&bundle_path).expect("export host runtime");
    let target = native_target_identity().expect("host target");
    let bundle = RuntimeBundle::load(&bundle_path, &target).expect("load host runtime");
    let object = temporary.path().join("input.o");
    let output = temporary.path().join("program");
    fs::write(&object, b"test object").expect("write test object");
    fs::write(&output, b"known-good artifact").expect("write prior artifact");

    let failing_program = temporary.path().join("failing-linker");
    write_linker(
        &failing_program,
        "printf 'deliberate failure\\n' >&2; exit 7",
    );
    let failing = RuntimeLinker::load(&failing_program).expect("identify failing linker");
    let before = directory_entries(temporary.path());
    assert_eq!(
        link_object_with_runtime_bundle(&object, &output, &bundle, &failing)
            .expect_err("link failure")
            .code(),
        "NativeLinkFailed"
    );
    assert_eq!(
        fs::read(&output).expect("preserved prior output"),
        b"known-good artifact"
    );
    assert_eq!(directory_entries(temporary.path()), before);

    let invalid_program = temporary.path().join("invalid-linker");
    write_linker(
        &invalid_program,
        "while [ \"$#\" -gt 0 ]; do if [ \"$1\" = \"-o\" ]; then shift; ln -sf missing-target \"$1\"; exit 0; fi; shift; done; exit 9",
    );
    let invalid = RuntimeLinker::load(&invalid_program).expect("identify invalid linker");
    let before = directory_entries(temporary.path());
    assert_eq!(
        link_object_with_runtime_bundle(&object, &output, &bundle, &invalid)
            .expect_err("reject symlink output")
            .code(),
        "ArtifactWriteFailed"
    );
    assert_eq!(
        fs::read(&output).expect("preserved output after validation failure"),
        b"known-good artifact"
    );
    assert_eq!(directory_entries(temporary.path()), before);

    let mutating_program = temporary.path().join("mutating-linker");
    write_linker(
        &mutating_program,
        "archive=$2; printf 'tampered runtime' > \"$archive\"; while [ \"$#\" -gt 0 ]; do if [ \"$1\" = \"-o\" ]; then shift; printf 'untrusted output' > \"$1\"; exit 0; fi; shift; done; exit 9",
    );
    let mutating = RuntimeLinker::load(&mutating_program).expect("identify mutating linker");
    let before = directory_entries(temporary.path());
    assert_eq!(
        link_object_with_runtime_bundle(&object, &output, &bundle, &mutating)
            .expect_err("reject runtime changed during link")
            .code(),
        "RuntimeBundleChecksumMismatch"
    );
    assert_eq!(
        fs::read(&output).expect("preserved output after revalidation failure"),
        b"known-good artifact"
    );
    assert_eq!(directory_entries(temporary.path()), before);
}

#[test]
#[allow(clippy::too_many_lines)]
fn runtime_bundle_manifest_rejects_unknown_fields_and_target_or_abi_mismatch() {
    let temporary = tempfile::tempdir().expect("runtime bundle test directory");
    let output = temporary.path().join("runtime");
    export_native_runtime_bundle(&output).expect("export host runtime");
    let target = native_target_identity().expect("host target");
    let manifest_path = output.join(RUNTIME_BUNDLE_MANIFEST);
    let original = fs::read(&manifest_path).expect("runtime manifest");

    let mut manifest = serde_json::from_slice::<serde_json::Value>(&original).expect("manifest");
    manifest
        .as_object_mut()
        .expect("manifest object")
        .insert("unknown".to_owned(), serde_json::json!(true));
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("encode unknown field"),
    )
    .expect("write unknown field");
    assert_eq!(
        RuntimeBundle::load(&output, &target)
            .expect_err("unknown field")
            .code(),
        "RuntimeBundleInvalid"
    );

    let duplicate = String::from_utf8(original.clone())
        .expect("UTF-8 manifest")
        .replacen(
            "\"schema_version\": 1,",
            "\"schema_version\": 1,\n  \"schema_version\": 1,",
            1,
        );
    fs::write(&manifest_path, duplicate).expect("write duplicate manifest field");
    assert_eq!(
        RuntimeBundle::load(&output, &target)
            .expect_err("duplicate manifest field")
            .code(),
        "RuntimeBundleInvalid"
    );

    let mut manifest = serde_json::from_slice::<serde_json::Value>(&original).expect("manifest");
    manifest["target_triple"] = serde_json::json!("not-the-host");
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("encode target mismatch"),
    )
    .expect("write target mismatch");
    assert_eq!(
        RuntimeBundle::load(&output, &target)
            .expect_err("target mismatch")
            .code(),
        "RuntimeBundleTargetMismatch"
    );

    let mut manifest = serde_json::from_slice::<serde_json::Value>(&original).expect("manifest");
    manifest["runtime_abi"] = serde_json::json!("wrong-abi");
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("encode ABI mismatch"),
    )
    .expect("write ABI mismatch");
    assert_eq!(
        RuntimeBundle::load(&output, &target)
            .expect_err("ABI mismatch")
            .code(),
        "RuntimeBundleAbiMismatch"
    );

    let mut manifest = serde_json::from_slice::<serde_json::Value>(&original).expect("manifest");
    manifest["link_args"] = serde_json::json!(["-o"]);
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("encode unsafe link argument"),
    )
    .expect("write unsafe link argument");
    assert_eq!(
        RuntimeBundle::load(&output, &target)
            .expect_err("unsafe link argument")
            .code(),
        "RuntimeBundleInvalid"
    );

    fs::write(&manifest_path, vec![b' '; 65 * 1024]).expect("write oversized manifest");
    assert_eq!(
        RuntimeBundle::load(&output, &target)
            .expect_err("oversized manifest")
            .code(),
        "RuntimeBundleInvalid"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn runtime_bundle_rejects_unsafe_paths_tampering_extras_and_symlinks() {
    let temporary = tempfile::tempdir().expect("runtime bundle test directory");
    let output = temporary.path().join("runtime");
    let exported = export_native_runtime_bundle(&output).expect("export host runtime");
    let target = native_target_identity().expect("host target");
    let manifest_path = output.join(RUNTIME_BUNDLE_MANIFEST);
    let original_manifest = fs::read(&manifest_path).expect("runtime manifest");
    let original_archive = fs::read(&exported.archive).expect("runtime archive");

    for unsafe_path in [
        "../outside.a",
        "C:runtime.a",
        "NUL.a",
        "bad\nruntime.a",
        "runtime.",
        RUNTIME_BUNDLE_MANIFEST,
    ] {
        let mut manifest =
            serde_json::from_slice::<serde_json::Value>(&original_manifest).expect("manifest");
        manifest["archive"] = serde_json::json!(unsafe_path);
        fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).expect("encode unsafe path"),
        )
        .expect("write unsafe path");
        assert_eq!(
            RuntimeBundle::load(&output, &target)
                .expect_err("unsafe archive path")
                .code(),
            "RuntimeBundleInvalid",
            "accepted unsafe archive path {unsafe_path:?}"
        );
    }

    fs::write(&manifest_path, &original_manifest).expect("restore manifest");
    fs::write(&exported.archive, b"tampered archive").expect("tamper archive");
    assert_eq!(
        RuntimeBundle::load(&output, &target)
            .expect_err("archive checksum")
            .code(),
        "RuntimeBundleChecksumMismatch"
    );

    fs::write(&exported.archive, &original_archive).expect("restore archive");
    fs::write(output.join("unexpected"), b"extra").expect("write extra file");
    assert_eq!(
        RuntimeBundle::load(&output, &target)
            .expect_err("extra bundle file")
            .code(),
        "RuntimeBundleInvalid"
    );
    fs::remove_file(output.join("unexpected")).expect("remove extra file");

    let oversized = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&exported.archive)
        .expect("open archive for sparse oversize test");
    oversized
        .set_len(256 * 1024 * 1024 + 1)
        .expect("make sparse oversized archive");
    drop(oversized);
    assert_eq!(
        RuntimeBundle::load(&output, &target)
            .expect_err("oversized archive")
            .code(),
        "RuntimeBundleInvalid"
    );
    fs::write(&exported.archive, &original_archive).expect("restore archive after size test");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = temporary.path().join("outside.a");
        fs::write(&outside, &original_archive).expect("outside archive");
        fs::remove_file(&exported.archive).expect("remove real archive");
        symlink(&outside, &exported.archive).expect("link archive");
        assert_eq!(
            RuntimeBundle::load(&output, &target)
                .expect_err("symlink archive")
                .code(),
            "RuntimeBundleInvalid"
        );

        fs::remove_file(&exported.archive).expect("remove archive symlink");
        fs::write(&exported.archive, &original_archive).expect("restore archive for root symlink");
        let root_link = temporary.path().join("runtime-link");
        symlink(&output, &root_link).expect("link runtime root");
        assert_eq!(
            RuntimeBundle::load(&root_link, &target)
                .expect_err("symlink bundle root")
                .code(),
            "RuntimeBundleInvalid"
        );
    }
}
