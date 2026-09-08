use std::{fs, path::Path, process::Command};
mod common;
use common::success;

const SOURCE: &str = r#"
import std.io.write_text
import std.text.concat
import std.text.slice

fn say(value Text) { discard write_text(value) }

concept Crash { fn fail(self Self) }
record Broken { message Text }
impl Crash for Broken {
    fn fail(self Broken) {
        defer { say("dynamic|") }
        discard concat("move", " receiver roots")
        discard slice(self.message, 0, 1)
    }
}

fn nested(mode Int) {
    var saved = concat("be", "fore|")
    defer { say(saved) }
    defer {
        saved = concat("af", "ter|")
        discard concat("move", " cleanup roots")
        if mode == 2 { discard slice("é", 0, 1) }
    }
    if mode == 0 || mode == 2 { assert false }
    if mode == 1 { discard slice("é", 0, 1) }
    let receiver dyn Crash = Broken { message = concat("", "é") }
    receiver.fail()
}

pub fn entry(mode Int) {
    if mode == 4 {
        let message = concat("res", "umed|")
        defer { say(message) }
        discard concat("move", " after a caught fault")
        assert message == "resumed|"
    } else {
        defer { say("outer|") }
        nested(mode)
    }
}
"#;

// This native-only driver never lies on an unwinding path: the Rust boundary
// catches the generated callback's fault before control returns to C.
const DRIVER: &str = r#"
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef struct {
    const uint8_t *message;
    size_t message_len;
    const uint8_t *test_name;
    size_t test_name_len;
} FaultView;
typedef struct { uint64_t key, generation; } FrameRootId;

extern void *loom_rt_fault_boundary(void *, void (*)(void *));
extern void loom_rt_fault_read(const void *, FaultView *);
extern void loom_rt_fault_drop(void *);
extern void loom_rt_frame_roots_run(void *, void (*)(void *, const void *));
extern void loom_rt_frame_root_insert(const void *, void *, FrameRootId *);
extern uint32_t loom_rt_frame_root_read(const void *, const FrameRootId *, void **);
extern uint32_t loom_rt_frame_root_remove(const void *, const FrameRootId *);
extern void *loom_rt_box_new(size_t, void (*)(void *));
extern void loom_rt_collect(void);
extern void loom_test_resume(void *);

static int contains(const uint8_t *data, size_t length, const char *needle) {
    size_t size = strlen(needle);
    if (size > length) return 0;
    for (size_t index = 0; index <= length - size; ++index) {
        if (memcmp(data + index, needle, size) == 0) return 1;
    }
    return 0;
}

static int sentinel(const void *roots, const FrameRootId *id) {
    void *current = NULL;
    return loom_rt_frame_root_read(roots, id, &current) == 1
        && current != NULL && *(const uint64_t *)current == 42;
}

static int exercise(const void *roots) {
    void *value = loom_rt_box_new(sizeof(uint64_t), NULL);
    *(uint64_t *)value = 42;
    FrameRootId id;
    loom_rt_frame_root_insert(roots, value, &id);
    for (int64_t mode = 0; mode < 4; ++mode) {
        void *fault = loom_rt_fault_boundary(&mode, loom_test_resume);
        if (fault == NULL) {
            fprintf(stderr, "expected fault in mode %lld\n", (long long)mode);
            return 1;
        }
        // The native frames are gone: this collection also checks root-head
        // rollback, while the owned diagnostic must remain independently live.
        loom_rt_collect();
        if (!sentinel(roots, &id)) {
            fputs("fault rollback lost the outer frame root\n", stderr);
            return 4;
        }
        FaultView view;
        loom_rt_fault_read(fault, &view);
        const char *expected = mode == 0 || mode == 2
            ? "assertion failed" : "invalid text slice";
        int valid = view.message != NULL
            && contains(view.message, view.message_len, expected)
            && view.test_name == NULL && view.test_name_len == 0;
        loom_rt_fault_drop(fault);
        if (!valid) {
            fprintf(stderr, "wrong first diagnostic in mode %lld\n", (long long)mode);
            return 2;
        }
        fputs("caught|", stdout);
        int64_t normal = 4;
        fault = loom_rt_fault_boundary(&normal, loom_test_resume);
        if (fault != NULL) {
            loom_rt_fault_drop(fault);
            fputs("normal callback failed after recovery\n", stderr);
            return 3;
        }
        loom_rt_collect();
        if (!sentinel(roots, &id)) {
            fputs("normal callback lost the outer frame root\n", stderr);
            return 5;
        }
    }
    return loom_rt_frame_root_remove(roots, &id) == 1 ? 0 : 6;
}

static void run(void *context, const void *roots) {
    *(int *)context = exercise(roots);
}

int main(void) {
    setbuf(stdout, NULL);
    int status = -1;
    loom_rt_frame_roots_run(&status, run);
    loom_rt_collect();
    return status;
}
"#;

fn link(object: &Path, shim: &Path, driver: &Path, output: &Path, level: &str) {
    let windows = cfg!(all(windows, target_env = "msvc"));
    let cc = std::env::var_os("LOOM_CC")
        .unwrap_or_else(|| if windows { "clang-cl" } else { "clang" }.into());
    let runtime = std::env::var_os("LOOM_RUNTIME_LIBRARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_BIN_EXE_loom-native")).with_file_name(if windows {
                "loom_runtime.lib"
            } else {
                "libloom_runtime.a"
            })
        });
    assert!(
        runtime.is_file(),
        "build the runtime before this native test"
    );
    let mut command = Command::new(cc);
    command.current_dir(shim.parent().unwrap());
    if windows {
        command.args(["/nologo", "/MT", if level == "0" { "/Od" } else { "/O2" }]);
    } else {
        command.arg(format!("-O{level}"));
    }
    command.arg(object).arg(shim).arg(driver).arg(runtime);
    // Match native_tool.rs's host/runtime link contract, adding only the C
    // driver input. No test entry point or ABI shim enters production code.
    if windows {
        command.args([
            "/link",
            "/SUBSYSTEM:CONSOLE",
            "/INCREMENTAL:NO",
            "/OPT:REF",
            "/Brepro",
            "/STACK:8388608",
            "/DEFAULTLIB:libcmt",
            "/DEFAULTLIB:oldnames",
            "kernel32.lib",
            "ntdll.lib",
            "userenv.lib",
            "ws2_32.lib",
            "dbghelp.lib",
        ]);
        let mut destination = std::ffi::OsString::from("/OUT:");
        destination.push(output);
        command.arg(destination);
    } else {
        command.arg("-o").arg(output);
        if cfg!(target_os = "macos") {
            command.arg("-Wl,-dead_strip");
        }
        if cfg!(target_os = "linux") {
            command.args(["-Wl,--gc-sections", "-ldl", "-lpthread", "-lm"]);
        }
    }
    success(&command.output().unwrap());
}

#[test]
fn generated_callbacks_recover_first_fault_through_cleanup_and_gc() {
    let package = tempfile::tempdir().unwrap();
    fs::write(package.path().join("main.loom"), SOURCE).unwrap();
    let driver = package.path().join("driver.c");
    fs::write(&driver, DRIVER).unwrap();
    let object = package.path().join("library.o");
    let ir_path = package.path().join("library.ll");
    let shim_path = package.path().join("shim.ll");
    let executable = common::executable(package.path(), "task-fault");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                package.path().to_str().unwrap(),
                "--output",
                object.to_str().unwrap(),
                "--emit-ir",
                ir_path.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let ir = fs::read_to_string(&ir_path).unwrap();
        let exports: Vec<_> = ir
            .lines()
            .filter(|line| {
                line.starts_with("define ")
                    && line.contains("@loom.fn.")
                    && !line.contains(" internal ")
            })
            .collect();
        assert_eq!(exports.len(), 1, "fixture has exactly one public entry");
        let definition = exports[0];
        let name = definition
            .split('@')
            .nth(1)
            .unwrap()
            .split('(')
            .next()
            .unwrap();
        assert!(definition.contains("(i64 "), "entry must take one Loom Int");
        let target = ir
            .lines()
            .filter(|line| {
                line.starts_with("target datalayout = ") || line.starts_with("target triple = ")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let shim = format!(
            "{target}\ndeclare void @{name}(i64)\n\
             define void @loom_test_resume(ptr %ctx) uwtable(sync) {{\n\
             entry:\n  %mode = load i64, ptr %ctx, align 8\n\
             call void @{name}(i64 %mode)\n  ret void\n}}\n"
        );
        assert!(ir.contains("loom_rt_fault") && ir.contains("loom_rt_cleanup_push"));
        assert!(
            ir.contains("uwtable(sync)"),
            "generated functions need unwind tables"
        );
        if level == "0" {
            assert!(
                ir.contains("loom.witness."),
                "exercise the erased call path"
            );
        }
        // Link Loom's actual object; recompiling its IR with Clang would mask
        // defects in the native backend's unwind-table emission.
        fs::write(&shim_path, shim).unwrap();
        link(&object, &shim_path, &driver, &executable, level);
        for stress in ["0", "1"] {
            let output = Command::new(&executable)
                .env("LOOM_GC_STRESS", stress)
                .output()
                .unwrap();
            success(&output);
            assert!(output.stderr.is_empty(), "{level}/{stress}: {output:?}");
            assert_eq!(
                output.stdout,
                b"after|outer|caught|resumed|after|outer|caught|resumed|after|outer|caught|resumed|dynamic|after|outer|caught|resumed|",
                "{level}/{stress}: {output:?}"
            );
        }
    }
}
