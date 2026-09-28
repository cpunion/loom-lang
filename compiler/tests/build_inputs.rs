use std::{fs, process::Command};

mod common;
use common::{loom, success};

#[test]
fn snapshots_survive_runtime_deletion_but_recheck_contents_and_protect_outputs() {
    success(&loom(&["test", "compiler/examples/build_inputs"]));
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    let resources = package.join("resources");
    fs::create_dir_all(&resources).unwrap();
    let assets = package.join("assets");
    fs::create_dir(&assets).unwrap();
    fs::write(package.join("loom.toml"), "[module]\nname = \"app\"\n").unwrap();
    fs::write(package.join("main.loom"), "import app.resources.banner\nimport std.io.write_text\nfn main() { discard write_text(banner()) }\ntest fn snapshot() { assert banner() == \"first 雪\\n\" }").unwrap();
    fs::write(
        resources.join("input.loom"),
        "import std.build.input_file\npub fn banner() Text { input_file(\"../assets/message.txt\") }",
    )
    .unwrap();
    let input = assets.join("message.txt");
    fs::write(&input, "first 雪\n").unwrap();
    let cache = directory.path().join("cache");
    let artifact = common::executable(directory.path(), "compiled");
    let receipt = directory.path().join("build.receipt");
    let ir = directory.path().join("app.ll");
    let build = |level: &str| {
        common::command(&["build"])
            .arg(&package)
            .arg("--output")
            .arg(&artifact)
            .arg("--receipt")
            .arg(&receipt)
            .arg("--frontend-cache")
            .arg(&cache)
            .env("LOOM_NATIVE_TIMINGS", "1")
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap()
    };
    success(&loom(&["check", package.to_str().unwrap()]));
    success(&loom(&["test", package.to_str().unwrap()]));
    success(&loom(&[
        "build",
        package.to_str().unwrap(),
        "--output",
        artifact.to_str().unwrap(),
        "--emit-ir",
        ir.to_str().unwrap(),
    ]));
    assert!(!fs::read_to_string(&ir).unwrap().contains("input_file"));
    for (level, hit) in [("0", false), ("2", true)] {
        let output = build(level);
        success(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains(if hit {
            "frontend hit"
        } else {
            "frontend miss"
        }));
        let run = Command::new(&artifact)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap();
        success(&run);
        assert_eq!(run.stdout, "first 雪\n".as_bytes());
    }
    let original_receipt = fs::read_to_string(&receipt).unwrap();
    assert!(original_receipt.starts_with("loom-build-receipt 3\n"));
    assert!(original_receipt.contains("\ninput-count 1\ninput "));
    // Output/IR/receipt preflights also run on frontend cache hits.
    for flag in ["--output", "--emit-ir", "--receipt"] {
        let output = common::command(&["build"])
            .arg(&package)
            .arg("--frontend-cache")
            .arg(&cache)
            .arg(flag)
            .arg(&input)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot replace a build input"));
        assert_eq!(fs::read_to_string(&input).unwrap(), "first 雪\n");
    }
    // Same byte count and source tree: the input bytes, not a sidecar or mtime,
    // must invalidate the checked closure and bind the new receipt.
    fs::write(&input, "other 雪\n").unwrap();
    let changed = build("2");
    success(&changed);
    assert!(String::from_utf8_lossy(&changed.stderr).contains("frontend miss"));
    assert_ne!(fs::read_to_string(&receipt).unwrap(), original_receipt);
    fs::remove_file(&input).unwrap();
    let run = Command::new(&artifact).output().unwrap();
    success(&run);
    assert_eq!(run.stdout, "other 雪\n".as_bytes());
    let saved = fs::read(&artifact).unwrap();
    let failed = build("2");
    assert!(!failed.status.success());
    assert_eq!(fs::read(&artifact).unwrap(), saved);
}

#[test]
fn build_inputs_stay_inside_the_declaring_module() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("loom.toml"), "[module]\nname = \"app\"\n").unwrap();
    fs::write(directory.path().join("outside.txt"), "outside").unwrap();
    fs::write(
        package.join("main.loom"),
        "import std.build.input_file\nfn main() { discard input_file(\"../outside.txt\") }",
    )
    .unwrap();
    let output = loom(&["check", package.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("escapes its module"),
        "{output:?}"
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            directory.path().join("outside.txt"),
            package.join("linked.txt"),
        )
        .unwrap();
        fs::write(
            package.join("main.loom"),
            "import std.build.input_file\nfn main() { discard input_file(\"linked.txt\") }",
        )
        .unwrap();
        let output = loom(&["check", package.to_str().unwrap()]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("escapes its module"));
    }
}

#[test]
fn binary_inputs_embed_compact_fresh_buffers_and_bind_caches_and_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("app");
    fs::create_dir(&package).unwrap();
    let source = package.join("main.loom");
    fs::write(
        &source,
        r#"
import std.build.input_bytes
import std.bytes.length
import std.bytes.get
import std.bytes.set
import std.bytes.push
import std.io.write_text
import std.encoding.hex.encode
import std.time.sleep_ms

fn data() Bytes {
    input_bytes("asset.bin")
}

fn last_byte(bytes Bytes) Int {
    get(bytes, length(bytes) - 1)
}

async fn main() {
    let expected = comptime { last_byte(input_bytes("asset.bin")) }
    let first = data()
    let second = data()
    assert length(first) == 4096 && get(first, 1) == 1
    set(first, 1, 99)
    assert get(second, 1) == 1
    push(first, 17)
    assert length(second) == 4096
    sleep_ms(1).await
    assert get(first, 1) == 99 && get(second, 1) == 1
    assert last_byte(second) == expected
    let shared = comptime {
        let bytes = input_bytes("asset.bin")
        (bytes, bytes)
    }
    set(shared.0, 1, 77)
    assert get(shared.1, 1) == 77 && get(data(), 1) == 1
    assert length(input_bytes("empty.bin")) == 0
    discard write_text(encode(second))
}
test fn embedded_size() {
    assert length(data()) == 4096
}
"#,
    )
    .unwrap();
    let asset = package.join("asset.bin");
    let mut bytes: Vec<u8> = (0..4096).map(|index| index as u8).collect();
    fs::write(&asset, &bytes).unwrap();
    fs::write(package.join("empty.bin"), []).unwrap();
    let artifact = common::executable(directory.path(), "binary_input");
    let ir = directory.path().join("binary_input.ll");
    let cache = directory.path().join("cache");
    let receipt = directory.path().join("receipt");
    let build = |level| {
        common::command(&["build"])
            .arg(&package)
            .arg("--output")
            .arg(&artifact)
            .arg("--receipt")
            .arg(&receipt)
            .arg("--frontend-cache")
            .arg(&cache)
            .env("LOOM_NATIVE_TIMINGS", "1")
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap()
    };
    success(&loom(&["check", package.to_str().unwrap()]));
    success(&loom(&["test", package.to_str().unwrap()]));
    success(
        &common::command(&["build"])
            .arg(&package)
            .arg("--output")
            .arg(&artifact)
            .arg("--emit-ir")
            .arg(&ir)
            .env("LOOM_OPT_LEVEL", "0")
            .output()
            .unwrap(),
    );
    let emitted = fs::read_to_string(&ir).unwrap();
    assert!(emitted.contains("@loom_rt_bytes_from_static("));
    // The one explicit push above remains; embedding 4 KiB must not emit
    // thousands of reserve/growth sites or runtime file reads, even at O0.
    assert!(
        emitted
            .matches("call void @loom_rt_bytes_reserve_one(")
            .count()
            < 8
    );
    assert!(!emitted.contains("@loom_rt_file_open("));
    assert!(emitted.len() < 250_000);
    for (level, hit) in [("0", false), ("2", true)] {
        let output = build(level);
        success(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains(if hit {
            "frontend hit"
        } else {
            "frontend miss"
        }));
        let run = Command::new(&artifact)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap();
        success(&run);
        assert_eq!(
            String::from_utf8(run.stdout).unwrap(),
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
    }
    let original_receipt = fs::read(&receipt).unwrap();
    bytes[4095] = 42;
    fs::write(&asset, &bytes).unwrap();
    let changed = build("2");
    success(&changed);
    assert!(String::from_utf8_lossy(&changed.stderr).contains("frontend miss"));
    assert_ne!(fs::read(&receipt).unwrap(), original_receipt);
    fs::remove_file(&asset).unwrap();
    let run = Command::new(&artifact)
        .env("LOOM_GC_STRESS", "1")
        .output()
        .unwrap();
    success(&run);
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    assert!(!build("2").status.success());
    fs::write(&asset, &bytes).unwrap();
    fs::write(
        &source,
        r#"
import std.build.input_file

fn main() {
    discard input_file("asset.bin")
}
"#,
    )
    .unwrap();
    let rejected = loom(&["check", package.to_str().unwrap()]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("not valid UTF-8"));
}
