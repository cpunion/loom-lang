use std::fs;
use std::path::Path;
mod common;

fn run_fixture(directory: &Path, source: &str) {
    fs::write(directory.join("main.loom"), source).unwrap();
    common::success(&common::managed(
        &["run", directory.to_str().unwrap()],
        directory,
    ));
}

#[test]
fn filesystem_enumeration_metadata_and_canonical_paths() {
    let directory = tempfile::tempdir().unwrap();
    let entries = directory.path().join("entries");
    fs::create_dir(&entries).unwrap();
    fs::create_dir(entries.join("nested")).unwrap();
    fs::create_dir(directory.path().join("empty")).unwrap();
    for name in ["a.loom", "z.loom", "é.loom"] {
        fs::write(entries.join(name), "").unwrap();
    }
    for index in (0..40).rev() {
        fs::write(entries.join(format!("entry{index:02}")), "").unwrap();
    }
    let canonical = fs::canonicalize(entries.join("a.loom")).unwrap();
    run_fixture(
        directory.path(),
        &format!(
            r#"
import std.fs.entries
import std.fs.kind
import std.fs.canonicalize
import std.fs.FileKind
import std.fs.FsError
import std.result.Result
import std.list.length
import std.list.get
import std.text.concat
import std.int.to_text

fn main() {{
    let names = match entries(concat("ent", "ries")) {{
        Result.Ok(value) => value
        Result.Err(_) => {{ assert false
            return }}
    }}
    assert length(names) == 44
    assert get(names, 0) == "a.loom"
    var index = 0
    while index < 40 {{
        let prefix = if index < 10 {{ "entry0" }} else {{ "entry" }}
        assert get(names, index + 1) == concat(prefix, to_text(index))
        index = index + 1
    }}
    assert get(names, 41) == "nested"
    assert get(names, 42) == "z.loom"
    assert get(names, 43) == "é.loom"
    assert match entries("empty") {{
        Result.Ok(value) => length(value) == 0
        Result.Err(_) => false
    }}
    assert match kind("entries/a.loom") {{
        Result.Ok(value) => value == FileKind.File
        Result.Err(_) => false
    }}
    assert match kind("entries") {{
        Result.Ok(value) => value == FileKind.Directory
        Result.Err(_) => false
    }}
    assert match kind("missing") {{
        Result.Ok(_) => false
        Result.Err(error) => error == FsError.Metadata
    }}
    assert match entries("entries/a.loom") {{
        Result.Ok(_) => false
        Result.Err(error) => error == FsError.Read
    }}
    assert match canonicalize(concat("entries/", "nested/../a.loom")) {{
        Result.Ok(value) => value == {canonical:?}
        Result.Err(_) => false
    }}
    assert match canonicalize("missing") {{
        Result.Ok(_) => false
        Result.Err(error) => error == FsError.Canonical
    }}
}}
"#,
            canonical = canonical.to_str().unwrap(),
        ),
    );
}

// APFS rejects non-UTF-8 filenames before the runtime can observe them.
#[cfg(target_os = "linux")]
#[test]
fn filesystem_rejects_non_utf8_names_without_rewriting_them() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let entries = directory.path().join("entries");
    fs::create_dir(&entries).unwrap();
    let invalid = entries.join(OsString::from_vec(vec![0xff]));
    fs::write(&invalid, "").unwrap();
    symlink(&invalid, directory.path().join("alias")).unwrap();
    run_fixture(
        directory.path(),
        r#"
import std.fs.entries
import std.fs.canonicalize
import std.fs.FsError
import std.result.Result

fn main() {
    assert match entries("entries") {
        Result.Ok(_) => false
        Result.Err(error) => error == FsError.Utf8
    }
    assert match canonicalize("alias") {
        Result.Ok(_) => false
        Result.Err(error) => error == FsError.Utf8
    }
}
"#,
    );
}
