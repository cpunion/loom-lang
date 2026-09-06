use std::{fs, path::Path, process::Command};
mod common;
use common::{loom, success};

fn write(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn source_path_dependencies_pass_native_commands_without_dependency_tests() {
    let package = common::root().join("compiler/examples/modules/app");
    let package = package.to_str().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "modules");
    let ir = temporary.path().join("modules.ll");
    success(&loom(&["check", package]));
    success(
        &common::command(&[
            "build",
            package,
            "--output",
            executable.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(&Command::new(executable).output().unwrap());
    success(&loom(&["run", package]));
    let tests = loom(&["test", package]);
    success(&tests);
    assert_eq!(tests.stdout, b"1 tests passed\n");
    assert!(!fs::read_to_string(ir).unwrap().contains("9001"));
}

#[test]
fn dependency_visibility_and_module_identity_follow_the_declaring_manifest() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(
        root,
        "app/loom.toml",
        "[module]\nname='app'\n[dependencies.codec]\npath='../codec'\n[dependencies.other]\npath='../other'\n[dependencies.unused]\npath='../not-present'\n",
    );
    let main = "import codec.answer\nimport other.answer\nfn main() { assert codec.answer() == 42 && other.answer() == 42 }";
    write(root, "app/main.loom", main);
    write(root, "app/main_test.loom", "test fn app_test() { main() }");
    write(
        root,
        "codec/loom.toml",
        "[module]\nname='codec'\n[dependencies.seed]\npath='../seed'\n",
    );
    write(
        root,
        "codec/main.loom",
        "import seed.value\npub fn answer() Int { value() }\ntest fn embedded() { assert false }",
    );
    write(
        root,
        "codec/main_test.loom",
        "test fn excluded() { assert false }",
    );
    write(
        root,
        "other/loom.toml",
        "[module]\nname='other'\n[dependencies.seed]\npath='../seed/.'\n",
    );
    write(
        root,
        "other/main.loom",
        "import seed.value\npub fn answer() Int { value() }",
    );
    write(root, "seed/loom.toml", "[module]\nname='seed'\n");
    write(root, "seed/main.loom", "pub fn value() Int { 42 }");
    let package = root.join("app");
    let package = package.to_str().unwrap();
    success(&loom(&["run", package]));
    let output = loom(&["test", package]);
    success(&output);
    assert_eq!(output.stdout, b"1 tests passed\n");

    // Loading a transitive dependency does not make it a direct dependency.
    write(
        root,
        "app/main.loom",
        "import codec.answer\nimport seed.value\nfn main() { assert answer() == value() }",
    );
    let output = loom(&["check", package]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("seed"));

    // Distinct roots with the same declared name must not silently share scope.
    write(root, "app/main.loom", main);
    write(
        root,
        "other/loom.toml",
        "[module]\nname='other'\n[dependencies.seed]\npath='../alternate'\n",
    );
    write(root, "alternate/loom.toml", "[module]\nname='seed'\n");
    write(root, "alternate/main.loom", "pub fn value() Int { 99 }");
    let output = loom(&["check", package]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("seed"));
}
