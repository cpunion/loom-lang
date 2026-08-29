#![allow(clippy::default_trait_access)]

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use inkwell::targets::TargetMachine;
use loom_codegen_llvm::{
    EmitOptions, OptimizationProfile, emit_native_object, native_object_fingerprint,
    target_identity,
};
use loom_driver::AnalysisHost;
use loom_interpreter::Interpreter;
use loom_mir::{
    Block, CallArgument, CallPlan, CheckedProgram, Constant, Expr, ExprKind, Function, FunctionId,
    Program, Type,
};

mod support;
#[cfg(unix)]
use support::run_with_closed_stdout;
use support::{emit_native, run_with_read_only_stdout};

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
const CROSS_TRIPLE: &str = "x86_64-unknown-linux-gnu";
#[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
const CROSS_TRIPLE: &str = "aarch64-unknown-linux-gnu";

fn assert_exact_stdout_ir(ir: &str) {
    assert!(ir.contains("@loom_runtime_stdout_write_v1"), "{ir}");
    for forbidden in ["@puts", "@printf", "@loom.runtime.print"] {
        assert!(!ir.contains(forbidden), "unexpected `{forbidden}`:\n{ir}");
    }
}

#[test]
fn raw_legacy_codegen_rejects_non_language_run_roots_before_fingerprinting_or_emission() {
    for (label, source) in [
        (
            "non-unit",
            "module invalid_result\n\npub fn main() Int { 1 }\n",
        ),
        (
            "parameter",
            "module invalid_parameter\n\npub fn main(value Int) { discard value\n}\n",
        ),
    ] {
        let project = tempfile::tempdir().expect("create invalid root project");
        std::fs::write(project.path().join("main.loom"), source)
            .expect("write invalid root source");
        let snapshot = AnalysisHost::new(project.path())
            .expect("load invalid root project")
            .snapshot()
            .expect("analyze invalid root project");
        assert!(
            !snapshot.has_errors(),
            "{label}: {:#?}",
            snapshot.diagnostics()
        );
        let program = snapshot.executable().expect("lower invalid root MIR");
        let options = EmitOptions::run("main");
        let fingerprint = native_object_fingerprint(program, &options)
            .expect_err("invalid raw root cannot receive an object identity");
        assert_eq!(fingerprint.code(), "InvalidRootSignature", "{label}");
        let emission = emit_native_object(program, &project.path().join("invalid.o"), &options)
            .expect_err("invalid raw root cannot reach LLVM emission");
        assert_eq!(emission.code(), "InvalidRootSignature", "{label}");
    }
}

#[test]
fn emits_links_and_runs_a_native_unit_entry() {
    let program = unit_program();
    let directory = tempfile::tempdir().expect("create temp directory");
    let executable = directory.path().join("program");
    let artifact = emit_native(&program, &executable, &EmitOptions::run("main"))
        .expect("emit native executable");
    assert_eq!(artifact.functions, 1);
    let output = Command::new(&executable).output().expect("run executable");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
fn legacy_run_returns_failure_when_exact_stdout_is_not_writable() {
    let program = unit_program();
    let directory = tempfile::tempdir().expect("create temp directory");
    let executable = directory.path().join("program");
    let ir = directory.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(&program, &executable, &options).expect("emit legacy native executable");

    let llvm = std::fs::read_to_string(ir).expect("read legacy stdout LLVM IR");
    assert_exact_stdout_ir(&llvm);
    let write = llvm
        .lines()
        .find(|line| line.contains("call i32 @loom_runtime_stdout_write_v1"))
        .expect("legacy Unit harness must call the exact stdout ABI");
    assert!(
        write.contains("i64 5"),
        "Unit plus LF must use the exact five-byte length: {write}"
    );

    let failed = run_with_read_only_stdout(&executable, directory.path());
    assert!(
        !failed.status.success(),
        "legacy harness ignored an exact stdout write failure: {failed:?}"
    );

    #[cfg(unix)]
    {
        let closed = run_with_closed_stdout(&executable);
        assert_eq!(
            closed.status.code(),
            Some(loom_runtime_abi::STDOUT_WRITE_FAILED),
            "legacy harness allowed SIGPIPE to bypass the stdout ABI: {closed:?}"
        );
    }
}

#[test]
fn legacy_passing_tests_return_failure_when_exact_stdout_is_not_writable() {
    let program = unit_test_program();
    let directory = tempfile::tempdir().expect("create temp directory");
    let executable = directory.path().join("tests");
    let ir = directory.path().join("tests.ll");
    let mut options = EmitOptions::tests();
    options.emit_ir = Some(ir.clone());
    emit_native(&program, &executable, &options).expect("emit legacy native tests executable");

    let llvm = std::fs::read_to_string(ir).expect("read legacy tests stdout LLVM IR");
    assert_exact_stdout_ir(&llvm);
    let normal = Command::new(&executable)
        .output()
        .expect("run legacy tests executable");
    assert!(normal.status.success(), "{normal:?}");
    assert_eq!(normal.stdout, b"passed sample.main\n");

    let failed = run_with_read_only_stdout(&executable, directory.path());
    assert!(
        !failed.status.success(),
        "legacy tests harness ignored a passing-line stdout failure: {failed:?}"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn unit_output_runs_after_its_runtime_is_destroyed() {
    let source = r"module root_result_lifetime

pub fn main() {
    var values = List[Int]()
    values.add(1)
}

test fn list_result_lifetime() {
    var values = List[Int]()
    values.add(2)
}
";
    let project = tempfile::tempdir().expect("create root lifetime project");
    std::fs::write(project.path().join("main.loom"), source).expect("write root lifetime source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load root lifetime project")
        .snapshot()
        .expect("analyze root lifetime project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());

    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(
        snapshot.executable().expect("lower root lifetime MIR"),
        &executable,
        &options,
    )
    .expect("emit root lifetime executable");

    let llvm = std::fs::read_to_string(ir).expect("read root lifetime LLVM IR");
    let success = llvm.find("run.success:").expect("run success block");
    let success = &llvm[success..];
    let destroy = success
        .find("call i32 @loom_runtime_destroy_v1")
        .expect("allocating root must release its runtime");
    let output = success
        .find("call i32 @loom_runtime_stdout_write_v1")
        .expect("Unit root must write its fixed result");
    assert!(
        destroy < output,
        "Unit output must not keep the root runtime active:\n{success}"
    );
    assert_exact_stdout_ir(&llvm);
    assert!(llvm.contains("call ptr @loom_runtime_create_v1"), "{llvm}");
    assert!(
        llvm.contains("call i32 @loom_runtime_activate_v1"),
        "{llvm}"
    );
    assert!(
        llvm.contains("call i32 @loom_runtime_deactivate_v1"),
        "{llvm}"
    );
    assert!(llvm.contains("@loom_context_raise_fault_v1"), "{llvm}");
    assert!(!llvm.contains("@loom_executor_"), "{llvm}");
    assert!(!llvm.contains("@loom_executor_create("), "{llvm}");
    assert!(
        !llvm.contains("@loom_executor_create_for_runtime_v1"),
        "{llvm}"
    );
    assert!(!llvm.contains("@loom_gc_activate_executor"), "{llvm}");
    assert!(!llvm.contains("@loom_gc_deactivate_executor"), "{llvm}");
    assert!(!llvm.contains("@loom_executor_raise_fault"), "{llvm}");
    assert!(llvm.contains("runtime.root.failed"), "{llvm}");
    assert!(llvm.contains("runtime.root.activation.failed"), "{llvm}");
    assert!(llvm.contains("runtime.root.activation.destroy"), "{llvm}");

    let output = Command::new(executable)
        .output()
        .expect("run root lifetime executable");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"Unit\n");

    let tests = project.path().join("tests");
    let tests_ir = project.path().join("tests.ll");
    let mut options = EmitOptions::tests();
    options.emit_ir = Some(tests_ir.clone());
    emit_native(
        snapshot.executable().expect("lower root lifetime test MIR"),
        &tests,
        &options,
    )
    .expect("emit root lifetime tests");
    let llvm = std::fs::read_to_string(tests_ir).expect("read test root lifetime LLVM IR");
    let inspect = llvm
        .find("test.tag")
        .expect("test harness must inspect its root result");
    let destroy = llvm[inspect..]
        .find("call i32 @loom_runtime_destroy_v1")
        .map(|offset| inspect + offset)
        .expect("allocating test root must release its runtime");
    assert!(
        inspect < destroy,
        "test runtime was destroyed before result inspection:\n{llvm}"
    );
    assert!(!llvm.contains("@loom_executor_create("), "{llvm}");
    assert!(
        !llvm.contains("@loom_executor_create_for_runtime_v1"),
        "{llvm}"
    );
    let output = Command::new(tests)
        .output()
        .expect("run root lifetime tests");
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn release_and_cross_target_object_policies_are_real_target_inputs() {
    let development =
        target_identity(None, OptimizationProfile::Development).expect("development target");
    let release = target_identity(None, OptimizationProfile::Release).expect("release target");
    assert_eq!(development.triple, release.triple);
    assert_eq!(development.data_layout, release.data_layout);
    assert_eq!(development.cpu_policy, release.cpu_policy);
    assert_eq!(development.cpu_features, release.cpu_features);
    assert_eq!(
        development.cpu_policy,
        TargetMachine::get_host_cpu_name().to_string()
    );
    assert_eq!(
        development.cpu_features,
        TargetMachine::get_host_cpu_features().to_string()
    );
    assert_ne!(development.optimization, release.optimization);

    let portable = target_identity(Some(&development.triple), OptimizationProfile::Development)
        .expect("explicit host triple uses the portable CPU policy");
    assert_eq!(portable.triple, development.triple);
    assert_eq!(portable.data_layout, development.data_layout);
    assert_eq!(portable.cpu_policy, "generic");
    assert!(portable.cpu_features.is_empty());

    let program = unit_program();
    let directory = tempfile::tempdir().expect("create cross target directory");
    let object = directory.path().join("program-aarch64.o");
    let options = EmitOptions::run("main")
        .with_target_triple(Some(CROSS_TRIPLE.to_owned()))
        .with_optimization(OptimizationProfile::Release);
    emit_native_object(&program, &object, &options).expect("emit AArch64 ELF object");
    assert!(
        std::fs::read(&object)
            .expect("read cross object")
            .starts_with(b"\x7fELF")
    );
}

#[test]
fn universal_value_abi_rejects_32_bit_targets() {
    let error = target_identity(
        Some("i686-unknown-linux-gnu"),
        OptimizationProfile::Development,
    )
    .expect_err("the universal Value ABI is 64-bit only");
    assert_eq!(error.code(), "UnsupportedNativePointerWidth");
    assert!(error.to_string().contains("requires 64-bit pointers"));
}

#[test]
fn text_literals_are_immortal_versioned_objects_across_a_gc_safepoint() {
    let source = r#"module text_literal_object

pub async fn main() {
    let empty = ""
    let unicode = "a界🙂"
    let with_nul = "a\0b"
    let empty_length = empty.length()
    let unicode_length = unicode.length()
    let nul_length = with_nul.length()
    assert empty_length == 0
    assert unicode_length == 3
    assert nul_length == 3
    Task.sleep(1).await
    assert empty == ""
    assert unicode == "a界🙂"
    assert with_nul == "a\0b"
}
"#;
    let project = tempfile::tempdir().expect("create Text literal project");
    std::fs::write(project.path().join("main.loom"), source).expect("write Text literal source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load Text literal project")
        .snapshot()
        .expect("analyze Text literal project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());

    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(
        snapshot.executable().expect("lower Text literal MIR"),
        &executable,
        &options,
    )
    .expect("emit Text literal executable");

    let llvm = std::fs::read_to_string(ir).expect("read Text literal LLVM IR");
    let literals = llvm
        .lines()
        .filter(|line| line.starts_with("@text.object."))
        .collect::<Vec<_>>();
    assert!(
        llvm.contains("@loom_layout_text_v1 = external global %loom.LayoutDescriptor"),
        "{literals:#?}",
    );
    assert!(
        literals
            .iter()
            .all(|line| line.contains("private unnamed_addr constant")),
        "{literals:#?}",
    );
    assert!(
        literals
            .iter()
            .any(|line| line.contains("i64 32, i64 0, i64 0")),
        "missing empty TextObject: {literals:#?}",
    );
    assert!(
        literals
            .iter()
            .any(|line| line.contains("i64 40, i64 8, i64 3")),
        "missing Unicode TextObject: {literals:#?}",
    );
    assert!(
        literals
            .iter()
            .any(|line| line.contains("i64 35, i64 3, i64 3")),
        "missing embedded-NUL TextObject: {literals:#?}",
    );
    for (store, _) in llvm.match_indices("store ptr @text.object.") {
        let start = llvm[..store]
            .rfind("store %loom.Value zeroinitializer")
            .expect("literal Value starts from a zero envelope");
        let envelope = &llvm[start..store];
        assert!(envelope.contains("store i64 4"), "{envelope}");
        assert_eq!(
            envelope.matches("store i64 ").count(),
            1,
            "Text literal envelope must write only its tag before data: {envelope}",
        );
    }
    let output = Command::new(executable)
        .output()
        .expect("run Text literal executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn stable_output_runtime_abis_publish_roots_and_run() {
    let source = r#"module stable_output_abi

import std.float.format_float
import std.process.environment

pub fn main() {
    let formatted = format_float(2.5)
    assert formatted == "2.5"

    match "roots".get(1) {
        Some(value) => {
            assert value == "o"
            Unit
        }
        None => {
            assert false
            Unit
        }
    }

    let values = ["zero", "one"]
    match values.get(1) {
        Some(value) => {
            assert value == "one"
            Unit
        }
        None => {
            assert false
            Unit
        }
    }

    let map = TextMap[Text]().insert("key", "value")
    match map.get("key") {
        Some(value) => {
            assert value == "value"
            Unit
        }
        None => {
            assert false
            Unit
        }
    }

    match environment("LOOM_STABLE_OUTPUT_TEST") {
        Some(value) => {
            assert value == "present"
            Unit
        }
        None => {
            assert false
            Unit
        }
    }
}
"#;
    let project = tempfile::tempdir().expect("create stable-output ABI project");
    std::fs::write(project.path().join("main.loom"), source)
        .expect("write stable-output ABI source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load stable-output ABI project")
        .snapshot()
        .expect("analyze stable-output ABI project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(
        snapshot.executable().expect("lower stable-output ABI MIR"),
        &executable,
        &options,
    )
    .expect("emit stable-output ABI executable");

    let llvm = std::fs::read_to_string(ir).expect("read stable-output ABI IR");
    let main = llvm_native_function(&llvm, "stable_output_abi_main");
    for symbol in [
        "@loom_runtime_format_float",
        "@loom_runtime_text_get",
        "@loom_runtime_list_get",
        "@loom_runtime_process_environment",
    ] {
        assert!(main.contains(symbol), "missing {symbol}: {main}");
        assert_gc_state_published_before(main, symbol);
    }
    assert!(
        main.lines().any(|line| {
            line.contains("call i32 @loom_runtime_list_get") && line.matches("ptr ").count() == 2
        }),
        "List.get did not use list/index/stable-output status ABI: {main}"
    );
    assert!(
        main.lines().any(|line| {
            line.contains("call i32 @loom_runtime_text_map_get")
                && line.matches("ptr ").count() == 3
        }),
        "TextMap.get did not use map/key/length/stable-output status ABI: {main}"
    );
    let map_lookup = main
        .find("call i32 @loom_runtime_text_map_get")
        .expect("TextMap.get lookup call");
    assert!(
        main[map_lookup..].contains("call i32 @loom_gc_clone_value_v1"),
        "TextMap.get must deep-clone the selected value after its non-collecting lookup: {main}"
    );
    assert_gc_state_published_before(main, "@loom_gc_clone_value_v1");
    assert!(
        main.lines().any(|line| {
            line.contains("call i32 @loom_runtime_process_environment")
                && line.matches("ptr ").count() == 2
        }),
        "environment did not use name/length/stable-output status ABI: {main}"
    );

    let output = Command::new(executable)
        .env("LOOM_STABLE_OUTPUT_TEST", "present")
        .output()
        .expect("run stable-output ABI executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn range_plan_removes_proved_checks_before_abi_selection_and_release_folds_code() {
    let source = r"module optimize

fn folded() Int {
    40 + 2
}

fn unreachable() Int {
    100 + 23
}

pub fn main() {
    let value = folded()
    assert value == 42
}
";
    let project = tempfile::tempdir().expect("create optimization project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load project")
        .snapshot()
        .expect("analyze project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower executable MIR");

    let development_ir = project.path().join("development.ll");
    let development_object = project.path().join("development.o");
    let mut development = EmitOptions::run("main");
    development.emit_ir = Some(development_ir.clone());
    emit_native_object(program, &development_object, &development).expect("emit development IR");

    let release_ir = project.path().join("release.ll");
    let release_object = project.path().join("release.o");
    let mut release = EmitOptions::run("main").with_optimization(OptimizationProfile::Release);
    release.emit_ir = Some(release_ir.clone());
    emit_native_object(program, &release_object, &release).expect("emit release IR");

    let development = std::fs::read_to_string(development_ir).expect("read development IR");
    let release = std::fs::read_to_string(release_ir).expect("read release IR");
    let folded = llvm_native_function(&development, "optimize_folded");
    assert!(
        folded.lines().next().is_some_and(|definition| {
            definition.starts_with("define internal i64 @loom.native.fn.")
                && definition.contains(".optimize_folded()")
        }),
        "{folded}"
    );
    assert!(!development.contains("optimize_unreachable"));
    assert!(folded.contains("add nsw i64"), "{folded}");
    assert!(!folded.contains("with.overflow"), "{folded}");
    assert!(!folded.lines().next().unwrap_or_default().contains("ptr"));
    assert!(!release.contains("optimize_folded"));
    assert!(!release.contains("optimize_unreachable"));
    assert!(!release.contains("llvm.sadd.with.overflow.i64"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn scalar_int_abi_is_recursive_checked_and_bridges_to_universal_value_at_root() {
    let source = r"module scalar_int

fn fibonacci(value Int) Int {
    if value < 2 {
        value
    } else {
        fibonacci(value - 1) + fibonacci(value - 2)
    }
}

fn contracted(value Int) Int
    requires value >= 0
    ensures result > old(value)
{
    value + 1
}

pub fn main() {
    let recursive = fibonacci(20)
    assert recursive == 6765
    let negative = checkedFibonacci(-1)
    assert negative == -1
    let answer = contracted(41)
    assert answer == 42
}

fn checkedFibonacci(value Int) Int {
    defer {
        Unit
    }
    fibonacci(value)
}
";
    let project = tempfile::tempdir().expect("create scalar Int project");
    std::fs::write(project.path().join("main.loom"), source).expect("write scalar Int source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load scalar Int project")
        .snapshot()
        .expect("analyze scalar Int project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower scalar Int MIR");

    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(program, &executable, &options).expect("emit scalar Int executable");

    let llvm = std::fs::read_to_string(ir).expect("read scalar Int LLVM IR");
    let fibonacci = llvm_native_function(&llvm, "scalar_int_fibonacci");
    let assumed_fibonacci = llvm_assumed_native_function(&llvm, "scalar_int_fibonacci");
    let fibonacci_symbol = llvm_defined_symbol(fibonacci);
    let assumed_fibonacci_symbol = llvm_defined_symbol(assumed_fibonacci);
    let checked_fibonacci_call = format!("call {{ i32, i64 }} @{fibonacci_symbol}");
    let assumed_fibonacci_call = format!("call i64 @{assumed_fibonacci_symbol}");
    assert!(
        fibonacci
            .lines()
            .next()
            .is_some_and(|line| line.contains("i64 %0, ptr %1")),
        "{fibonacci}"
    );
    assert_eq!(
        fibonacci.matches(&checked_fibonacci_call).count(),
        2,
        "{fibonacci}"
    );
    assert_eq!(
        fibonacci.matches(&assumed_fibonacci_call).count(),
        1,
        "{fibonacci}"
    );
    assert!(fibonacci.contains("icmp ule i64 %0, 92"), "{fibonacci}");
    assert!(!fibonacci.contains("%loom.ArgNode"), "{fibonacci}");
    assert!(
        !fibonacci.contains("llvm.ssub.with.overflow.i64"),
        "{fibonacci}"
    );
    assert_eq!(fibonacci.matches("sub nsw i64").count(), 2, "{fibonacci}");
    assert!(
        fibonacci.contains("llvm.sadd.with.overflow.i64"),
        "{fibonacci}"
    );
    assert!(
        assumed_fibonacci
            .lines()
            .next()
            .is_some_and(|line| line.contains("i64 %0") && !line.contains("ptr")),
        "{assumed_fibonacci}"
    );
    assert_eq!(
        assumed_fibonacci.matches(&assumed_fibonacci_call).count(),
        2,
        "{assumed_fibonacci}"
    );
    assert_eq!(
        assumed_fibonacci.matches("sub nsw i64").count(),
        2,
        "{assumed_fibonacci}"
    );
    assert_eq!(
        assumed_fibonacci.matches("add nsw i64").count(),
        1,
        "{assumed_fibonacci}"
    );
    assert!(
        !assumed_fibonacci.contains("with.overflow"),
        "{assumed_fibonacci}"
    );
    assert!(
        assumed_fibonacci.contains("call void @llvm.assume(i1 %assumed.domain)"),
        "{assumed_fibonacci}"
    );
    let terminal_branches = fibonacci
        .lines()
        .filter(|line| {
            line.trim_start().starts_with("br i1 ")
                && (line.contains("label %operation.fail") || line.contains("label %call.failure"))
        })
        .collect::<Vec<_>>();
    assert!(terminal_branches.len() >= 3, "{fibonacci}");
    assert!(
        terminal_branches.iter().all(|line| line.contains("!prof")),
        "{terminal_branches:#?}"
    );
    let ordinary_if = fibonacci
        .lines()
        .find(|line| line.contains("label %if.then") && line.contains("label %if.else"))
        .unwrap_or_else(|| panic!("missing ordinary if branch: {fibonacci}"));
    assert!(!ordinary_if.contains("!prof"), "{ordinary_if}");
    assert!(
        llvm.contains(r#"!{!"branch_weights", i32 2000, i32 1}"#),
        "{llvm}"
    );
    assert!(
        llvm.contains(r#"!{!"branch_weights", i32 1, i32 2000}"#),
        "{llvm}"
    );
    let wrapper = llvm_function(&llvm, "scalar_int_main");
    let native_main = llvm_native_function(&llvm, "scalar_int_main");
    let native_main_symbol = llvm_defined_symbol(native_main);
    assert!(
        native_main.contains(&assumed_fibonacci_call),
        "{native_main}"
    );
    assert!(
        wrapper.contains(&format!("@{native_main_symbol}")),
        "{wrapper}"
    );
    let wrapper_status = wrapper
        .lines()
        .find(|line| line.contains("br i1 %integer.call.ok"))
        .unwrap_or_else(|| panic!("missing scalar wrapper status branch: {wrapper}"));
    assert!(wrapper_status.contains("!prof"), "{wrapper_status}");
    let fault_attributes = llvm_declaration_attributes(&llvm, "loom_context_raise_fault_v1");
    assert!(fault_attributes.contains("cold"), "{fault_attributes}");
    assert!(fault_attributes.contains("noinline"), "{fault_attributes}");
    let contracted = llvm_native_function(&llvm, "scalar_int_contracted");
    assert!(
        contracted.contains("call i32 @loom_gc_clone_value_v1"),
        "{contracted}"
    );
    assert!(
        llvm_defined_symbol(wrapper).ends_with(".scalar_int_main"),
        "{wrapper}"
    );
    assert!(llvm.contains("call ptr @loom_runtime_create_v1"), "{llvm}");
    assert!(
        llvm.contains("call i32 @loom_runtime_activate_v1"),
        "{llvm}"
    );
    assert!(llvm.contains("@loom_context_raise_fault_v1"), "{llvm}");
    assert!(!llvm.contains("@loom_executor_"), "{llvm}");

    let output = Command::new(executable)
        .output()
        .expect("run scalar Int executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, b"Unit\n");

    let overflow_source = r"module scalar_int_fault

fn checkedAdd(left Int, right Int) Int {
    left + right
}

pub fn main() {
    discard checkedAdd(9223372036854775807, 1)
}
";
    let overflow_project = tempfile::tempdir().expect("create scalar Int fault project");
    std::fs::write(overflow_project.path().join("main.loom"), overflow_source)
        .expect("write scalar Int fault source");
    let snapshot = AnalysisHost::new(overflow_project.path())
        .expect("load scalar Int fault project")
        .snapshot()
        .expect("analyze scalar Int fault project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let overflow_executable = overflow_project.path().join("program");
    emit_native(
        snapshot.executable().expect("lower scalar Int fault MIR"),
        &overflow_executable,
        &EmitOptions::run("main"),
    )
    .expect("emit scalar Int fault executable");
    let output = Command::new(overflow_executable)
        .output()
        .expect("run scalar Int fault executable");
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("IntegerOverflow")
            || String::from_utf8_lossy(&output.stderr).contains("IntegerOverflow"),
        "{output:?}"
    );

    let recursive_overflow_source = r"module scalar_recursive_fault

fn amplified(value Int) Int {
    if value < 1 {
        1
    } else {
        amplified(value - 1) * 2000000
    }
}

pub fn main() {
    discard amplified(4)
}
";
    let recursive_overflow_project =
        tempfile::tempdir().expect("create recursive scalar Int fault project");
    std::fs::write(
        recursive_overflow_project.path().join("main.loom"),
        recursive_overflow_source,
    )
    .expect("write recursive scalar Int fault source");
    let snapshot = AnalysisHost::new(recursive_overflow_project.path())
        .expect("load recursive scalar Int fault project")
        .snapshot()
        .expect("analyze recursive scalar Int fault project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let recursive_overflow_executable = recursive_overflow_project.path().join("program");
    let recursive_overflow_ir = recursive_overflow_project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(recursive_overflow_ir.clone());
    emit_native(
        snapshot
            .executable()
            .expect("lower recursive scalar Int fault MIR"),
        &recursive_overflow_executable,
        &options,
    )
    .expect("emit recursive scalar Int fault executable");
    let recursive_llvm =
        std::fs::read_to_string(recursive_overflow_ir).expect("read recursive overflow LLVM IR");
    let amplified = llvm_native_function(&recursive_llvm, "scalar_recursive_fault_amplified");
    let amplified_symbol = llvm_defined_symbol(amplified);
    let checked_amplified_call = format!("call {{ i32, i64 }} @{amplified_symbol}");
    let assumed_amplified =
        llvm_assumed_native_function(&recursive_llvm, "scalar_recursive_fault_amplified");
    let assumed_amplified_call = format!("call i64 @{}", llvm_defined_symbol(assumed_amplified));
    assert!(amplified.contains("icmp ule i64 %0, 3"), "{amplified}");
    assert_eq!(
        amplified.matches(&checked_amplified_call).count(),
        1,
        "{amplified}"
    );
    assert_eq!(
        amplified.matches(&assumed_amplified_call).count(),
        1,
        "{amplified}"
    );
    let recursive_main = llvm_native_function(&recursive_llvm, "scalar_recursive_fault_main");
    assert!(
        recursive_main.contains(&checked_amplified_call),
        "{recursive_main}"
    );
    let output = Command::new(recursive_overflow_executable)
        .output()
        .expect("run recursive scalar Int fault executable");
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("IntegerOverflow")
            || String::from_utf8_lossy(&output.stderr).contains("IntegerOverflow"),
        "{output:?}"
    );
}

#[test]
fn pure_scalar_int_abi_omits_status_executor_and_root_runtime() {
    let source = r"module pure_scalar_int

fn identity(value Int) Int {
    value
}

fn choose(value Int) Int {
    if value < 0 {
        identity(0)
    } else {
        identity(value)
    }
}

pub fn main() {
    discard choose(42)
}
";
    let project = tempfile::tempdir().expect("create pure scalar Int project");
    std::fs::write(project.path().join("main.loom"), source).expect("write pure scalar source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load pure scalar project")
        .snapshot()
        .expect("analyze pure scalar project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower pure scalar MIR");

    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(program, &executable, &options).expect("emit pure scalar executable");

    let llvm = std::fs::read_to_string(ir).expect("read pure scalar LLVM IR");
    let identity = llvm_native_function(&llvm, "pure_scalar_int_identity");
    let choose = llvm_native_function(&llvm, "pure_scalar_int_choose");
    let main = llvm_native_function(&llvm, "pure_scalar_int_main");
    let identity_symbol = llvm_defined_symbol(identity);
    let choose_symbol = llvm_defined_symbol(choose);
    assert!(!llvm.contains("@loom.int.fn."), "{llvm}");
    assert!(
        identity
            .lines()
            .next()
            .is_some_and(|line| line.contains("define internal i64") && line.contains("i64 %0")),
        "{identity}"
    );
    assert!(
        choose.contains(&format!("call i64 @{identity_symbol}")),
        "{choose}"
    );
    assert!(
        main.contains(&format!("call i64 @{choose_symbol}")),
        "{main}"
    );
    assert!(!identity.lines().next().unwrap_or_default().contains("ptr"));
    assert!(!choose.lines().next().unwrap_or_default().contains("ptr"));
    assert!(!main.lines().next().unwrap_or_default().contains("ptr"));
    assert!(!llvm.contains("@loom_runtime_create_v1"), "{llvm}");
    assert!(!llvm.contains("@loom_runtime_activate_v1"), "{llvm}");
    assert!(!llvm.contains("@loom_runtime_destroy_v1"), "{llvm}");
    assert!(!llvm.contains("@loom_executor_create("), "{llvm}");
    assert!(
        !llvm.contains("@loom_executor_create_for_runtime_v1"),
        "{llvm}"
    );
    assert!(!llvm.contains("@loom_gc_activate_executor"), "{llvm}");
    assert!(!llvm.contains("@loom_gc_deactivate_executor"), "{llvm}");
    assert!(!llvm.contains("@loom_executor_raise_fault"), "{llvm}");

    let output = Command::new(executable)
        .output()
        .expect("run pure scalar executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
fn typed_text_copy_and_length_run_without_an_active_runtime() {
    let source = r#"module pure_text_leaf

fn copiedLiteralLength() Int {
    let original = "loom"
    let copied = original
    copied.length()
}

pub fn main() {
    discard copiedLiteralLength()
}
"#;
    let (project, _program, llvm) = emit_source_with_ir(source);
    let copied = llvm_native_function(&llvm, "pure_text_leaf_copiedLiteralLength");
    let main = llvm_native_function(&llvm, "pure_text_leaf_main");

    assert!(!copied.lines().next().unwrap_or_default().contains("ptr"));
    assert!(!main.lines().next().unwrap_or_default().contains("ptr"));
    assert!(!copied.contains("@loom_gc_clone_value_v1"), "{copied}");
    assert!(!copied.contains("@loom_gc_root_push_v1"), "{copied}");
    assert!(!copied.contains("@loom_gc_root_pop_v1"), "{copied}");
    assert!(!copied.contains("@loom_gc_safepoint_v1"), "{copied}");
    assert!(!llvm.contains("@loom_runtime_create_v1"), "{llvm}");
    assert!(!llvm.contains("@loom_runtime_activate_v1"), "{llvm}");
    assert!(!llvm.contains("@loom_runtime_destroy_v1"), "{llvm}");

    let output = Command::new(project.path().join("program"))
        .output()
        .expect("run context-free Text leaf executable");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
fn universal_calls_collect_only_when_argument_evaluation_deep_copies() {
    let source = r"module universal_call_requirements

record Pair { value Int }

fn read(value Pair) Int { value.value }

fn moveCall(value Pair) Int { read(value) }

fn copyCall(value Pair) Int { read(value) }

pub fn main() {
    let moved = moveCall(Pair { value = 41 })
    let copied = copyCall(Pair { value = 42 })
    discard moved + copied
}
";
    let project = tempfile::tempdir().expect("create universal call project");
    std::fs::write(project.path().join("main.loom"), source).expect("write universal call source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load universal call project")
        .snapshot()
        .expect("analyze universal call project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let mut program = snapshot
        .executable()
        .expect("lower universal call MIR")
        .clone()
        .into_program();

    let move_call = program
        .functions
        .iter_mut()
        .find(|function| function.name.ends_with("moveCall"))
        .expect("find moveCall MIR");
    let call = move_call.body.tail.as_mut().expect("moveCall tail");
    let ExprKind::Call { arguments, .. } = &mut call.kind else {
        panic!("moveCall tail is not a call: {call:?}");
    };
    let [CallArgument::Value(argument)] = arguments.as_mut_slice() else {
        panic!("moveCall has unexpected call arguments: {arguments:?}");
    };
    let ExprKind::Copy(place) = &argument.kind else {
        panic!("moveCall argument is not a copy: {argument:?}");
    };
    argument.kind = ExprKind::Move(place.clone());
    let program = program
        .into_checked()
        .expect("mutated universal call MIR remains valid");

    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(&program, &executable, &options).expect("emit universal call executable");
    let llvm = std::fs::read_to_string(ir).expect("read universal call LLVM IR");

    let moved = llvm_function(&llvm, "universal_call_requirements_moveCall");
    assert!(moved.contains("call i32 @loom.fn."), "{moved}");
    assert!(!moved.contains("@loom_gc_clone_value_v1"), "{moved}");
    assert!(!moved.contains("@loom_gc_root_push_v1"), "{moved}");
    assert!(!moved.contains("@loom_gc_safepoint_v1"), "{moved}");

    let copied = llvm_function(&llvm, "universal_call_requirements_copyCall");
    assert_balanced_gc_root_frame(copied);
    assert!(copied.contains("@loom_gc_clone_value_v1"), "{copied}");
    assert_gc_state_published_before(copied, "@loom_gc_clone_value_v1");

    let output = Command::new(executable)
        .output()
        .expect("run universal call executable");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
fn allocating_functions_emit_balanced_shadow_roots_while_pure_scalars_do_not() {
    let source = r#"module shadow_root_shape

fn identity(value Int) Int { value }

fn scalarEqual(left Int, right Int) Bool { left == right }

fn allocate(count Int) List[Text] {
    var values = List[Text]()
    for index in 0..count {
        values.add("rooted")
        Unit
    }
    values
}

pub fn main() {
    let values = allocate(4)
    let count = values.length()
    let expected = identity(4)
    assert count == expected
    let equal = scalarEqual(count, expected)
    assert equal
}
"#;
    let (project, _program, llvm) = emit_source_with_ir(source);

    let identity = llvm_native_function(&llvm, "shadow_root_shape_identity");
    assert!(!identity.contains("@loom_gc_root_push_v1"), "{identity}");
    assert!(!identity.contains("@loom_gc_root_pop_v1"), "{identity}");
    assert!(!identity.contains("@loom_gc_safepoint_v1"), "{identity}");
    assert!(!identity.contains("gc.root.frame"), "{identity}");

    let scalar_equal = llvm_native_function(&llvm, "shadow_root_shape_scalarEqual");
    assert!(
        !scalar_equal.contains("@loom_gc_root_push_v1"),
        "{scalar_equal}"
    );
    assert!(
        !scalar_equal.contains("@loom_gc_root_pop_v1"),
        "{scalar_equal}"
    );
    assert!(!scalar_equal.contains("gc.root.frame"), "{scalar_equal}");
    assert!(
        !scalar_equal.contains("@loom_gc_safepoint_v1"),
        "scalar equality cannot collect and must not poll: {scalar_equal}"
    );

    let allocate = llvm_function(&llvm, "shadow_root_shape_allocate");
    assert_balanced_gc_root_frame(allocate);
    assert!(!allocate.contains("@loom_gc_safepoint_v1"), "{allocate}");
    assert!(
        llvm.contains("private unnamed_addr constant %loom.GcRootDescriptor"),
        "{llvm}"
    );
    assert!(llvm.contains("@llvm.trap"), "{llvm}");
    let descriptor = gc_root_descriptor(&llvm, allocate);
    assert!(descriptor.state_count > 1, "{descriptor:?}\n{allocate}");
    assert!(
        descriptor
            .bitmaps
            .chunks(descriptor.bitmap_words)
            .collect::<BTreeSet<_>>()
            .len()
            > 1,
        "root states were not specialized: {descriptor:?}\n{allocate}"
    );
    assert_gc_state_published_before(allocate, "@loom_runtime_list_add");

    assert_emitted_main_succeeds(&project);
}

#[test]
fn text_leaf_copies_and_read_only_builtins_do_not_create_gc_boundaries() {
    let source = r#"module text_leaf_requirements

fn inspect(value Text) Int {
    let copied = value
    let count = copied.length()
    if copied.contains("loom") { count } else { 0 }
}

fn concatenate(value Text) Text {
    value.concat("!")
}

pub fn main() {
    let count = inspect("loom")
    assert count == 4
    let joined = concatenate("loom")
    assert joined == "loom!"
}
"#;
    let (project, _program, llvm) = emit_source_with_ir(source);

    let inspect = llvm_function(&llvm, "text_leaf_requirements_inspect");
    assert!(inspect.contains("@loom_runtime_text_contains"), "{inspect}");
    assert!(!inspect.contains("@loom_gc_clone_value_v1"), "{inspect}");
    assert!(
        !inspect.contains("@loom_gc_build_value_nodes_v1"),
        "{inspect}"
    );
    assert!(!inspect.contains("@loom_gc_root_push_v1"), "{inspect}");
    assert!(!inspect.contains("@loom_gc_root_pop_v1"), "{inspect}");
    assert!(!inspect.contains("@loom_gc_safepoint_v1"), "{inspect}");
    assert!(!inspect.contains("gc.root.frame"), "{inspect}");

    let concatenate = llvm_function(&llvm, "text_leaf_requirements_concatenate");
    assert_balanced_gc_root_frame(concatenate);
    assert!(
        concatenate.contains("@loom_runtime_text_concat"),
        "{concatenate}"
    );
    assert_gc_state_published_before(concatenate, "@loom_runtime_text_concat");

    assert_emitted_main_succeeds(&project);
}

#[test]
fn read_only_aggregate_operations_borrow_copies_without_collecting() {
    let source = r#"module readonly_builtin_requirements

fn byteCount(value Bytes) Int { value.length() }

fn mapFacts(value TextMap[Text]) Int {
    let count = value.length()
    if value.contains("key") { count } else { 0 }
}

fn emptyMapCount() Int {
    let value = TextMap[Text]()
    value.length()
}

fn pathText(value Path) Text { value.as_text() }

pub fn main() {
    let bytes = "abc".encode_utf8()
    let byteCountValue = byteCount(bytes)
    assert byteCountValue == 3
    let emptyCount = emptyMapCount()
    assert emptyCount == 0
    let fields = TextMap[Text]().insert("key", "value")
    let facts = mapFacts(fields)
    assert facts == 1
    match Path.from_text("relative") {
        Ok(path) => {
            let text = pathText(path)
            assert text == "relative"
            Unit
        }
        Err(_) => {
            assert false
            Unit
        }
    }
}
"#;
    let (project, _program, llvm) = emit_source_with_ir(source);

    for name in ["byteCount", "mapFacts", "pathText"] {
        let function = llvm_function(&llvm, &format!("readonly_builtin_requirements_{name}"));
        assert!(!function.contains("@loom_gc_clone_value_v1"), "{function}");
        assert!(
            !function.contains("@loom_gc_build_value_nodes_v1"),
            "{function}"
        );
        assert!(!function.contains("@loom_gc_root_push_v1"), "{function}");
        assert!(!function.contains("@loom_gc_root_pop_v1"), "{function}");
        assert!(!function.contains("@loom_gc_safepoint_v1"), "{function}");
        assert!(!function.contains("gc.root.frame"), "{function}");
    }
    let empty_map = llvm_native_function(&llvm, "readonly_builtin_requirements_emptyMapCount");
    assert!(!empty_map.lines().next().unwrap_or_default().contains("ptr"));
    assert!(
        !empty_map.contains("@loom_gc_clone_value_v1"),
        "{empty_map}"
    );
    assert!(!empty_map.contains("@loom_gc_root_push_v1"), "{empty_map}");
    assert!(!empty_map.contains("@loom_gc_safepoint_v1"), "{empty_map}");
    let map_facts = llvm_function(&llvm, "readonly_builtin_requirements_mapFacts");
    assert!(
        map_facts.contains("@loom_runtime_text_map_get"),
        "{map_facts}"
    );
    assert_emitted_main_succeeds(&project);
}

#[test]
fn scalar_aggregate_temporaries_do_not_expand_gc_root_frames() {
    let small_elements = std::iter::repeat_n("0", 4).collect::<Vec<_>>().join(", ");
    let large_elements = std::iter::repeat_n("0", 256).collect::<Vec<_>>().join(", ");
    let source = format!(
        r"module scalar_aggregate_roots

fn small() Int {{
    let values = [{small_elements}]
    values.length()
}}

fn large() Int {{
    let values = [{large_elements}]
    values.length()
}}

pub fn main() {{
    let smallCount = small()
    let largeCount = large()
    assert smallCount == 4
    assert largeCount == 256
}}
"
    );
    let (project, _program, llvm) = emit_source_with_ir(&source);

    let small = llvm_native_function(&llvm, "scalar_aggregate_roots_small");
    let large = llvm_native_function(&llvm, "scalar_aggregate_roots_large");
    assert_balanced_gc_root_frame(small);
    assert_balanced_gc_root_frame(large);
    let small_slots = gc_root_slot_count(small);
    let large_slots = gc_root_slot_count(large);
    assert_eq!(small_slots, 3, "unexpected small root frame: {small}");
    assert_eq!(large_slots, 3, "scalar elements grew root slots: {large}");

    assert_emitted_main_succeeds(&project);
}

#[test]
fn gc_root_bitmaps_cover_more_than_one_word_without_live_tail_bits() {
    let parameters = (0..66)
        .map(|index| format!("value{index} Text"))
        .collect::<Vec<_>>()
        .join(", ");
    let arguments = std::iter::repeat_n("\"root\"", 66)
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        r#"module gc_root_bitmap_tail

fn retain({parameters}, count Int) Text {{
    var noise = List[Text]()
    for index in 0..count {{
        noise.add("relocate")
        Unit
    }}
    value65
}}

pub fn main() {{
    let retained = retain({arguments}, 4)
    assert retained == "root"
}}
"#
    );
    let (project, _program, llvm) = emit_source_with_ir(&source);
    let retain = llvm_function(&llvm, "gc_root_bitmap_tail_retain");
    let descriptor = gc_root_descriptor(&llvm, retain);

    assert!(descriptor.slot_count > 64, "{descriptor:?}\n{retain}");
    assert!(descriptor.bitmap_words > 1, "{descriptor:?}\n{retain}");
    let tail_bits = descriptor.slot_count % 64;
    assert_ne!(tail_bits, 0, "test must exercise a partial tail word");
    let tail_mask = (1_u64 << tail_bits) - 1;
    for state in descriptor.bitmaps.chunks(descriptor.bitmap_words) {
        assert_eq!(
            state[descriptor.bitmap_words - 1] & !tail_mask,
            0,
            "unused tail bits are live: {descriptor:?}\n{retain}"
        );
    }
    assert!(!retain.contains("@loom_gc_safepoint_v1"), "{retain}");
    assert_gc_state_published_before(retain, "@loom_runtime_list_add");

    assert_emitted_main_succeeds(&project);
}

#[test]
fn safepoint_states_drop_dead_temporaries_and_keep_live_caller_values() {
    let source = r#"module precise_root_liveness

fn allocate(count Int) List[Text] {
    var values = List[Text]()
    for index in 0..count {
        values.add("relocate")
        Unit
    }
    values
}

fn retain(value Text, count Int) Text {
    let copied = value
    let noise = allocate(count)
    let noiseCount = noise.length()
    assert noiseCount == count
    copied
}

pub fn main() {
    let retained = retain("kept", 4096)
    assert retained == "kept"
}
"#;
    let (project, _program, llvm) = emit_source_with_ir(source);
    let retain = llvm_function(&llvm, "precise_root_liveness_retain");
    assert_balanced_gc_root_frame(retain);

    let descriptor = gc_root_descriptor(&llvm, retain);
    assert!(descriptor.state_count > 2, "{descriptor:?}\n{retain}");
    assert_eq!(
        descriptor.bitmaps.len(),
        descriptor.state_count * descriptor.bitmap_words,
        "{descriptor:?}\n{retain}"
    );
    let source_call_state = gc_state_before_call(retain, "call i32 @loom.fn.", 0);
    let copied = gc_root_slot_index(retain, "local.2.copied");
    assert!(
        descriptor.is_live(source_call_state, copied),
        "Text leaf retained across a collecting call is not rooted: {descriptor:?}\n{retain}"
    );
    assert!(!retain.contains("@loom_gc_clone_value_v1"), "{retain}");
    assert_gc_state_published_before(retain, "call i32 @loom.fn.");
    assert!(!retain.contains("@loom_gc_safepoint_v1"), "{retain}");

    assert_emitted_main_succeeds(&project);
}

#[test]
#[allow(clippy::too_many_lines)]
fn moving_gc_relocates_sync_nested_projected_and_dynamic_call_state() {
    let source = r#"module moving_gc_sync

dyn concept CounterOps {
    method allocateAndAdd(mut self, amount Int, count Int) Int
}

record Counter { value Int }
record Holder { counter Counter }

impl CounterOps for Counter {
    method allocateAndAdd(mut self, amount Int, count Int) Int {
        var noise = List[Text]()
        for index in 0..count {
            noise.add("relocate")
            Unit
        }
        let noiseCount = noise.length()
        assert noiseCount == count
        self.value = self.value + amount
        self.value
    }
}

impl Holder {
    method projected(mut self, amount Int, count Int) Int {
        self.counter.allocateAndAdd(amount, count)
    }

    method nested(mut self, amount Int, count Int) Int {
        self.projected(amount, count)
    }
}

fn dynamicAdd(counter dyn CounterOps, amount Int, count Int) Int {
    counter.allocateAndAdd(amount, count)
}

pub fn main() {
    var projected = Holder { counter = Counter { value = 10 } }
    let projectedResult = projected.nested(5, 4096)
    assert projectedResult == 15
    let projectedValue = projected.counter.value
    assert projectedValue == 15

    var dynamic = Holder { counter = Counter { value = 20 } }
    let dynamicResult = dynamicAdd(dynamic.counter, 7, 4096)
    assert dynamicResult == 27
    let dynamicValue = dynamic.counter.value
    assert dynamicValue == 27
}
"#;
    let (project, _program, llvm) = emit_source_with_ir(source);

    let allocate = llvm_function(&llvm, "moving_gc_sync_allocateAndAdd");
    let projected = llvm_function(&llvm, "moving_gc_sync_projected");
    let nested = llvm_function(&llvm, "moving_gc_sync_nested");
    let dynamic = llvm_function(&llvm, "moving_gc_sync_dynamicAdd");
    assert_balanced_gc_root_frame(allocate);
    assert_balanced_gc_root_frame(projected);
    assert_balanced_gc_root_frame(nested);
    assert_balanced_gc_root_frame(dynamic);
    assert!(!allocate.contains("@loom_gc_safepoint_v1"), "{allocate}");
    assert!(projected.contains("inout.projected.proxy"), "{projected}");
    assert!(dynamic.contains("dyn.dispatch.proxy"), "{dynamic}");
    assert_gc_state_published_before(projected, "call i32 @loom.fn.");
    assert_gc_state_published_before(nested, "call i32 @loom.fn.");
    assert_gc_state_published_before(dynamic, "dyn.call");
    assert_gc_state_published_before(allocate, "@loom_runtime_list_add");
    let dynamic_descriptor = gc_root_descriptor(&llvm, dynamic);
    let projected_descriptor = gc_root_descriptor(&llvm, projected);
    let projected_call_state = gc_state_before_call(projected, "call i32 @loom.fn.", 0);
    let projected_proxy = gc_root_slot_index(projected, "inout.projected.proxy");
    assert!(
        projected_descriptor.is_live(projected_call_state, projected_proxy),
        "projected InOut proxy is not live at its call: {projected_descriptor:?}\n{projected}"
    );
    let dynamic_call_state = gc_state_before_call(dynamic, "dyn.call", 0);
    let dynamic_proxy = gc_root_slot_index(dynamic, "dyn.dispatch.proxy");
    assert!(
        dynamic_descriptor.is_live(dynamic_call_state, dynamic_proxy),
        "dynamic dispatch proxy is not live at its call: {dynamic_descriptor:?}\n{dynamic}"
    );
    assert!(
        dynamic_descriptor.state_count > 1,
        "{dynamic_descriptor:?}\n{dynamic}"
    );
    assert!(!llvm.contains("ptrtoint"), "{llvm}");
    assert!(!llvm.contains("inttoptr"), "{llvm}");

    assert_emitted_main_succeeds(&project);
}

#[test]
fn moving_gc_rederives_tuple_and_stages_all_match_bindings_before_clone() {
    let source = r#"module moving_gc_projection_clone

enum Payload {
    Values(List[Text], Text)
}

fn values(count Int) List[Text] {
    var values = List[Text]()
    for index in 0..count {
        values.add("relocate")
        Unit
    }
    values
}

fn tupleRetainsSecond(count Int) Text {
    let pair = (values(count), "tuple-kept")
    let copied, retained = pair
    let copiedCount = copied.length()
    assert copiedCount == count
    retained
}

fn matchRetainsSecond(count Int) Text {
    let payload = Payload.Values(values(count), "match-kept")
    match payload {
        Payload.Values(copied, retained) => {
            let copiedCount = copied.length()
            assert copiedCount == count
            retained
        }
    }
}

pub fn main() {
    let tupleValue = tupleRetainsSecond(4096)
    assert tupleValue == "tuple-kept"
    let matchValue = matchRetainsSecond(4096)
    assert matchValue == "match-kept"
}
"#;
    let (project, _program, llvm) = emit_source_with_ir(source);

    let tuple = llvm_function(&llvm, "moving_gc_projection_clone_tupleRetainsSecond");
    assert!(
        tuple.matches("call i32 @loom_gc_clone_value_v1").count() >= 2,
        "tuple destructuring must clone both elements: {tuple}"
    );
    assert!(
        tuple
            .lines()
            .filter(|line| line.contains("%tuple.data") && line.contains("= load ptr"))
            .count()
            >= 2,
        "tuple head must be reloaded after a preceding clone can collect: {tuple}"
    );
    assert_gc_state_published_before(tuple, "@loom_gc_clone_value_v1");

    let matched = llvm_function(&llvm, "moving_gc_projection_clone_matchRetainsSecond");
    assert!(
        matched.matches("match.binding.proxy").count() >= 2,
        "all derived match bindings must be staged in stable roots: {matched}"
    );
    assert!(
        matched.matches("call i32 @loom_gc_clone_value_v1").count() >= 2,
        "match must clone both staged bindings: {matched}"
    );
    assert_gc_state_published_before(matched, "@loom_gc_clone_value_v1");

    assert_emitted_main_succeeds(&project);
}

#[test]
fn moving_gc_relocates_async_task_slots_across_pending_and_resume() {
    let source = r#"module moving_gc_async

async fn allocateAcrossAwait(count Int) Int {
    var values = List[Text]()
    for index in 0..count {
        values.add("before-await")
        Unit
    }
    let before = values.length()
    assert before == count
    Task.sleep(1).await
    for index in 0..count {
        values.add("after-await")
        Unit
    }
    values.length()
}

pub async fn main() {
    let count = allocateAcrossAwait(4096).await
    assert count == 8192
}
"#;
    let (project, _program, llvm) = emit_source_with_ir(source);

    let allocate = llvm_resume_function(&llvm, "moving_gc_async_allocateAcrossAwait");
    let main = llvm_resume_function(&llvm, "moving_gc_async_main");
    assert_balanced_gc_root_frame(allocate);
    assert_balanced_gc_root_frame(main);
    assert!(!allocate.contains("@loom_gc_safepoint_v1"), "{allocate}");
    assert!(
        allocate.contains("i32 0"),
        "pending return is missing: {allocate}"
    );
    assert_gc_state_published_before(allocate, "@loom_runtime_list_add");
    let descriptor = gc_root_descriptor(&llvm, allocate);
    assert_eq!(
        descriptor.bitmaps.len(),
        descriptor.state_count * descriptor.bitmap_words,
        "{descriptor:?}\n{allocate}"
    );

    assert_emitted_main_succeeds(&project);
}

#[test]
fn primitive_scalar_abi_uses_i1_i64_and_double_without_universal_calls() {
    let source = r"module primitive_scalar

fn negate(value Bool) Bool {
    !value
}

fn double(value Float) Float {
    value * 2.0
}

fn unitIdentity(value Unit) {
    value
}

pub fn main() {
    let negated = negate(false)
    assert negated
    let doubled = double(2.5)
    assert doubled == 5.0
    unitIdentity(Unit)
}
";
    let project = tempfile::tempdir().expect("create primitive scalar project");
    std::fs::write(project.path().join("main.loom"), source)
        .expect("write primitive scalar source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load primitive scalar project")
        .snapshot()
        .expect("analyze primitive scalar project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower primitive scalar MIR");

    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(program, &executable, &options).expect("emit primitive scalar executable");

    let llvm = std::fs::read_to_string(ir).expect("read primitive scalar LLVM IR");
    let negate = llvm_native_function(&llvm, "primitive_scalar_negate");
    let double = llvm_native_function(&llvm, "primitive_scalar_double");
    let unit = llvm_native_function(&llvm, "primitive_scalar_unitIdentity");
    let main = llvm_native_function(&llvm, "primitive_scalar_main");
    let negate_symbol = llvm_defined_symbol(negate);
    let double_symbol = llvm_defined_symbol(double);
    let unit_symbol = llvm_defined_symbol(unit);
    assert!(
        negate.lines().next().unwrap_or_default().contains("i1 %0"),
        "{negate}"
    );
    assert!(
        double
            .lines()
            .next()
            .unwrap_or_default()
            .contains("double %0"),
        "{double}"
    );
    assert!(
        unit.lines().next().unwrap_or_default().contains("i1 %0"),
        "{unit}"
    );
    assert!(
        main.contains(&format!("call i1 @{negate_symbol}")),
        "{main}"
    );
    assert!(
        main.contains(&format!("call double @{double_symbol}")),
        "{main}"
    );
    assert!(main.contains(&format!("call i1 @{unit_symbol}")), "{main}");
    assert!(!main.contains("%loom.ArgNode"), "{main}");

    let output = Command::new(executable)
        .output()
        .expect("run primitive scalar executable");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
#[allow(clippy::too_many_lines)]
fn loop_temporaries_stay_in_one_shot_prologues_and_large_release_loops_do_not_grow_stack() {
    let source = r#"module stack_loop

record Counter {
    total Int
    calls Int
}

impl Counter {
    method add(mut self, value Int) {
        self.total = self.total + value
        self.calls = self.calls + 1
    }
}

fn modularProduct(left Int, right Int) Int {
    let modulus = 2147483647
    let product = left * right
    product - (product / modulus) * modulus
}

fn spin(size Int) Int {
    var state = 1
    for ignored in 0..size {
        state = modularProduct(state, 48271)
        Unit
    }
    state
}

fn periodicValue(index Int) Int {
    index - (index / 1024) * 1024
}

fn recordMethod(size Int) Counter {
    var counter = Counter { total = 0, calls = 0 }
    for index in 0..size {
        counter.add(periodicValue(index))
        Unit
    }
    counter
}

fn consumeCounter(counter Counter) Int {
    counter.total + counter.calls
}

fn scalarRecord() Int {
    let counter = Counter { total = 20, calls = 22 }
    counter.total + counter.calls
}

fn collectingField(value Text) Int {
    var values = List[Text]()
    values.add(value)
    values.length()
}

fn recordWithCollectingField(value Text) Int {
    let counter = Counter { total = collectingField(value), calls = 2 }
    counter.total + counter.calls
}

fn nestedLoops(size Int) Int {
    var total = 0
    for outer in 0..size {
        for inner in 0..size {
            total = total + 1
            Unit
        }
        Unit
    }
    total
}

pub fn main() {
    let state = spin(100000)
    assert state == 1405402365
    let counter = recordMethod(100000)
    assert counter.total == 51031728
    assert counter.calls == 100000
    let fallbackValue = consumeCounter(recordMethod(1))
    assert fallbackValue == 1
    let scalarRecordValue = scalarRecord()
    assert scalarRecordValue == 42
    let collectingRecordValue = recordWithCollectingField("rooted")
    assert collectingRecordValue == 3
    let nested = nestedLoops(100)
    assert nested == 10000
    var original = Counter { total = 0, calls = 0 }
    var copied = original
    copied.add(7)
    let originalTotal = original.total
    let originalCalls = original.calls
    let copiedTotal = copied.total
    let copiedCalls = copied.calls
    assert originalTotal == 0
    assert originalCalls == 0
    assert copiedTotal == 7
    assert copiedCalls == 1
}
"#;
    let project = tempfile::tempdir().expect("create loop stack project");
    std::fs::write(project.path().join("main.loom"), source).expect("write loop stack source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load loop stack project")
        .snapshot()
        .expect("analyze loop stack project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower loop stack MIR");

    let development_ir = project.path().join("development.ll");
    let development_object = project.path().join("development.o");
    let mut development = EmitOptions::run("main");
    development.emit_ir = Some(development_ir.clone());
    emit_native_object(program, &development_object, &development)
        .expect("emit development loop IR");
    let llvm = std::fs::read_to_string(development_ir).expect("read development loop IR");
    for function_name in [
        "stack_loop_spin",
        "stack_loop_recordMethod",
        "stack_loop_scalarRecord",
        "stack_loop_nestedLoops",
    ] {
        let signature = llvm
            .lines()
            .find(|line| line.starts_with("define ") && line.contains(function_name))
            .unwrap_or_else(|| panic!("missing {function_name} in development IR"));
        let start = llvm.find(signature).expect("locate function signature");
        let body = &llvm[start..];
        let end = body.find("\n}").expect("locate function end");
        let mut block = "";
        for line in body[..end].lines().skip(1) {
            if !line.starts_with(char::is_whitespace)
                && let Some((label, _)) = line.split_once(':')
            {
                block = label;
            }
            assert!(
                !line.contains(" alloca ") || matches!(block, "entry" | "body.start"),
                "{function_name} contains a dynamic alloca in `{block}`: {line}"
            );
        }
    }
    let add = llvm_native_function(&llvm, "stack_loop_add");
    assert!(add.contains("copy.scalar"), "{add}");
    assert!(add.contains("assign.scalar"), "{add}");
    assert!(!add.contains("move = load %loom.Value"), "{add}");
    assert!(!add.contains("@loom_gc_build_value_nodes_v1"), "{add}");
    assert!(!add.contains("@loom_gc_clone_value_v1"), "{add}");
    assert!(!add.contains("@loom_gc_root_"), "{add}");
    assert!(!add.contains("@loom_gc_safepoint_v1"), "{add}");
    let modular_product = llvm_native_function(&llvm, "stack_loop_modularProduct");
    assert!(
        modular_product
            .lines()
            .next()
            .is_some_and(|line| line.contains("define internal i64") && !line.contains("ptr")),
        "{modular_product}"
    );
    assert!(
        !modular_product.contains("with.overflow"),
        "{modular_product}"
    );
    assert!(modular_product.contains("mul nsw i64"), "{modular_product}");
    assert!(modular_product.contains("sdiv i64"), "{modular_product}");
    let modular_product_symbol = llvm_defined_symbol(modular_product);
    let spin = llvm_native_function(&llvm, "stack_loop_spin");
    assert!(
        spin.contains(&format!("call i64 @{modular_product_symbol}")),
        "{spin}"
    );
    let record_method = llvm_native_function(&llvm, "stack_loop_recordMethod");
    assert!(
        record_method.lines().next().is_some_and(|line| line
            .contains("define internal { i64, i64 }")
            && !line.contains("ptr")),
        "{record_method}"
    );
    assert!(record_method.contains("record.local"), "{record_method}");
    assert!(
        !record_method.contains("gc.root.stack.record.value"),
        "POD field slots cannot contain managed pointers: {record_method}"
    );
    assert!(
        !gc_root_slot_is_registered(record_method, "record.local."),
        "POD field storage entered the shadow-root frame: {record_method}"
    );
    assert!(
        !gc_root_slot_is_registered(record_method, "local.1.counter"),
        "the private POD header entered the shadow-root frame: {record_method}"
    );
    assert!(
        !record_method.contains("@loom_gc_safepoint_v1"),
        "private record hot loops have no synthetic safepoint: {record_method}"
    );
    assert!(
        record_method.contains("record.copy.private"),
        "{record_method}"
    );
    assert!(
        !record_method.contains("@loom_gc_clone_value_v1"),
        "{record_method}"
    );
    assert!(!record_method.contains("@loom_gc_build_value_nodes_v1"));
    assert!(!record_method.contains("@loom_gc_root_"));
    assert!(!record_method.contains("node.next"), "{record_method}");
    let universal_record_method = llvm_function(&llvm, "stack_loop_recordMethod");
    assert_balanced_gc_root_frame(universal_record_method);
    assert_eq!(
        universal_record_method
            .matches("call i32 @loom_gc_build_value_nodes_v1")
            .count(),
        1,
        "the universal Value result must retain its moving-GC materialization: {universal_record_method}"
    );
    assert_gc_state_published_before(
        universal_record_method,
        "call i32 @loom_gc_build_value_nodes_v1",
    );
    let record_add = llvm_native_function(&llvm, "stack_loop_add");
    assert_eq!(
        record_add.matches("add nsw i64").count(),
        2,
        "both closed record recurrences must be proved before ABI selection: {record_add}"
    );
    assert!(!record_add.contains("with.overflow"), "{record_add}");
    let scalar_record = llvm_native_function(&llvm, "stack_loop_scalarRecord");
    assert!(!scalar_record.contains("gc.root.frame"), "{scalar_record}");
    assert!(
        !scalar_record.contains("@loom_gc_root_push_v1"),
        "{scalar_record}"
    );
    assert!(
        !scalar_record.contains("@loom_gc_safepoint_v1"),
        "{scalar_record}"
    );
    let collecting_record = llvm_function(&llvm, "stack_loop_recordWithCollectingField");
    assert_balanced_gc_root_frame(collecting_record);
    assert!(
        !gc_root_slot_is_registered(collecting_record, "record.local."),
        "POD fields entered roots around a collecting field call: {collecting_record}"
    );
    assert_gc_state_published_before(collecting_record, "call i32 @loom.fn.");
    let main = llvm_native_function(&llvm, "stack_loop_main");
    assert!(
        main.contains("record.copy.field"),
        "the original-to-copy path must use independent managed fields: {main}"
    );

    let executable = project.path().join("release-program");
    let release_ir = project.path().join("release.ll");
    let mut release = EmitOptions::run("main").with_optimization(OptimizationProfile::Release);
    release.emit_ir = Some(release_ir.clone());
    emit_native(program, &executable, &release).expect("emit release loop executable");
    let release_llvm = std::fs::read_to_string(release_ir).expect("read release loop IR");
    if let Some(release_record_method) = llvm_any_function(&release_llvm, "stack_loop_recordMethod")
    {
        assert!(
            !release_record_method.contains("with.overflow"),
            "closed record recurrences must remain unchecked after optimization: {release_record_method}"
        );
        assert!(
            !release_record_method.contains("node.next"),
            "the optimized loop must not recover the universal record chain: {release_record_method}"
        );
    }
    let output = Command::new(executable)
        .output()
        .expect("run release loop executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
fn private_pod_results_flow_through_status_tail_return_and_branch_calls() {
    let source = r"module pod_result_flow

record Pair { value Int }

fn make(value Int) Pair {
    Pair { value = value }
}

fn checkedMake(value Int, accepted Bool) Pair {
    assert accepted
    Pair { value = value }
}

fn forward(value Int) Pair {
    make(value)
}

fn explicit(value Int) Pair {
    return make(value)
}

fn choose(value Int, left Bool) Pair {
    if left {
        make(value)
    } else {
        make(value + 1)
    }
}

pub fn main() {
    let checked = checkedMake(7, true)
    assert checked.value == 7
    let forwarded = forward(42)
    assert forwarded.value == 42
    let returned = explicit(43)
    assert returned.value == 43
    let selected = choose(44, true)
    assert selected.value == 44
}

pub fn faultMain() {
    let rejected = checkedMake(9, false)
    assert rejected.value == 9
}
";
    let (project, program, llvm) = emit_source_with_ir(source);

    let checked = llvm_native_function(&llvm, "pod_result_flow_checkedMake");
    assert!(
        checked
            .lines()
            .next()
            .is_some_and(|line| line.contains("define internal { i32, { i64 } }")),
        "{checked}"
    );
    let main = llvm_native_function(&llvm, "pod_result_flow_main");
    assert!(
        main.contains("call { i32, { i64 } } @loom.native.fn.")
            && main.contains("pod_result_flow_checkedMake"),
        "{main}"
    );
    assert!(main.contains("native.call.value"), "{main}");

    for suffix in [
        "pod_result_flow_checkedMake",
        "pod_result_flow_forward",
        "pod_result_flow_explicit",
        "pod_result_flow_choose",
    ] {
        let function = llvm_native_function(&llvm, suffix);
        assert!(
            !function.contains("@loom.fn."),
            "private POD flow fell back to the universal ABI: {function}"
        );
        assert!(
            !function.contains("@loom_gc_build_value_nodes_v1"),
            "{function}"
        );
        assert!(!function.contains("@loom_gc_clone_value_v1"), "{function}");
    }

    for suffix in ["pod_result_flow_forward", "pod_result_flow_explicit"] {
        let function = llvm_native_function(&llvm, suffix);
        assert!(
            function.contains("call { i64 } @loom.native.fn.")
                && function.contains("pod_result_flow_make"),
            "{function}"
        );
    }
    let choose = llvm_native_function(&llvm, "pod_result_flow_choose");
    assert!(
        choose.matches("pod_result_flow_make").count() >= 2,
        "{choose}"
    );

    assert_emitted_main_succeeds(&project);

    let fault_executable = project.path().join("fault-program");
    emit_native(&program, &fault_executable, &EmitOptions::run("faultMain"))
        .expect("emit faulting private POD executable");
    let output = Command::new(fault_executable)
        .output()
        .expect("run faulting private POD executable");
    assert!(!output.status.success(), "{output:?}");
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostic.contains("AssertionFault"), "{output:?}");
}

const POD_VALUE_PARAMETERS_SOURCE: &str = r#"module pod_value_parameters

record Pair {
    left Int
    right Int
}

fn sum(value Pair) Int {
    value.left + value.right
}

fn duplicate(value Pair) Pair {
    value
}

fn forward(value Pair) Pair {
    duplicate(value)
}

impl Pair {
    method total(self) Int {
        self.left + self.right
    }

    method shifted(self, amount Int) Pair {
        Pair {
            left = self.left + amount,
            right = self.right
        }
    }

    method checkedCopy(self, accepted Bool) Pair {
        assert accepted
        self
    }

    method addToLeft(mut self, amount Int) {
        self.left = self.left + amount
    }
}

pub fn main() {
    let original = Pair { left = 3, right = 4 }
    let summed = sum(original)
    assert summed == 7
    let total = original.total()
    assert total == 7

    var copied = forward(original)
    copied.addToLeft(10)
    let copiedLeft = copied.left
    let copiedRight = copied.right
    assert copiedLeft == 13
    assert copiedRight == 4
    assert original.left == 3
    assert original.right == 4

    let shifted = original.shifted(5)
    assert shifted.left == 8
    assert shifted.right == 4

    var checked = original.checkedCopy(true)
    checked.addToLeft(20)
    let checkedLeft = checked.left
    assert checkedLeft == 23
    assert original.left == 3

    let boundary = publicBoundary("")
    assert boundary == 24
}

pub fn faultMain() {
    let original = Pair { left = 3, right = 4 }
    let rejected = original.checkedCopy(false)
    assert rejected.left == 3
}

pub fn overflowMain() {
    let overflowing = Pair { left = 9223372036854775807, right = 1 }
    let impossible = sum(overflowing)
    assert impossible == 0
}

pub fn publicBoundary(ignored Text) Int {
    let value = Pair { left = 5, right = 7 }
    let ordinary = sum(value)
    let readonly = value.total()
    let length = ignored.length()
    ordinary + readonly + length
}
"#;

#[test]
fn private_pod_value_parameters_and_readonly_receivers_use_aggregate_values() {
    let (project, _program, llvm) = emit_source_with_ir(POD_VALUE_PARAMETERS_SOURCE);

    let sum = llvm_native_function(&llvm, "pod_value_parameters_sum");
    assert!(
        sum.lines().next().is_some_and(|line| {
            line.contains("define internal i64") && line.contains("({ i64, i64 }")
        }),
        "{sum}"
    );
    let duplicate = llvm_native_function(&llvm, "pod_value_parameters_duplicate");
    assert!(
        duplicate.lines().next().is_some_and(|line| {
            line.contains("define internal { i64, i64 }") && line.contains("({ i64, i64 }")
        }),
        "{duplicate}"
    );
    let readonly = llvm_native_function(&llvm, "pod_value_parameters_total");
    assert!(
        readonly.lines().next().is_some_and(|line| {
            line.contains("define internal i64") && line.contains("({ i64, i64 }")
        }),
        "{readonly}"
    );
    assert!(
        !readonly.lines().next().unwrap().contains("ptr %0"),
        "{readonly}"
    );

    let main = llvm_native_function(&llvm, "pod_value_parameters_main");
    for callee in [
        "pod_value_parameters_sum",
        "pod_value_parameters_total",
        "pod_value_parameters_forward",
        "shifted",
        "checkedCopy",
    ] {
        assert!(
            main.contains(callee),
            "missing aggregate call `{callee}`: {main}"
        );
    }
    let forward = llvm_native_function(&llvm, "pod_value_parameters_forward");
    assert!(
        forward.contains("call { i64, i64 } @loom.native.fn.")
            && forward.contains("pod_value_parameters_duplicate"),
        "{forward}"
    );
    let checked = llvm_native_function(&llvm, "checkedCopy");
    assert!(
        checked.lines().next().is_some_and(|line| {
            line.contains("define internal { i32, { i64, i64 } }") && line.contains("({ i64, i64 }")
        }),
        "{checked}"
    );

    let boundary = llvm_function(&llvm, "pod_value_parameters_publicBoundary");
    assert!(
        boundary.contains("@loom.native.fn.")
            && boundary.contains("pod_value_parameters_sum")
            && boundary.contains("pod_value_parameters_total"),
        "{boundary}"
    );
    assert!(
        !boundary.lines().any(|line| {
            line.contains("call i32 @loom.fn.")
                && (line.contains("pod_value_parameters_sum")
                    || line.contains("pod_value_parameters_total"))
        }),
        "{boundary}"
    );

    for suffix in [
        "pod_value_parameters_sum",
        "pod_value_parameters_duplicate",
        "pod_value_parameters_forward",
        "pod_value_parameters_total",
        "shifted",
        "checkedCopy",
    ] {
        let function = llvm_native_function(&llvm, suffix);
        for forbidden in [
            "@loom.fn.",
            "@loom_gc_build_value_nodes_v1",
            "@loom_gc_clone_value_v1",
            "@loom_gc_root_",
            "@loom_gc_safepoint_v1",
        ] {
            assert!(
                !function.contains(forbidden),
                "POD aggregate value path contains `{forbidden}`: {function}"
            );
        }
    }

    assert_emitted_main_succeeds(&project);
}

#[test]
fn private_pod_value_parameter_propagates_checked_integer_faults() {
    let (project, program, _llvm) = emit_source_with_ir(POD_VALUE_PARAMETERS_SOURCE);
    let executable = project.path().join("fault-program");
    let ir = project.path().join("fault-program.ll");
    let mut options = EmitOptions::run("overflowMain");
    options.emit_ir = Some(ir.clone());
    emit_native(&program, &executable, &options).expect("emit faulting POD value executable");

    let llvm = std::fs::read_to_string(ir).expect("read faulting POD value IR");
    let sum = llvm_native_function(&llvm, "pod_value_parameters_sum");
    assert!(
        sum.lines().next().is_some_and(|line| {
            line.contains("define internal { i32, i64 }") && line.contains("({ i64, i64 }")
        }),
        "{sum}"
    );
    assert!(!sum.contains("@loom.fn."), "{sum}");
    assert!(sum.contains("llvm.sadd.with.overflow.i64"), "{sum}");

    let output = Command::new(executable)
        .output()
        .expect("run faulting POD value executable");
    assert!(!output.status.success(), "{output:?}");
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostic.contains("IntegerOverflow"), "{output:?}");
}

#[test]
fn private_pod_readonly_result_propagates_assertion_faults() {
    let (project, program, _llvm) = emit_source_with_ir(POD_VALUE_PARAMETERS_SOURCE);
    let executable = project.path().join("readonly-fault-program");
    emit_native(&program, &executable, &EmitOptions::run("faultMain"))
        .expect("emit faulting readonly POD executable");

    let output = Command::new(executable)
        .output()
        .expect("run faulting readonly POD executable");
    assert!(!output.status.success(), "{output:?}");
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostic.contains("AssertionFault"), "{output:?}");
}

#[test]
fn private_pod_inout_writes_back_before_fault_status_propagates() {
    let source = r"module pod_fault_writeback

record Counter { value Int }

impl Counter {
    method setThenAssert(mut self, accepted Bool) {
        self.value = 9
        assert accepted
    }
}

fn exercise() {
    var counter = Counter { value = 1 }
    counter.setThenAssert(true)
    let observed = counter.value
    assert observed == 9
}

pub fn main() {
    exercise()
}
";
    let (project, _program, llvm) = emit_source_with_ir(source);

    let method = llvm_native_function(&llvm, "pod_fault_writeback_setThenAssert");
    assert!(method.contains("define internal { i32, i1 }"), "{method}");
    assert!(
        !method.contains("@loom_gc_build_value_nodes_v1"),
        "{method}"
    );
    assert!(!method.contains("@loom_gc_clone_value_v1"), "{method}");
    assert!(!method.contains("@loom_gc_root_"), "{method}");
    assert!(!method.contains("@loom_gc_safepoint_v1"), "{method}");
    let failure = method.find("assert.fail").expect("assertion failure block");
    let writeback = method[failure..]
        .find("native.writeback")
        .map(|offset| failure + offset)
        .expect("receiver writeback on the failure edge");
    let returned = method[writeback..]
        .find("ret { i32, i1 }")
        .map(|offset| writeback + offset)
        .expect("native status return after receiver writeback");
    assert!(failure < writeback && writeback < returned, "{method}");

    let caller = llvm_native_function(&llvm, "pod_fault_writeback_exercise");
    let call = caller
        .find("pod_fault_writeback_setThenAssert")
        .expect("private POD method call");
    let unpack = caller[call..]
        .find("native.call.inout.result")
        .map(|offset| call + offset)
        .expect("caller receiver writeback");
    let status = caller[unpack..]
        .find("call.ok")
        .map(|offset| unpack + offset)
        .expect("status propagation after receiver writeback");
    assert!(call < unpack && unpack < status, "{caller}");

    assert_emitted_main_succeeds(&project);
}

#[test]
fn private_pod_release_hot_path_has_no_universal_value_or_gc_operations() {
    let source = r"module private_pod_release

record Counter {
    total Int
    calls Int
}

impl Counter {
    method add(mut self, value Int) {
        self.total = self.total + value
        self.calls = self.calls + 1
    }
}

fn periodicValue(index Int) Int {
    index - (index / 1024) * 1024
}

fn recordMethod(size Int) Counter {
    var counter = Counter { total = 0, calls = 0 }
    for index in 0..size {
        counter.add(periodicValue(index))
        Unit
    }
    counter
}

pub fn main() {
    let counter = recordMethod(100000)
    assert counter.total == 51031728
    assert counter.calls == 100000
}
";
    let project = tempfile::tempdir().expect("create private POD release project");
    std::fs::write(project.path().join("main.loom"), source).expect("write private POD source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load private POD project")
        .snapshot()
        .expect("analyze private POD project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let executable = project.path().join("program");
    let ir = project.path().join("release.ll");
    let mut options = EmitOptions::run("main").with_optimization(OptimizationProfile::Release);
    options.emit_ir = Some(ir.clone());
    emit_native(
        snapshot.executable().expect("lower private POD MIR"),
        &executable,
        &options,
    )
    .expect("emit private POD release executable");
    let llvm = std::fs::read_to_string(ir).expect("read private POD release IR");

    let mut hot_functions =
        vec![llvm_any_function(&llvm, "i32 @main(").expect("release C entry point")];
    for suffix in [
        "private_pod_release_main",
        "private_pod_release_recordMethod",
        "private_pod_release_add",
    ] {
        if let Some(function) = llvm_any_function(&llvm, suffix) {
            hot_functions.push(function);
        }
    }
    for function in hot_functions {
        for forbidden in [
            "%loom.ValueNode",
            "node.next",
            "@loom_gc_build_value_nodes_v1",
            "@loom_gc_clone_value_v1",
            "@loom_gc_root_",
            "@loom_gc_safepoint_v1",
        ] {
            assert!(
                !function.contains(forbidden),
                "release private POD hot path contains `{forbidden}`: {function}"
            );
        }
    }
    assert!(
        !llvm.lines().any(|line| {
            line.starts_with("define internal i32 @loom.fn.")
                && (line.contains("private_pod_release_recordMethod")
                    || line.contains("private_pod_release_add"))
        }),
        "unreferenced universal POD fallbacks must be eliminated in release IR: {llvm}"
    );

    let output = Command::new(executable)
        .output()
        .expect("run private POD release executable");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
fn projected_pod_copy_and_move_publish_the_private_native_result() {
    let source = r#"module projected_pod_publication

record Pair { left Int, right Int }
record Holder { selected Pair, guard Bool }

fn select() Pair {
    let holder = Holder {
        selected = Pair { left = 11, right = 29 },
        guard = true,
    }
    holder.selected
}

pub fn main() {
    let keepRoot = "projected-pod"
    let pair = select()
    assert pair.left == 11
    assert pair.right == 29
    discard keepRoot
}
"#;
    let (copy_project, copy_program, copy_ir) = emit_source_with_ir(source);
    assert_emitted_main_succeeds(&copy_project);
    let copy = llvm_native_function(&copy_ir, "projected_pod_publication_select");
    assert!(copy.contains("record.copy.projected.native"), "{copy}");

    let mut raw = copy_program.into_program();
    let select = raw
        .functions
        .iter_mut()
        .find(|function| function.name.ends_with("select"))
        .expect("find projected POD producer");
    let tail = select
        .body
        .tail
        .as_deref_mut()
        .expect("projected POD producer tail");
    let ExprKind::Copy(place) = &tail.kind else {
        panic!("projected POD source tail is not a copy: {tail:?}")
    };
    tail.kind = ExprKind::Move(place.clone());
    let moved = raw
        .into_checked()
        .expect("projected POD move consumes its complete Holder root");
    let main = *moved.as_program().exports.get("main").expect("main export");
    assert_eq!(
        Interpreter::new(&moved)
            .invoke(main, Vec::new(), loom_core::Span::default())
            .expect("interpret projected POD move"),
        loom_interpreter::Value::Unit,
    );

    let move_project = tempfile::tempdir().expect("create projected move project");
    let executable = move_project.path().join("program");
    let ir_path = move_project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir_path.clone());
    emit_native(&moved, &executable, &options).expect("emit projected POD move");
    let output = Command::new(&executable)
        .output()
        .expect("run projected POD move");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"Unit\n");
    let move_ir = std::fs::read_to_string(ir_path).expect("read projected move IR");
    let moved = llvm_native_function(&move_ir, "projected_pod_publication_select");
    assert!(moved.contains("record.copy.projected.native"), "{moved}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn readonly_list_builtins_snapshot_the_header_and_clone_only_the_selected_value() {
    let source = r"module list_readonly

record Boxed {
    value Int
}

impl Boxed {
    method bump(mut self) {
        self.value = self.value + 1
    }
}

record Holder {
    values List[Int]
}

impl Holder {
    method appendAndChoose(mut self) Int {
        self.values.add(11)
        2
    }
}

fn readAt(values List[Int], index Int) Int {
    let length = values.length()
    assert length == 2
    match values.get(index) {
        Some(value) => value
        None => -1
    }
}

async fn delayedIndex() Int {
    Task.sleep(1).await
    0
}

pub async fn main() {
    var numbers = List[Int]()
    numbers.add(7)
    numbers.add(9)
    let selectedNumber = readAt(numbers, 1)
    assert selectedNumber == 9

    var holder = Holder { values = numbers }
    let beforeArgumentMutation = holder.values.get(holder.appendAndChoose())
    match beforeArgumentMutation {
        Some(_) => {
            assert false
            Unit
        }
        None => Unit
    }
    let numberCount = holder.values.length()
    assert numberCount == 3

    var boxes = List[Boxed]()
    let original = Boxed { value = 1 }
    boxes.add(original)
    var extracted = match boxes.get(0) {
        Some(value) => value
        None => Boxed { value = -1 }
    }
    extracted.bump()
    let stored = match boxes.get(0) {
        Some(value) => value
        None => Boxed { value = -1 }
    }
    let extractedValue = extracted.value
    assert extractedValue == 2
    assert stored.value == 1

    var delayedValues = List[Int]()
    delayedValues.add(42)
    let delayed = delayedValues.get(delayedIndex().await)
    match delayed {
        Some(value) => {
            assert value == 42
            Unit
        }
        None => {
            assert false
            Unit
        }
    }
}
";
    let project = tempfile::tempdir().expect("create readonly List project");
    std::fs::write(project.path().join("main.loom"), source).expect("write readonly List source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load readonly List project")
        .snapshot()
        .expect("analyze readonly List project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower readonly List MIR");

    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(program, &executable, &options).expect("emit readonly List executable");

    let llvm = std::fs::read_to_string(ir).expect("read readonly List LLVM IR");
    let read_at = llvm_function(&llvm, "list_readonly_readAt");
    let get = read_at
        .find("@loom_runtime_list_get")
        .expect("readAt calls native List.get");
    assert!(
        read_at[..get].contains("list.readonly.snapshot"),
        "{read_at}"
    );
    assert!(
        !read_at[..get].contains("@loom_gc_clone_value_v1"),
        "{read_at}"
    );
    assert!(
        read_at[get..].contains("@loom_gc_clone_value_v1"),
        "{read_at}"
    );
    let option_match_branches = read_at
        .lines()
        .filter(|line| {
            line.trim_start().starts_with("br i1 ")
                && line.contains("label %match.arm")
                && (line.contains("label %match.next") || line.contains("label %pattern.payload"))
        })
        .collect::<Vec<_>>();
    assert!(!option_match_branches.is_empty(), "{read_at}");
    assert!(
        option_match_branches
            .iter()
            .all(|line| !line.contains("!prof")),
        "{option_match_branches:#?}"
    );

    let output = Command::new(executable)
        .output()
        .expect("run readonly List executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
#[allow(clippy::too_many_lines)]
fn synchronous_local_int_lists_use_contiguous_native_storage_by_shape() {
    let source = r"module native_int_list

fn renamedSameShape(size Int) Int {
    var items = List[Int]()
    for index in 0..size {
        items.add(index * 3 - 1)
        Unit
    }
    var checksum = 0
    for index in 0..size {
        match items.get(index) {
            Some(value) => {
                checksum = checksum + value
                Unit
            }
            None => {
                assert false
                Unit
            }
        }
        Unit
    }
    let count = items.length()
    assert count == size
    checksum
}

fn appendOnly(size Int) Int {
    var values = List[Int]()
    for index in 0..size {
        values.add(index)
        Unit
    }
    values.length()
}

fn negativeRange() Int {
    var values = List[Int]()
    for index in 3..-2 {
        values.add(index)
        Unit
    }
    values.length()
}

fn nonemptyEmptyRange() Int {
    var values = List[Int]()
    values.add(41)
    for index in 3..-2 {
        values.add(index)
        Unit
    }
    values.length()
}

fn earlyReturnAppend(size Int) Int {
    var values = List[Int]()
    for index in 0..size {
        values.add(if index == 7 {
            return 77
        } else {
            index
        })
        Unit
    }
    values.length()
}

fn offsetRange() Int {
    var values = List[Int]()
    for index in 3..7 {
        values.add(if index < 5 {
            index
        } else {
            index + 0
        })
        Unit
    }
    var checksum = 0
    for index in 0..4 {
        match values.get(index) {
            Some(value) => {
                checksum = checksum + value
                Unit
            }
            None => {
                assert false
                Unit
            }
        }
        Unit
    }
    checksum
}

fn twoAppends(size Int) Int {
    var values = List[Int]()
    for index in 0..size {
        values.add(index)
        values.add(index + 1)
        Unit
    }
    values.length()
}

fn appendObservedLength() Int {
    var values = List[Int]()
    values.add(99)
    for index in 0..4 {
        values.add(values.length())
        Unit
    }
    let count = values.length()
    let last = match values.get(4) {
        Some(value) => value
        None => -100
    }
    count * 10 + last
}

fn checkedElement(index Int, accepted Bool) Int {
    assert accepted || index != 7
    index
}

fn faultableAppend(size Int, accepted Bool) Int {
    var values = List[Int]()
    for index in 0..size {
        values.add(checkedElement(index, accepted))
        Unit
    }
    values.length()
}

fn edgeReads() Int {
    var numbers = List[Int]()
    let empty = match numbers.get(0) {
        Some(_) => -100
        None => 1
    }
    for value in 0..40 {
        numbers.add(value + 10)
        Unit
    }
    let negative = match numbers.get(-1) {
        Some(_) => -100
        None => 2
    }
    let pastEnd = match numbers.get(40) {
        Some(_) => -100
        None => 4
    }
    let last = match numbers.get(39) {
        Some(value) => value
        None => -100
    }
    empty + negative + pastEnd + last
}

fn cleanupOnFaultPath(accepted Bool) Int {
    var values = List[Int]()
    values.add(41)
    assert accepted
    match values.get(0) {
        Some(value) => value + 1
        None => -1
    }
}

fn interveningAppendKeepsCheck(size Int) Int {
    var values = List[Int]()
    for index in 0..size {
        values.add(index)
        Unit
    }
    values.add(99)
    var checksum = 0
    for index in 0..size {
        match values.get(index) {
            Some(value) => {
                checksum = checksum + value
                Unit
            }
            None => {
                assert false
                Unit
            }
        }
        Unit
    }
    checksum
}

fn noneSideEffectFallsBack(buildSize Int, scanSize Int) Int {
    var values = List[Int]()
    for index in 0..buildSize {
        values.add(index)
        Unit
    }
    var noneCount = 0
    for index in 0..scanSize {
        match values.get(index) {
            Some(_) => Unit
            None => {
                noneCount = noneCount + 1
                Unit
            }
        }
        Unit
    }
    noneCount
}

pub fn main() {
    let scan = renamedSameShape(40)
    let empty = renamedSameShape(0)
    let emptyAppend = appendOnly(0)
    let one = appendOnly(1)
    let negative = negativeRange()
    let nonemptyEmpty = nonemptyEmptyRange()
    let earlyReturn = earlyReturnAppend(40)
    let offset = offsetRange()
    let grown = appendOnly(40)
    let fallback = twoAppends(20)
    let observed = appendObservedLength()
    let faultable = faultableAppend(40, true)
    let edges = edgeReads()
    let cleanup = cleanupOnFaultPath(true)
    let intervening = interveningAppendKeepsCheck(40)
    let noneEffects = noneSideEffectFallsBack(2, 3)
    assert scan == 2300
    assert empty == 0
    assert emptyAppend == 0
    assert one == 1
    assert negative == 0
    assert nonemptyEmpty == 1
    assert earlyReturn == 77
    assert offset == 18
    assert grown == 40
    assert fallback == 40
    assert observed == 54
    assert faultable == 40
    assert edges == 56
    assert cleanup == 42
    assert intervening == 780
    assert noneEffects == 1
}

pub fn faultMain() {
    let count = faultableAppend(40, false)
    assert count == 40
}
";
    let project = tempfile::tempdir().expect("create native Int list project");
    std::fs::write(project.path().join("main.loom"), source).expect("write native Int list source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load native Int list project")
        .snapshot()
        .expect("analyze native Int list project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower native Int list MIR");

    let development_ir = project.path().join("development.ll");
    let development_object = project.path().join("development.o");
    let mut development = EmitOptions::run("main");
    development.emit_ir = Some(development_ir.clone());
    emit_native_object(program, &development_object, &development)
        .expect("emit development native Int list IR");

    let release_ir = project.path().join("release.ll");
    let executable = project.path().join("program");
    let mut release = EmitOptions::run("main").with_optimization(OptimizationProfile::Release);
    release.emit_ir = Some(release_ir.clone());
    emit_native(program, &executable, &release).expect("emit release native Int list executable");

    for ir in [&development_ir, &release_ir] {
        let llvm = std::fs::read_to_string(ir).expect("read native Int list LLVM IR");
        let scan = llvm_native_function(&llvm, "native_int_list_renamedSameShape");
        assert!(!scan.contains("gc.root.frame"), "{scan}");
        assert!(!scan.contains("@loom_gc_root_push_v1"), "{scan}");
        assert!(!scan.contains("@loom_gc_root_pop_v1"), "{scan}");
        assert!(!scan.contains("@loom_gc_safepoint_v1"), "{scan}");
        assert!(scan.contains("@loom_int_list_reserve_v1"), "{scan}");
        assert!(scan.contains("@loom_int_list_drop_v1"), "{scan}");
        assert_eq!(
            scan.matches("call i32 @loom_int_list_reserve_v1").count(),
            1,
            "append must have one guarded growth call site: {scan}"
        );
        let reserve = scan
            .find("call i32 @loom_int_list_reserve_v1")
            .expect("native append has a reserve call");
        let reserve_block = scan[..reserve]
            .lines()
            .rev()
            .find(|line| !line.starts_with(char::is_whitespace) && line.contains(':'))
            .expect("reserve call is inside a basic block");
        assert!(
            reserve_block.contains("int.list.add.grow"),
            "reserve escaped the capacity-growth block `{reserve_block}`: {scan}"
        );
        assert!(!scan.contains("@loom_runtime_list_add"), "{scan}");
        assert!(!scan.contains("@loom_runtime_list_get"), "{scan}");
        assert!(!scan.contains("@loom_gc_alloc_value_node"), "{scan}");
        assert!(!scan.contains("list.get.owned"), "{scan}");
        assert!(!scan.contains("@loom_gc_clone_value_v1"), "{scan}");
        assert!(
            !scan.contains("int.list.get.past.end"),
            "exact completed append range must omit per-element upper-bound checks: {scan}"
        );
        assert!(
            !scan.contains("int.list.get.none"),
            "a statically exhaustive get must not emit the unreachable None arm: {scan}"
        );
        assert!(scan.contains("add nsw i64"), "{scan}");
        assert!(
            !scan.contains("llvm.sadd.with.overflow.i64"),
            "the independent range plan proves the completed scan checksum: {scan}"
        );

        if ir == &release_ir {
            for (name, instruction) in [
                ("int.list.loop.data", "= phi ptr"),
                ("int.list.loop.capacity", "= phi i64"),
                ("range.current.scalar", "= phi i64"),
            ] {
                assert!(
                    scan.lines()
                        .any(|line| line.contains(name) && line.contains(instruction)),
                    "release append loop lost `{name}` {instruction}: {scan}"
                );
            }
            assert!(
                scan.lines()
                    .any(|line| { line.contains("call i64 @llvm.vector.reduce.add.v") }),
                "exact native Int list scan lost its vector reduction: {scan}"
            );
            for reload in [
                "int.list.add.length",
                "int.list.add.capacity",
                "int.list.add.data",
                "int.list.add.index",
            ] {
                assert!(
                    !scan
                        .lines()
                        .any(|line| line.contains(reload) && line.contains("= load")),
                    "release hot loop retained `{reload}`: {scan}"
                );
            }
        }

        if ir == &development_ir {
            let append = llvm_native_function(&llvm, "native_int_list_appendOnly");
            for phi in [
                "int.list.loop.data = phi ptr",
                "int.list.loop.length = phi i64",
                "int.list.loop.capacity = phi i64",
                "range.current.scalar = phi i64",
            ] {
                assert!(append.contains(phi), "missing `{phi}`: {append}");
            }
            assert_deferred_native_int_list_length_commits(append);
            let nonempty_empty = llvm_native_function(&llvm, "native_int_list_nonemptyEmptyRange");
            assert_deferred_native_int_list_length_commits(nonempty_empty);
            assert!(
                append
                    .lines()
                    .any(|line| line.contains("store i64 %range.current.scalar")),
                "append value stopped consuming the proved SSA range binder: {append}"
            );
            let blocks = llvm_basic_blocks(append);
            let header = blocks
                .iter()
                .find_map(|(label, block)| label.starts_with("range.header.").then_some(*block))
                .expect("native append has a range header");
            assert!(
                !header.contains("load "),
                "native append reloaded an immutable range bound in its header: {append}"
            );
            for (name, instruction) in [
                ("int.list.loop.initial.data", "= load ptr"),
                ("int.list.loop.initial.length", "= load i64"),
                ("int.list.loop.initial.capacity", "= load i64"),
                ("int.list.loop.grown.data", "= load ptr"),
                ("int.list.loop.grown.capacity", "= load i64"),
            ] {
                assert!(
                    append
                        .lines()
                        .any(|line| line.contains(name) && line.contains(instruction)),
                    "missing `{name}` {instruction}: {append}"
                );
            }
            assert!(!append.contains("int.list.loop.grown.length"), "{append}");
            for slot in ["int.list.add.slot", "int.list.get.slot"] {
                assert!(
                    scan.lines()
                        .any(|line| { line.contains(slot) && line.contains("getelementptr i64") }),
                    "private Int list slot `{slot}` lost typed addressing: {scan}"
                );
            }
            assert!(!scan.contains("ptrtoint ptr"), "{scan}");
            assert!(!scan.contains("inttoptr i64"), "{scan}");
            for (name, instruction) in [
                ("int.list.add.length", "= load i64"),
                ("int.list.add.capacity", "= load i64"),
                ("int.list.add.data", "= load ptr"),
                ("int.list.add.index", "= load i64"),
            ] {
                assert!(
                    !append
                        .lines()
                        .any(|line| line.contains(name) && line.contains(instruction)),
                    "specialized loop retained `{name}` {instruction}: {append}"
                );
            }

            let observed = llvm_native_function(&llvm, "native_int_list_appendObservedLength");
            assert!(
                observed.contains("int.list.loop.length = phi i64"),
                "same-list length observation lost append-loop SSA: {observed}"
            );
            assert_eager_native_int_list_length_commits(observed);
            let fallback = llvm_native_function(&llvm, "native_int_list_twoAppends");
            assert!(
                !fallback.contains("int.list.loop.length = phi i64"),
                "multiple append statements must conservatively fall back: {fallback}"
            );
            assert!(
                fallback.lines().any(|line| line.contains("int.list.add.length")
                    && line.contains("= load i64")),
                "fallback must retain authoritative header reloads: {fallback}"
            );
            assert_eq!(
                fallback
                    .matches("call i32 @loom_int_list_reserve_v1")
                    .count(),
                2,
                "each generic append site needs its own guarded reserve: {fallback}"
            );
            assert!(!fallback.contains("@loom_runtime_list_add"), "{fallback}");

            let faultable = llvm_native_function(&llvm, "native_int_list_faultableAppend");
            assert!(
                faultable.contains("int.list.loop.length = phi i64"),
                "fallible element lost append-loop SSA: {faultable}"
            );
            assert_deferred_native_int_list_length_commits(faultable);
            assert_native_int_list_dropped_once_on_each_return(faultable);
            let element_call = faultable
                .find("native_int_list_checkedElement")
                .expect("fallible append evaluates its element call");
            let capacity_test = faultable
                .find("int.list.add.full = icmp")
                .expect("fallible append tests capacity");
            assert!(
                element_call < capacity_test,
                "element evaluation must precede capacity synchronization: {faultable}"
            );

            let early_return = llvm_native_function(&llvm, "native_int_list_earlyReturnAppend");
            assert_deferred_native_int_list_length_commits(early_return);
            assert_native_int_list_dropped_once_on_each_return(early_return);

            for fallback in [
                "native_int_list_interveningAppendKeepsCheck",
                "native_int_list_noneSideEffectFallsBack",
            ] {
                let body = llvm_native_function(&llvm, fallback);
                assert!(body.contains("int.list.get.past.end"), "{body}");
                assert!(body.contains("int.list.get.none"), "{body}");
            }
            let cleanup = llvm_native_function(&llvm, "native_int_list_cleanupOnFaultPath");
            assert!(cleanup.contains("@loom_int_list_drop_v1"), "{cleanup}");
            assert_native_int_list_dropped_once_on_each_return(cleanup);
        }
    }

    let output = Command::new(executable)
        .output()
        .expect("run native Int list executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, b"Unit\n");

    let fault_executable = project.path().join("fault-program");
    emit_native(program, &fault_executable, &EmitOptions::run("faultMain"))
        .expect("emit fallible native Int list executable");
    let output = Command::new(fault_executable)
        .output()
        .expect("run fallible native Int list executable");
    assert!(!output.status.success(), "{output:?}");
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostic.contains("AssertionFault"), "{output:?}");
    assert!(!diagnostic.contains("ListRuntimeFault"), "{output:?}");
}

#[test]
fn native_int_list_storage_falls_back_for_escaping_text_async_and_hazards() {
    let source = r#"module native_int_list_fallback

fn consume(values List[Int]) Int {
    values.length()
}

fn escaping() Int {
    var values = List[Int]()
    values.add(7)
    consume(values)
}

fn textList() Int {
    var values = List[Text]()
    values.add("seven")
    values.length()
}

fn receiverObservationHazard() Int {
    var values = List[Int]()
    values.add(1)
    match values.get(if values.length() == 1 { 0 } else { 0 }) {
        Some(value) => value
        None => -1
    }
}

async fn asynchronous() Int {
    var values = List[Int]()
    values.add(9)
    Task.sleep(1).await
    values.length()
}

pub async fn main() {
    let escaped = escaping()
    let text = textList()
    let observed = receiverObservationHazard()
    let waited = asynchronous().await
    assert escaped == 1
    assert text == 1
    assert observed == 1
    assert waited == 1
}
"#;
    let project = tempfile::tempdir().expect("create native Int list fallback project");
    std::fs::write(project.path().join("main.loom"), source)
        .expect("write native Int list fallback source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load native Int list fallback project")
        .snapshot()
        .expect("analyze native Int list fallback project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(
        snapshot.executable().expect("lower fallback MIR"),
        &executable,
        &options,
    )
    .expect("emit native Int list fallback executable");

    let llvm = std::fs::read_to_string(ir).expect("read fallback LLVM IR");
    for function in [
        "native_int_list_fallback_escaping",
        "native_int_list_fallback_textList",
        "native_int_list_fallback_receiverObservationHazard",
    ] {
        let body = llvm_native_function(&llvm, function);
        assert!(body.contains("@loom_runtime_list_add"), "{body}");
        assert!(!body.contains("@loom_int_list_reserve_v1"), "{body}");
    }
    let asynchronous = llvm_resume_function(&llvm, "native_int_list_fallback_asynchronous");
    assert!(
        asynchronous.contains("@loom_runtime_list_add"),
        "{asynchronous}"
    );
    assert!(
        !asynchronous.contains("@loom_int_list_reserve_v1"),
        "{asynchronous}"
    );

    let output = Command::new(executable)
        .output()
        .expect("run native Int list fallback executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn proven_construction_omits_validation_while_dynamic_input_keeps_it() {
    let source = r"module construction

import std.float.is_finite

type Money = Float where is_finite(self) && self >= 0.0

fn direct() Money
    requires true
    ensures result >= 0.0
{
    assert true
    Money(10.0)
}

fn checked(raw Float) Result[Money, ConstraintError] {
    Money(raw)
}

pub fn main() {
    let money = direct()
    assert money == 10.0
    match checked(-97531.125) {
        Err(first) => match checked(-86420.5) {
            Err(second) => {
                assert first == second
                Unit
            }
            Ok(_) => {
                assert false
                Unit
            }
        }
        Ok(_) => {
            assert false
            Unit
        }
    }
}
";
    let project = tempfile::tempdir().expect("create construction project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load construction project")
        .snapshot()
        .expect("analyze construction project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower construction MIR");
    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(program, &executable, &options).expect("emit construction executable");

    let ir = std::fs::read_to_string(ir).expect("read construction IR");
    let direct = llvm_function(&ir, "construction_direct");
    let checked = llvm_function(&ir, "construction_checked");
    assert!(!direct.contains("constraint.ok"), "{direct}");
    assert!(!direct.contains("constraint.error"), "{direct}");
    assert!(!direct.contains("PreconditionFault"), "{direct}");
    assert!(!direct.contains("PostconditionFault"), "{direct}");
    assert!(!direct.contains("assert.fail"), "{direct}");
    assert!(checked.contains("constraint.ok"), "{checked}");
    assert!(checked.contains("constraint.error"), "{checked}");
    let result_branch = checked
        .lines()
        .find(|line| {
            line.contains("label %constraint.ok") && line.contains("label %constraint.error")
        })
        .unwrap_or_else(|| panic!("missing checked Result branch: {checked}"));
    assert!(!result_branch.contains("!prof"), "{result_branch}");

    let output = Command::new(executable)
        .output()
        .expect("run construction executable");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
#[allow(clippy::too_many_lines)]
fn requires_faults_preserve_exact_caller_spans_across_llvm_abis() {
    let source = r#"module requires_caller_blame

fn legacy(value Int) Text
    requires value > 0
{
    "accepted"
}

fn native(value Int) Int
    requires value > 0
{
    value
}

async fn asynchronous(value Int) Int
    requires value > 0
{
    value
}

test fn a_legacy_first() {
    discard legacy(0)
}

test fn b_legacy_second() {
    discard legacy(0)
}

test fn c_native_first() {
    discard native(0)
}

test fn d_native_second() {
    discard native(0)
}

test async fn e_async_first() {
    discard asynchronous(0).await
}

test async fn f_async_second() {
    discard asynchronous(0).await
}

test fn g_root_boundary()
    requires false
{
}
"#;
    let project = tempfile::tempdir().expect("create requires blame project");
    std::fs::write(project.path().join("main.loom"), source).expect("write requires blame source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load requires blame project")
        .snapshot()
        .expect("analyze requires blame project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower requires blame MIR");

    let mut interpreter = Interpreter::new(program);
    let expected_faults = interpreter
        .run_tests()
        .into_iter()
        .map(|result| {
            serde_json::to_value(result.failure.expect("requires test must fail"))
                .expect("serialize interpreted contract fault")
        })
        .collect::<Vec<_>>();
    assert_eq!(expected_faults.len(), 7);

    for pair in [0..2, 2..4, 4..6] {
        assert_eq!(
            expected_faults[pair.start]["fault"]["contractSpan"],
            expected_faults[pair.start + 1]["fault"]["contractSpan"]
        );
        assert_ne!(
            expected_faults[pair.start]["fault"]["blameSpan"],
            expected_faults[pair.start + 1]["fault"]["blameSpan"]
        );
    }

    let executable = project.path().join("tests");
    let ir = project.path().join("tests.ll");
    let mut options = EmitOptions::tests();
    options.emit_ir = Some(ir.clone());
    emit_native(program, &executable, &options).expect("emit requires blame tests");
    let llvm = std::fs::read_to_string(ir).expect("read requires blame LLVM IR");
    assert!(
        llvm.contains("@loom_context_raise_fault_with_span_v1"),
        "{llvm}"
    );

    let output = Command::new(executable)
        .env("LOOM_FAULT_FORMAT", "json")
        .output()
        .expect("run requires blame tests");
    assert!(!output.status.success(), "{output:?}");
    let compiled = String::from_utf8(output.stderr)
        .expect("requires fault stderr is UTF-8")
        .lines()
        .filter_map(|line| line.strip_prefix("LOOM_FAULT_JSON_V1:"))
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("parse native fault"))
        .collect::<Vec<_>>();
    assert_eq!(compiled, expected_faults);
    let root_span = program
        .functions
        .iter()
        .find(|function| function.name.ends_with("g_root_boundary"))
        .expect("find root boundary function")
        .span;
    assert_eq!(
        compiled[6]["fault"]["blameSpan"],
        serde_json::to_value(root_span).expect("serialize root boundary span"),
        "the test harness boundary must use the root function span"
    );
}

fn llvm_function<'source>(ir: &'source str, symbol_suffix: &str) -> &'source str {
    let marker = "define internal i32 @loom.fn.";
    let start = ir
        .match_indices(&marker)
        .map(|(index, _)| index)
        .find(|index| {
            ir[*index..]
                .lines()
                .next()
                .is_some_and(|line| line.contains(symbol_suffix))
        })
        .unwrap_or_else(|| panic!("missing LLVM function containing `{symbol_suffix}`"));
    let rest = &ir[start + marker.len()..];
    let end = rest.find("\ndefine ").unwrap_or(rest.len());
    &ir[start..start + marker.len() + end]
}

fn emit_source_with_ir(source: &str) -> (tempfile::TempDir, CheckedProgram, String) {
    let project = tempfile::tempdir().expect("create source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load source project")
        .snapshot()
        .expect("analyze source project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower executable MIR").clone();
    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(&program, &executable, &options).expect("emit native executable");
    let llvm = std::fs::read_to_string(ir).expect("read native LLVM IR");
    (project, program, llvm)
}

fn assert_emitted_main_succeeds(project: &tempfile::TempDir) {
    let output = Command::new(project.path().join("program"))
        .output()
        .expect("run native executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, b"Unit\n");
}

fn llvm_any_function<'source>(ir: &'source str, symbol_suffix: &str) -> Option<&'source str> {
    let start = ir
        .match_indices("define ")
        .map(|(index, _)| index)
        .find(|index| {
            ir[*index..]
                .lines()
                .next()
                .is_some_and(|line| line.contains(symbol_suffix))
        })?;
    let rest = &ir[start + "define ".len()..];
    let end = rest.find("\ndefine ").unwrap_or(rest.len());
    Some(&ir[start..start + "define ".len() + end])
}

fn llvm_native_function<'source>(ir: &'source str, symbol_suffix: &str) -> &'source str {
    let marker = "define internal ";
    let start = ir
        .match_indices(marker)
        .map(|(index, _)| index)
        .find(|index| {
            ir[*index..].lines().next().is_some_and(|line| {
                line.contains("@loom.native.fn.") && line.contains(symbol_suffix)
            })
        })
        .unwrap_or_else(|| panic!("missing native LLVM function containing `{symbol_suffix}`"));
    let rest = &ir[start + marker.len()..];
    let end = rest.find("\ndefine ").unwrap_or(rest.len());
    &ir[start..start + marker.len() + end]
}

fn llvm_defined_symbol(function: &str) -> &str {
    function
        .lines()
        .next()
        .and_then(|definition| definition.split_once('@'))
        .and_then(|(_, symbol_and_parameters)| symbol_and_parameters.split_once('('))
        .map(|(symbol, _)| symbol)
        .expect("LLVM function definition must contain a symbol")
}

fn llvm_assumed_native_function<'source>(ir: &'source str, symbol_suffix: &str) -> &'source str {
    let marker = "define internal ";
    let start = ir
        .match_indices(marker)
        .map(|(index, _)| index)
        .find(|index| {
            ir[*index..].lines().next().is_some_and(|line| {
                line.contains("@loom.native.assumed.fn.") && line.contains(symbol_suffix)
            })
        })
        .unwrap_or_else(|| {
            panic!("missing assumed native LLVM function containing `{symbol_suffix}`")
        });
    let rest = &ir[start + marker.len()..];
    let end = rest.find("\ndefine ").unwrap_or(rest.len());
    &ir[start..start + marker.len() + end]
}

fn assert_native_int_list_dropped_once_on_each_return(function: &str) {
    let blocks = llvm_basic_blocks(function);
    let returns = blocks
        .iter()
        .filter(|(_, block)| {
            block
                .lines()
                .any(|line| line.trim_start().starts_with("ret "))
        })
        .collect::<Vec<_>>();
    assert!(!returns.is_empty(), "function has no return: {function}");
    for (label, block) in returns {
        let cleanup = if block.contains("@loom_int_list_drop_v1") {
            *block
        } else {
            let predecessors = llvm_block_predecessors(block);
            assert_eq!(
                predecessors.len(),
                1,
                "return `{label}` must have one cleanup predecessor: {block}"
            );
            blocks
                .get(predecessors[0])
                .unwrap_or_else(|| panic!("missing predecessor `{}`: {function}", predecessors[0]))
        };
        assert_eq!(
            cleanup.matches("@loom_int_list_drop_v1").count(),
            1,
            "return `{label}` must run exactly one List drop: {cleanup}"
        );
        let expected_root_pop = usize::from(function.contains("@loom_gc_root_push_v1"));
        assert_eq!(
            cleanup.matches("@loom_gc_root_pop_v1").count(),
            expected_root_pop,
            "return `{label}` has an unexpected root-pop shape: {cleanup}"
        );
    }
}

fn assert_deferred_native_int_list_length_commits(function: &str) {
    let blocks = llvm_basic_blocks(function);
    let grow = blocks
        .iter()
        .find_map(|(label, block)| {
            (label.starts_with("int.list.add.grow.")
                && block.contains("call i32 @loom_int_list_reserve_v1")
                && block.contains("int.list.loop.length"))
            .then_some(*block)
        })
        .unwrap_or_else(|| panic!("native append has no growth block: {function}"));
    let length_sync = grow
        .find("store i64 %int.list.loop.length")
        .unwrap_or_else(|| panic!("growth did not publish the SSA length: {grow}"));
    let reserve = grow
        .find("call i32 @loom_int_list_reserve_v1")
        .expect("growth block calls reserve");
    assert!(
        length_sync < reserve,
        "SSA length must be published before reserve: {grow}"
    );

    let exit = blocks
        .iter()
        .find_map(|(label, block)| label.starts_with("range.exit.").then_some(*block))
        .unwrap_or_else(|| panic!("native append has no range exit: {function}"));
    assert!(
        exit.contains("store i64 %int.list.loop.length"),
        "normal exit did not publish the final SSA length: {exit}"
    );
    let ready = blocks
        .iter()
        .find_map(|(label, block)| {
            (label.starts_with("int.list.add.ready.") && block.contains("int.list.loop.length"))
                .then_some(*block)
        })
        .unwrap_or_else(|| panic!("native append has no ready block: {function}"));
    assert!(
        !ready.contains("store i64 %int.list.add.next.length"),
        "non-observing append retained a per-iteration length commit: {ready}"
    );
}

fn assert_eager_native_int_list_length_commits(function: &str) {
    let blocks = llvm_basic_blocks(function);
    let ready = blocks
        .iter()
        .find_map(|(label, block)| {
            (label.starts_with("int.list.add.ready.") && block.contains("int.list.loop.length"))
                .then_some(*block)
        })
        .unwrap_or_else(|| panic!("receiver-observing append has no ready block: {function}"));
    let slot_store = ready
        .find("ptr %int.list.add.slot")
        .unwrap_or_else(|| panic!("receiver-observing append did not store its slot: {ready}"));
    let length_commit = ready
        .find("store i64 %int.list.add.next.length")
        .unwrap_or_else(|| {
            panic!("receiver-observing append lost its eager length commit: {ready}")
        });
    assert!(
        slot_store < length_commit,
        "receiver-observing append committed length before its slot: {ready}"
    );

    let grow = blocks
        .iter()
        .find_map(|(label, block)| {
            (label.starts_with("int.list.add.grow.")
                && block.contains("call i32 @loom_int_list_reserve_v1")
                && block.contains("int.list.loop.length"))
            .then_some(*block)
        })
        .unwrap_or_else(|| panic!("receiver-observing append has no growth block: {function}"));
    assert!(
        !grow.contains("store i64 %int.list.loop.length"),
        "eager growth retained a redundant deferred sync: {grow}"
    );
    let exit = blocks
        .iter()
        .find_map(|(label, block)| label.starts_with("range.exit.").then_some(*block))
        .unwrap_or_else(|| panic!("receiver-observing append has no exit block: {function}"));
    assert!(
        !exit.contains("store i64 %int.list.loop.length"),
        "eager exit retained a redundant deferred sync: {exit}"
    );
}

fn assert_balanced_gc_root_frame(function: &str) {
    assert_eq!(
        function.matches("call i32 @loom_gc_root_push_v1").count(),
        1,
        "allocating function must push one root frame: {function}"
    );
    let returns = function
        .lines()
        .filter(|line| line.trim_start().starts_with("ret "))
        .count();
    assert!(returns > 0, "function has no return: {function}");
    assert_eq!(
        function.matches("call i32 @loom_gc_root_pop_v1").count(),
        returns,
        "every return must pop the root frame: {function}"
    );
}

fn gc_root_slot_count(function: &str) -> usize {
    let allocation = function
        .lines()
        .find(|line| line.contains("gc.root.slots") && line.contains("alloca"))
        .unwrap_or_else(|| panic!("function has no GC root slots: {function}"))
        .split_once("alloca ")
        .map_or_else(
            || panic!("malformed GC root slot allocation: {function}"),
            |(_, allocation)| allocation,
        );
    if let Some(array) = allocation.strip_prefix('[') {
        return array
            .split_once(" x ptr]")
            .and_then(|(count, _)| count.parse().ok())
            .unwrap_or_else(|| panic!("malformed GC root slot array: {allocation}"));
    }
    let fields = allocation
        .strip_prefix("{ ")
        .and_then(|fields| fields.split_once(" }").map(|(fields, _)| fields))
        .unwrap_or_else(|| panic!("malformed GC root slot structure: {allocation}"));
    fields
        .split(',')
        .filter(|field| field.trim() == "ptr")
        .count()
}

#[derive(Debug)]
struct GcRootDescriptorIr {
    slot_count: usize,
    state_count: usize,
    bitmap_words: usize,
    bitmaps: Vec<u64>,
}

impl GcRootDescriptorIr {
    fn is_live(&self, state: usize, slot: usize) -> bool {
        assert!(state < self.state_count, "{self:?}");
        assert!(slot < self.slot_count, "{self:?}");
        let word = state * self.bitmap_words + slot / 64;
        self.bitmaps[word] & (1_u64 << (slot % 64)) != 0
    }
}

fn gc_root_descriptor(llvm: &str, function: &str) -> GcRootDescriptorIr {
    let descriptor_name = function
        .lines()
        .find(|line| line.contains("store ptr @loom.gc.root.descriptor."))
        .and_then(|line| {
            line.split_whitespace()
                .find(|token| token.starts_with("@loom.gc.root.descriptor."))
        })
        .unwrap_or_else(|| panic!("function has no GC root descriptor: {function}"))
        .trim_end_matches(',');
    let descriptor = llvm
        .lines()
        .find(|line| line.starts_with(&format!("{descriptor_name} =")))
        .unwrap_or_else(|| panic!("missing descriptor `{descriptor_name}`: {llvm}"));
    let fields = descriptor
        .split_once("%loom.GcRootDescriptor { ")
        .and_then(|(_, fields)| fields.split_once(" }").map(|(fields, _)| fields))
        .unwrap_or_else(|| panic!("malformed GC root descriptor: {descriptor}"));
    let fields = fields.split(',').map(str::trim).collect::<Vec<_>>();
    assert_eq!(fields.len(), 6, "malformed descriptor: {descriptor}");
    let slot_count = parse_llvm_usize_field(fields[2], "i64", descriptor);
    let state_count = parse_llvm_usize_field(fields[3], "i64", descriptor);
    let bitmap_words = parse_llvm_usize_field(fields[4], "i64", descriptor);
    let bitmap_name = fields[5]
        .strip_prefix("ptr ")
        .unwrap_or_else(|| panic!("descriptor has no bitmap pointer: {descriptor}"));
    let bitmap = llvm
        .lines()
        .find(|line| line.starts_with(&format!("{bitmap_name} =")))
        .unwrap_or_else(|| panic!("missing bitmap `{bitmap_name}`: {llvm}"));
    let bitmaps = bitmap
        .split("i64 ")
        .skip(1)
        .map(|value| {
            let value = value.split([',', ']']).next().unwrap_or_default().trim();
            parse_llvm_u64(value, bitmap)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        bitmaps.len(),
        state_count * bitmap_words,
        "malformed bitmap: {bitmap}"
    );
    GcRootDescriptorIr {
        slot_count,
        state_count,
        bitmap_words,
        bitmaps,
    }
}

fn parse_llvm_usize_field(field: &str, ty: &str, context: &str) -> usize {
    field
        .strip_prefix(&format!("{ty} "))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("malformed `{field}` in {context}"))
}

fn parse_llvm_u64(value: &str, context: &str) -> u64 {
    value.parse().unwrap_or_else(|_| {
        let signed = value
            .parse::<i64>()
            .unwrap_or_else(|_| panic!("malformed integer `{value}` in {context}"));
        u64::from_ne_bytes(signed.to_ne_bytes())
    })
}

fn gc_state_before_call(function: &str, needle: &str, occurrence: usize) -> usize {
    let lines = function.lines().collect::<Vec<_>>();
    let (call, _) = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("call ") && line.contains(needle))
        .nth(occurrence)
        .unwrap_or_else(|| panic!("missing call #{occurrence} `{needle}`: {function}"));
    for line in lines[..call].iter().rev() {
        if !line.starts_with(char::is_whitespace) && line.contains(':') {
            break;
        }
        let line = line.trim();
        if let Some(value) = line
            .strip_prefix("store i64 ")
            .and_then(|line| line.split_once(", ptr %gc.root.state"))
            .map(|(value, _)| value)
        {
            return parse_llvm_usize_field(&format!("i64 {value}"), "i64", function);
        }
    }
    panic!("call `{needle}` has no preceding GC state publication: {function}");
}

fn assert_gc_state_published_before(function: &str, needle: &str) {
    let calls = function
        .lines()
        .filter(|line| line.contains("call ") && line.contains(needle))
        .count();
    assert!(calls > 0, "missing call `{needle}`: {function}");
    for occurrence in 0..calls {
        let _ = gc_state_before_call(function, needle, occurrence);
    }
}

fn gc_root_slot_index(function: &str, pointer: &str) -> usize {
    find_gc_root_slot_index(function, pointer)
        .unwrap_or_else(|| panic!("root slot for `{pointer}` is missing: {function}"))
}

fn gc_root_slot_is_registered(function: &str, pointer: &str) -> bool {
    find_gc_root_slot_index(function, pointer).is_some()
}

fn find_gc_root_slot_index(function: &str, pointer: &str) -> Option<usize> {
    for store in function
        .lines()
        .filter(|line| line.contains("store ptr %") && line.contains(pointer))
    {
        let destination = store
            .split_once(", ptr ")
            .and_then(|(_, destination)| destination.split(',').next())
            .map(str::trim)
            .unwrap_or_default();
        let Some(field) = function.lines().find(|line| {
            line.trim_start().starts_with(&format!("{destination} ="))
                && line.contains("gc.root.slots")
        }) else {
            continue;
        };
        return Some(
            field
                .rsplit_once("i32 ")
                .and_then(|(_, index)| index.split(',').next())
                .and_then(|index| index.trim().parse().ok())
                .unwrap_or_else(|| panic!("malformed root slot field: {field}")),
        );
    }
    None
}

fn llvm_basic_blocks(function: &str) -> BTreeMap<&str, &str> {
    function
        .split("\n\n")
        .filter_map(|block| {
            let label = block
                .lines()
                .find(|line| !line.starts_with(char::is_whitespace) && line.contains(':'))?
                .split_once(':')?
                .0;
            Some((label, block))
        })
        .collect()
}

fn llvm_block_predecessors(block: &str) -> Vec<&str> {
    let Some((_, predecessors)) = block
        .lines()
        .next()
        .and_then(|line| line.split_once("; preds = "))
    else {
        return Vec::new();
    };
    predecessors
        .split(',')
        .map(|predecessor| predecessor.trim().trim_start_matches('%'))
        .collect()
}

fn llvm_declaration_attributes<'source>(ir: &'source str, symbol: &str) -> &'source str {
    let declaration = ir
        .lines()
        .find(|line| line.starts_with("declare ") && line.contains(&format!("@{symbol}")))
        .unwrap_or_else(|| panic!("missing LLVM declaration `{symbol}`"));
    let (_, group) = declaration
        .rsplit_once(" #")
        .unwrap_or_else(|| panic!("LLVM declaration has no attribute group: {declaration}"));
    let marker = format!("attributes #{group} =");
    ir.lines()
        .find(|line| line.starts_with(&marker))
        .unwrap_or_else(|| panic!("missing LLVM attribute group `{marker}`"))
}

fn llvm_resume_function<'source>(ir: &'source str, symbol_suffix: &str) -> &'source str {
    let marker = "define internal i32 @loom.resume.";
    let start = ir
        .match_indices(marker)
        .map(|(index, _)| index)
        .find(|index| {
            ir[*index..]
                .lines()
                .next()
                .is_some_and(|line| line.contains(symbol_suffix))
        })
        .unwrap_or_else(|| panic!("missing LLVM resume function containing `{symbol_suffix}`"));
    let rest = &ir[start + marker.len()..];
    let end = rest.find("\ndefine ").unwrap_or(rest.len());
    &ir[start..start + marker.len() + end]
}

fn unit_program() -> CheckedProgram {
    let mut program = Program::default();
    program.functions.push(Function {
        id: FunctionId(0),
        name: "sample.main".into(),
        span: Default::default(),
        type_parameters: 0,
        is_async: false,
        suspension_points: Vec::new(),
        params: Vec::new(),
        witness_params: Vec::new(),
        witness_prefix_count: 0,
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr::new(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
                Default::default(),
            ))),
            span: Default::default(),
        },
        call_plan: CallPlan::default(),
    });
    program.exports = BTreeMap::from([("main".into(), FunctionId(0))]);
    program
        .renumber_expr_ids()
        .expect("renumber unit-program expressions");
    program
        .into_checked()
        .expect("valid checked unit-program fixture")
}

fn unit_test_program() -> CheckedProgram {
    let mut program = unit_program().into_program();
    program.tests = vec![FunctionId(0)];
    program
        .into_checked()
        .expect("valid checked unit-test-program fixture")
}

#[test]
fn core_examples_compile_and_run_as_native_programs() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    for fixture in [
        "constraints-contracts",
        "concepts-polymorphism",
        "async-resources",
    ] {
        let source = workspace.join("examples").join(fixture);
        let snapshot = AnalysisHost::new(&source)
            .expect("load project")
            .snapshot()
            .expect("analyze project");
        assert!(
            !snapshot.has_errors(),
            "{fixture}: {:?}",
            snapshot.diagnostics()
        );
        let program = snapshot.executable().expect("lower executable MIR");
        let directory = tempfile::tempdir().expect("create temp directory");
        let executable = directory.path().join("program");
        let mut options = EmitOptions::run("main");
        if fixture == "async-resources" {
            options.emit_ir = Some(directory.path().join("program.ll"));
        }
        emit_native(program, &executable, &options)
            .unwrap_or_else(|error| panic!("{fixture}: {error}"));
        let output = Command::new(&executable).output().expect("run executable");
        assert!(
            output.status.success(),
            "{fixture}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(output.stdout, b"Unit\n");
        if let Some(ir) = &options.emit_ir {
            let ir = std::fs::read_to_string(ir).expect("read async LLVM IR");
            assert!(ir.contains("@loom.resume."), "{ir}");
            assert!(ir.contains("@loom_executor_run"), "{ir}");
            assert!(ir.contains("@loom_task_suspend_value"), "{ir}");
            assert!(ir.contains("@loom_task_from_wait_source"), "{ir}");
            assert!(ir.contains("@loom_join_create"), "{ir}");
            assert!(ir.contains("@loom_wait_now_ns"), "{ir}");
            assert!(ir.contains("state.resume."), "{ir}");
            assert!(ir.contains("call ptr @loom_runtime_create_v1"), "{ir}");
            assert!(ir.contains("call i32 @loom_runtime_activate_v1"), "{ir}");
            assert!(
                ir.contains("call ptr @loom_executor_create_for_runtime_v1"),
                "{ir}"
            );
            assert!(ir.contains("call void @loom_executor_destroy"), "{ir}");
            assert!(ir.contains("call i32 @loom_runtime_deactivate_v1"), "{ir}");
            assert!(ir.contains("call i32 @loom_runtime_destroy_v1"), "{ir}");
            assert!(ir.contains("@loom_context_raise_fault_v1"), "{ir}");
            assert!(!ir.contains("@loom_executor_create("), "{ir}");
            assert!(!ir.contains("@loom_gc_activate_executor"), "{ir}");
            assert!(!ir.contains("@loom_gc_deactivate_executor"), "{ir}");
            assert!(!ir.contains("@loom_executor_raise_fault"), "{ir}");
            assert!(ir.contains("executor.root.failed"), "{ir}");
            assert!(ir.contains("executor.root.failure.deactivate"), "{ir}");
            assert!(ir.contains("executor.root.failure.destroy.runtime"), "{ir}");

            let executor_destroy = ir
                .rfind("call void @loom_executor_destroy")
                .expect("async root destroys its executor");
            let teardown = &ir[executor_destroy..];
            let runtime_deactivate = teardown
                .find("call i32 @loom_runtime_deactivate_v1")
                .expect("async root deactivates its runtime");
            let runtime_destroy = teardown
                .find("call i32 @loom_runtime_destroy_v1")
                .expect("async root destroys its runtime");
            assert!(
                runtime_deactivate < runtime_destroy,
                "async root teardown order is invalid: {ir}"
            );
        }

        let tests = directory.path().join("tests");
        emit_native(program, &tests, &EmitOptions::tests())
            .unwrap_or_else(|error| panic!("{fixture} tests: {error}"));
        let output = Command::new(&tests).output().expect("run native tests");
        assert!(
            output.status.success(),
            "{fixture} tests: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.lines().all(|line| line.starts_with("passed ")),
            "{stdout}"
        );
    }
}

#[test]
fn stored_tasks_dynamic_lists_and_join_modes_run_natively() {
    let source = r"module joins

async fn one() Int {
    Task.sleep(2).await
    1
}

async fn two() Int {
    // An immediately ready sibling makes Task.any deterministic without using
    // wall-clock timing as an ordering oracle.
    2
}

pub async fn main() {
    Task.sleep(1).await
    let first = one()
    let second = two()
    let values = Task.all([first, second]).await

    let combined = Task.all(one(), two())
    let left, right = combined.await
    assert left == 1
    assert right == 2

    let winner = Task.any([one(), two()]).await
    assert winner == 2

    let settled = Task.settled([one(), two()])
    let outcomes = settled.await

    let raced = Task.race([one(), two()])
    let outcome = raced.await
}
";
    let project = tempfile::tempdir().expect("create join project");
    std::fs::write(project.path().join("main.loom"), source).expect("write join source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load join project")
        .snapshot()
        .expect("analyze join project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower join MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit native join executable");
    let output = Command::new(executable).output().expect("run native joins");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
fn task_outcomes_match_and_expose_fault_details_natively() {
    let source = r#"module outcomes

async fn completed() Int {
    7
}

async fn faulted() Int {
    assert false
    0
}

pub async fn main() {
    let success, failure = Task.settled(completed(), faulted()).await
    match success {
        Completed(value) => {
            assert value == 7
            Unit
        }
        Faulted(_) => {
            assert false
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
    match failure {
        Completed(_) => {
            assert false
            Unit
        }
        Faulted(fault) => {
            let code = fault.code()
            let message = fault.message()
            assert code == "AssertionFault"
            assert message == "assertion was not satisfied"
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
}
"#;
    let project = tempfile::tempdir().expect("create outcome project");
    std::fs::write(project.path().join("main.loom"), source).expect("write outcome source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load outcome project")
        .snapshot()
        .expect("analyze outcome project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower outcome MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit native outcome executable");
    let output = Command::new(executable)
        .output()
        .expect("run native outcomes");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
fn duration_file_and_socket_tasks_run_natively() {
    use std::io::{Read, Write};

    let project = tempfile::tempdir().expect("create I/O project");
    let file = project.path().join("round-trip.txt");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept test client");
        let mut request = [0_u8; 4];
        socket.read_exact(&mut request).expect("read request");
        assert_eq!(&request, b"ping");
        socket.write_all(b"pong").expect("write response");
    });
    let source = format!(
        r#"module standard_io

import std.time.milliseconds
import std.file.open_read
import std.file.create
import std.net.connect

pub async fn main() {{
    let delay = milliseconds(1)
    let observed = delay.as_milliseconds()
    assert observed == 1
    Task.sleep(delay).await
    {{
        scoped output = create("{}").await
        output.write_text("hello from loom").await
        Unit
    }}
    {{
        scoped input = open_read("{}").await
        let content = input.read_text().await
        assert content == "hello from loom"
        Unit
    }}
    {{
        scoped socket = connect("127.0.0.1", {}).await
        socket.write_text("ping").await
        let response = socket.read_text().await
        assert response == "pong"
        Unit
    }}
}}
"#,
        file.display(),
        file.display(),
        port,
    );
    std::fs::write(project.path().join("main.loom"), source).expect("write I/O source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load I/O project")
        .snapshot()
        .expect("analyze I/O project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower I/O MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit native I/O executable");
    let output = Command::new(executable)
        .output()
        .expect("run native I/O program");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, b"Unit\n");
    server.join().expect("join test server");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "hello from loom");
}

#[test]
fn cancellation_resumes_the_suspended_state_and_runs_cleanup() {
    let source = r"module cancellation

async fn slow() Int {
    defer {
        assert false
    }
    Task.sleep(100).await
    1
}

async fn fast() Int {
    2
}

pub async fn main() {
    let winner = Task.any(slow(), fast()).await
    assert winner == 2
}
";
    let project = tempfile::tempdir().expect("create cancellation project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load cancellation project")
        .snapshot()
        .expect("analyze cancellation project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower cancellation MIR");
    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(program, &executable, &options).expect("emit cancellation executable");
    let llvm = std::fs::read_to_string(ir).expect("read cancellation IR");
    assert!(llvm.contains("state.cancelled"), "{llvm}");
    assert!(llvm.contains("AssertionFault"), "{llvm}");
    let output = Command::new(executable)
        .output()
        .expect("run cancellation executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(!stdout.contains("AssertionFault\n"), "{stdout}");
    assert!(stdout.ends_with("Unit\n"), "{stdout}");
}

#[test]
fn nested_control_await_resumes_in_the_selected_branch() {
    let source = r"module nested

async fn child(value Int) Int {
    Task.sleep(1).await
    value
}

async fn choose(flag Bool) Int {
    if flag {
        child(7).await
    } else {
        child(9).await
    }
}

pub async fn main() {
    let selected = choose(true).await
    assert selected == 7
}
";
    let project = tempfile::tempdir().expect("create nested await project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load nested await project")
        .snapshot()
        .expect("analyze nested await project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower nested await MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit nested await executable");
    let output = Command::new(executable)
        .output()
        .expect("run nested await executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.stdout, b"Unit\n");
}

#[test]
fn compact_witness_ir_removes_linked_nodes_concat_and_legacy_task_clone() {
    let source = r"module compact_witness

concept Check {
    method check(self) Bool
}

record Number { value Int }

impl Check for Number {
    method check(self) Bool { self.value == 7 }
}

async fn delayedCheck[T: Check](value T) Bool {
    Task.sleep(1).await
    value.check()
}

pub async fn main() {
    let checked = delayedCheck(Number { value = 7 }).await
    assert checked
}
";
    let (project, _program, llvm) = emit_source_with_ir(source);

    assert!(llvm.contains("%loom.WitnessDescriptor = type"), "{llvm}");
    assert!(llvm.contains("%loom.WitnessInstance = type"), "{llvm}");
    assert!(llvm.contains("@loom_task_capture_witnesses_v1"), "{llvm}");
    assert!(llvm.contains("@loom_task_witness_v1"), "{llvm}");
    assert!(llvm.contains("@loom_gc_clone_value_v1"), "{llvm}");
    assert!(llvm.contains("@loom_gc_build_value_nodes_v1"), "{llvm}");
    for legacy in [
        "%loom.WitnessNode",
        "@loom.runtime.concat_witnesses",
        "@loom.runtime.clone",
        "@loom.runtime.clone_nodes",
        "@loom_gc_alloc_value_node",
        "@loom_gc_alloc_witness_node",
        "@loom_task_clone_witness",
    ] {
        assert!(
            !llvm.contains(legacy),
            "legacy witness ABI `{legacy}`:\n{llvm}"
        );
    }

    assert_emitted_main_succeeds(&project);
}

#[test]
fn static_generic_concepts_and_conditional_witnesses_compile_natively() {
    let source = r"module sample

concept Equivalent {
    method equivalent(self, other Self) Bool
}

record Atom { value Int }

impl Equivalent for Atom {
    method equivalent(self, other Atom) Bool { self.value == other.value }
}

record Boxed[T] { value T }

impl[T: Equivalent] Equivalent for Boxed[T] {
    method equivalent(self, other Boxed[T]) Bool {
        self.value.equivalent(other.value)
    }
}

fn same[T: Equivalent](left T, right T) Bool {
    left.equivalent(right)
}

pub fn main() {
    let left = Boxed { value = Atom { value = 7 } }
    let right = Boxed { value = Atom { value = 7 } }
    let equal = same(left, right)
    assert equal
}

test fn conditional_witness() {
    let left = Boxed { value = Atom { value = 7 } }
    let right = Boxed { value = Atom { value = 7 } }
    let equal = same(left, right)
    assert equal
}
";
    let project = tempfile::tempdir().expect("create source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load project")
        .snapshot()
        .expect("analyze project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower executable MIR");

    for (name, options) in [
        ("program", EmitOptions::run("main")),
        ("tests", EmitOptions::tests()),
    ] {
        let executable = project.path().join(name);
        emit_native(program, &executable, &options).expect("emit native executable");
        let output = Command::new(&executable)
            .output()
            .expect("run native executable");
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn conformance_and_method_proofs_keep_their_native_parameter_order() {
    let source = r#"module sample

concept Check {
    method check(self) Bool
}

concept Combine {
    method both[U: Check](self, other U) Bool
}

record Left { value Int }
record Right { value Text }
record Holder[T] { value T }

impl Check for Left {
    method check(self) Bool { self.value == 1 }
}

impl Check for Right {
    method check(self) Bool { self.value == "ok" }
}

impl[T: Check] Combine for Holder[T] {
    method both[U: Check](self, other U) Bool {
        self.value.check() && other.check()
    }
}

pub fn main() {
    let holder = Holder { value = Left { value = 1 } }
    let other = Right { value = "ok" }
    let combined = holder.both(other)
    assert combined
}
"#;
    let project = tempfile::tempdir().expect("create source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load project")
        .snapshot()
        .expect("analyze project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower executable MIR");
    let both_source = program
        .functions
        .iter()
        .find(|function| function.name.rsplit('.').next() == Some("both"))
        .expect("lowered Combine.both method");
    assert_eq!(both_source.witness_prefix_count, 1);
    assert_eq!(both_source.witness_params.len(), 2);
    let executable = project.path().join("program");
    let ir = project.path().join("program.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir.clone());
    emit_native(program, &executable, &options).expect("emit native executable");
    let llvm = std::fs::read_to_string(ir).expect("read proof ABI LLVM IR");
    let both = llvm_function(&llvm, "both");
    let signature = both.lines().next().expect("Combine.both definition");
    let parameters = signature
        .split_once('(')
        .and_then(|(_, tail)| tail.split_once(')'))
        .map(|(parameters, _)| parameters)
        .expect("Combine.both parameter list");
    let parameters = parameters.split(',').map(str::trim).collect::<Vec<_>>();
    assert_eq!(parameters.len(), 8, "{signature}");
    assert!(
        parameters[..5]
            .iter()
            .all(|parameter| parameter.starts_with("ptr %")),
        "{signature}"
    );
    assert!(
        parameters[5..]
            .iter()
            .all(|parameter| parameter.starts_with("i64 %")),
        "{signature}"
    );
    let proof_loads = both
        .lines()
        .filter(|line| line.contains("witness.value") && line.contains("getelementptr"))
        .collect::<Vec<_>>();
    assert!(
        proof_loads.iter().any(|line| line.contains("ptr %2")),
        "missing conformance proof load: {both}"
    );
    assert!(
        proof_loads.iter().any(|line| line.contains("ptr %3")),
        "missing requirement proof load: {both}"
    );
    let output = Command::new(&executable)
        .output()
        .expect("run native executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn witness_method_tables_are_dense_per_concept_and_live_requirement() {
    let source = r#"module compact_table

dyn concept Noise {
    method noiseOne(self) Text
    method noiseTwo(self) Text
    method noiseThree(self) Text
}

record NoiseValue {}

impl Noise for NoiseValue {
    method noiseOne(self) Text { "one" }
    method noiseTwo(self) Text { "two" }
    method noiseThree(self) Text { "three" }
}

dyn concept Visible {
    method visibleText(self) Text
    method unusedText(self) Text
}

record Message { text Text }

impl Visible for Message {
    method visibleText(self) Text { self.text }
    method unusedText(self) Text { "unused" }
}

fn eraseMessage(value Message) dyn Visible { value }

pub fn main() {
    let erased = eraseMessage(Message { text = "live" })
    let text = erased.visibleText()
    assert text == "live"
}
"#;
    let (project, program, llvm) = emit_source_with_ir(source);
    let noise = program
        .concepts
        .iter()
        .find(|concept| concept.name == "Noise")
        .expect("Noise concept");
    let visible = program
        .concepts
        .iter()
        .find(|concept| concept.name == "Visible")
        .expect("Visible concept");
    let visible_requirement = program
        .requirements
        .iter()
        .find(|requirement| requirement.name == "visibleText")
        .expect("visibleText requirement");
    assert!(visible_requirement.id.0 >= 3);

    let table_definitions = llvm
        .lines()
        .filter(|line| line.starts_with("%loom.WitnessMethods."))
        .collect::<Vec<_>>();
    assert_eq!(
        table_definitions,
        vec![format!(
            "%loom.WitnessMethods.{} = type {{ ptr }}",
            visible.id.0
        )],
        "{llvm}"
    );
    assert!(
        !llvm.contains(&format!("%loom.WitnessMethods.{} = type", noise.id.0)),
        "{llvm}"
    );
    let method_tables = llvm
        .lines()
        .filter(|line| line.starts_with("@loom.witness.methods."))
        .collect::<Vec<_>>();
    assert_eq!(method_tables.len(), 1, "{method_tables:#?}");
    assert_eq!(
        method_tables[0].matches("ptr @").count(),
        1,
        "{method_tables:#?}"
    );
    let descriptors = llvm
        .lines()
        .filter(|line| line.starts_with("@loom.witness.descriptor."))
        .collect::<Vec<_>>();
    assert_eq!(descriptors.len(), 1, "{descriptors:#?}");
    assert!(descriptors[0].contains("i64 0, i64 1"), "{descriptors:#?}");
    assert!(!llvm.contains("noiseOne"), "{llvm}");
    assert!(!llvm.contains("noiseTwo"), "{llvm}");
    assert!(!llvm.contains("noiseThree"), "{llvm}");
    assert!(!llvm.contains("unusedText"), "{llvm}");

    assert_emitted_main_succeeds(&project);
}

#[test]
fn concrete_conditional_erasure_owns_proof_after_the_producer_stack_returns() {
    let source = r#"module compact_escape

dyn concept Render {
    method render(self) Text
}

record Label { text Text }

impl Render for Label {
    method render(self) Text { self.text }
}

record Wrapped[T] { value T }

impl[T: Render] Render for Wrapped[T] {
    method render(self) Text { self.value.render() }
}

fn eraseWrapped(value Wrapped[Label]) dyn Render { value }

fn produce() dyn Render {
    eraseWrapped(Wrapped { value = Label { text = "survived" } })
}

pub fn main() {
    let erased = produce()
    let text = erased.render()
    assert text == "survived"
}
"#;
    let (project, program, llvm) = emit_source_with_ir(source);
    let erase_source = program
        .functions
        .iter()
        .find(|function| function.name.rsplit('.').next() == Some("eraseWrapped"))
        .expect("concrete conditional erase function");
    assert_eq!(erase_source.witness_prefix_count, 0);
    assert_eq!(erase_source.witness_params.len(), 0);
    assert!(program.functions.iter().any(|function| {
        function.name.rsplit('.').next() == Some("render") && function.witness_prefix_count == 1
    }));

    let erase = llvm_function(&llvm, "eraseWrapped");
    assert!(erase.contains("@loom_gc_clone_witness_v1"), "{erase}");
    assert!(erase.contains("witness.application"), "{erase}");
    assert!(erase.contains("witness.prerequisites"), "{erase}");
    let data_allocation = erase
        .find("@loom_gc_alloc_value")
        .expect("owned dyn allocates its data box");
    let witness_clone = erase
        .find("@loom_gc_clone_witness_v1")
        .expect("conditional proof is cloned");
    let data_publication = erase
        .find("dyn.data.publish.value")
        .expect("owned dyn publishes its data box");
    assert!(
        erase[..data_allocation].contains("@loom_gc_clone_value_v1"),
        "owned value must be cloned before its data box allocation: {erase}"
    );
    assert!(
        !erase[data_allocation..].contains("@loom_gc_clone_value_v1"),
        "MakeView retained the moving destination-before-clone order: {erase}"
    );
    assert!(data_allocation < data_publication, "{erase}");
    assert!(data_publication < witness_clone, "{erase}");
    assert_gc_state_published_before(erase, "@loom_gc_alloc_value");
    assert_gc_state_published_before(erase, "@loom_gc_clone_witness_v1");
    assert!(llvm.contains("dyn.call"), "{llvm}");
    assert!(
        llvm.lines().any(|line| {
            line.starts_with("@loom.witness.descriptor.") && line.contains("i64 1")
        })
    );

    assert_emitted_main_succeeds(&project);
}

#[test]
#[allow(clippy::too_many_lines)]
fn projected_inout_and_dynamic_calls_use_address_free_stable_proxies() {
    let source = r#"module stable_call_carriers

dyn concept CounterOps {
    method add(mut self, amount Int) Int
    method read(self) Int
}

dyn concept ReadableCounter {
    method read(self) Int
}

record Counter { value Int }
record Holder { counter Counter }

impl CounterOps for Counter {
    method add(mut self, amount Int) Int {
        self.value = self.value + amount
        if amount < 0 {
            assert false
            Unit
        } else {
            Unit
        }
        self.value
    }

    method read(self) Int { self.value }
}

impl ReadableCounter for Counter {
    method read(self) Int { self.value }
}

impl Holder {
    method addProjected(mut self, amount Int) Int {
        self.counter.add(amount)
    }

    method failProjected(mut self) {
        discard self.counter.add(-1)
    }

    method addAfterAllocation(mut self) Int {
        self.counter.add(allocatingAmount())
    }
}

fn allocatingAmount() Int {
    let allocated = "a".concat("bc")
    allocated.length()
}

fn addDynamic(counter dyn CounterOps, amount Int) Int {
    counter.add(amount)
}

fn readDynamic(counter dyn ReadableCounter) Int {
    counter.read()
}

fn observeReadonly(holder Holder) Int {
    readDynamic(holder.counter)
}

pub fn main() {
    var projected = Holder { counter = Counter { value = 10 } }
    let projectedValue = projected.addProjected(2)
    assert projectedValue == 12
    let projectedAfter = projected.counter.value
    assert projectedAfter == 12
    let allocatedValue = projected.addAfterAllocation()
    assert allocatedValue == 15

    var dynamic = Holder { counter = Counter { value = 20 } }
    let dynamicValue = addDynamic(dynamic.counter, 3)
    assert dynamicValue == 23
    let dynamicAfter = dynamic.counter.value
    assert dynamicAfter == 23
    let readonlyValue = observeReadonly(dynamic)
    assert readonlyValue == 23

    if false {
        projected.failProjected()
    } else {
        Unit
    }
}
"#;
    let (project, _program, llvm) = emit_source_with_ir(source);

    let projected = llvm_function(&llvm, "stable_call_carriers_addProjected");
    let projected_call = projected
        .find("call i32 @loom.fn.")
        .expect("projected call");
    let projected_writeback = projected[projected_call..]
        .find("inout.writeback.value")
        .map(|offset| projected_call + offset)
        .expect("projected writeback");
    let projected_status = projected[projected_writeback..]
        .find("call.ok")
        .map(|offset| projected_writeback + offset)
        .expect("status propagation after projected writeback");
    assert!(projected.contains("inout.projected.proxy"), "{projected}");
    assert!(projected.contains("inout.copy.in.value"), "{projected}");
    assert!(projected_call < projected_writeback, "{projected}");
    assert!(projected_writeback < projected_status, "{projected}");

    let allocating = llvm_function(&llvm, "stable_call_carriers_addAfterAllocation");
    let allocation = allocating
        .find("stable_call_carriers_allocatingAmount")
        .expect("later allocating argument is evaluated");
    let copy_in = allocating
        .find("inout.copy.in.value")
        .expect("projected receiver copy-in");
    assert!(allocation < copy_in, "{allocating}");

    let failure = llvm_function(&llvm, "stable_call_carriers_failProjected");
    let failure_call = failure.find("call i32 @loom.fn.").expect("failing call");
    let failure_writeback = failure[failure_call..]
        .find("inout.writeback.value")
        .map(|offset| failure_call + offset)
        .expect("failure-path projected writeback");
    let failure_status = failure[failure_writeback..]
        .find("call.ok")
        .map(|offset| failure_writeback + offset)
        .expect("failure status propagation");
    assert!(failure_call < failure_writeback, "{failure}");
    assert!(failure_writeback < failure_status, "{failure}");

    let dynamic = llvm_function(&llvm, "stable_call_carriers_addDynamic");
    let dynamic_call = dynamic.find("dyn.call").expect("dynamic dispatch");
    let receiver_writeback = dynamic[dynamic_call..]
        .find("dyn.receiver.writeback.value")
        .map(|offset| dynamic_call + offset)
        .expect("dynamic receiver writeback");
    let dynamic_status = dynamic[receiver_writeback..]
        .find("call.ok")
        .map(|offset| receiver_writeback + offset)
        .expect("dynamic status propagation");
    assert!(dynamic.contains("dyn.dispatch.proxy"), "{dynamic}");
    assert!(dynamic.contains("dyn.dispatch.copy.in.value"), "{dynamic}");
    assert!(dynamic_call < receiver_writeback, "{dynamic}");
    assert!(receiver_writeback < dynamic_status, "{dynamic}");

    let main = llvm_native_function(&llvm, "stable_call_carriers_main");
    assert!(main.contains("dyn.borrow.writeback.data"), "{main}");
    assert!(main.contains("dyn.borrow.writeback.value"), "{main}");
    let readonly = llvm_function(&llvm, "stable_call_carriers_observeReadonly");
    assert!(!readonly.contains("dyn.borrow.writeback"), "{readonly}");
    assert!(!llvm.contains("ptrtoint"), "{llvm}");
    assert!(!llvm.contains("inttoptr"), "{llvm}");

    assert_emitted_main_succeeds(&project);
}

#[test]
fn projected_inout_commits_before_fault_cleanup_observes_the_owner() {
    let source = r#"module fault_writeback_order

record Cell { value Int }
record Holder { cell Cell }

impl Cell {
    method mutateThenFail(mut self) {
        self.value = 9
        assert false
    }
}

impl Holder {
    method invokeFailure(mut self) {
        self.cell.mutateThenFail()
    }
}

async fn failAndCleanup() {
    var holder = Holder { cell = Cell { value = 1 } }
    defer {
        let observed = holder.cell.value
        if observed == 9 {
            Unit
        } else {
            let impossible = 1 / 0
            discard impossible
            Unit
        }
    }
    holder.invokeFailure()
}

async fn complete() {}

pub async fn main() {
    let failed, completed = Task.settled(failAndCleanup(), complete()).await
    match failed {
        Faulted(fault) => {
            let code = fault.code()
            assert code == "AssertionFault"
            Unit
        }
        Completed(_) => {
            assert false
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
    match completed {
        Completed(_) => Unit
        Faulted(_) => {
            assert false
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
}
"#;
    let (project, _program, llvm) = emit_source_with_ir(source);

    let invoke = llvm_function(&llvm, "fault_writeback_order_invokeFailure");
    let call = invoke.find("call i32 @loom.fn.").expect("failing call");
    let writeback = invoke[call..]
        .find("inout.writeback.value")
        .map(|offset| call + offset)
        .expect("projected owner writeback");
    let status = invoke[writeback..]
        .find("call.ok")
        .map(|offset| writeback + offset)
        .expect("status branch after writeback");
    assert!(call < writeback, "{invoke}");
    assert!(writeback < status, "{invoke}");

    assert_emitted_main_succeeds(&project);
}

#[test]
fn native_int_is_checked_i64_even_after_llvm_optimization() {
    let cases = [
        (
            "overflow",
            "let value = 9223372036854775807 + 1",
            "IntegerOverflow",
        ),
        (
            "division_by_zero",
            "let value = 7 / 0",
            "IntegerDivisionByZero",
        ),
        (
            "division_overflow",
            "let value = -9223372036854775808 / -1",
            "IntegerDivisionOverflow",
        ),
    ];
    for (name, statement, expected) in cases {
        let source = format!("module sample\n\npub fn main() {{\n    {statement}\n}}\n");
        let project = tempfile::tempdir().expect("create source project");
        std::fs::write(project.path().join("main.loom"), source).expect("write source");
        let snapshot = AnalysisHost::new(project.path())
            .expect("load project")
            .snapshot()
            .expect("analyze project");
        assert!(
            !snapshot.has_errors(),
            "{name}: {:?}",
            snapshot.diagnostics()
        );
        let program = snapshot.executable().expect("lower executable MIR");
        let executable = project.path().join("program");
        emit_native(program, &executable, &EmitOptions::run("main"))
            .expect("emit native executable");
        let output = Command::new(&executable)
            .output()
            .expect("run native executable");
        assert!(!output.status.success(), "{name} unexpectedly succeeded");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{name}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn deferred_integer_checks_use_cleanup_execution_order_not_registration_order() {
    let source = r"module deferred_overflow

pub fn main() {
    var value = 0
    defer {
        value = value * 9223372036854775807
        Unit
    }
    defer {
        value = 2
        Unit
    }
}
";
    let project = tempfile::tempdir().expect("create deferred overflow project");
    std::fs::write(project.path().join("main.loom"), source)
        .expect("write deferred overflow source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load deferred overflow project")
        .snapshot()
        .expect("analyze deferred overflow project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot
        .executable()
        .expect("lower deferred overflow executable MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit deferred overflow executable");
    let output = Command::new(executable)
        .output()
        .expect("run deferred overflow executable");
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("IntegerOverflow"),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn float_text_builtins_compile_and_run_natively() {
    let source = r#"module sample

import std.float.parse_float
import std.float.format_float

pub fn main() {
    let finite = format_float(1.25)
    assert finite == "1.25"
    let large = format_float(1e20)
    assert large == "100000000000000000000.0"
    let small = format_float(1e-7)
    assert small == "0.0000001"
    let negative_zero = format_float(-0.0)
    assert negative_zero == "-0.0"
    let positive_infinity = format_float(1.0 / 0.0)
    assert positive_infinity == "Infinity"
    let negative_infinity = format_float(-1.0 / 0.0)
    assert negative_infinity == "-Infinity"
    let not_a_number = format_float(0.0 / 0.0)
    assert not_a_number == "NaN"

    match parse_float("1e3") {
        Ok(value) => {
            assert value == 1000.0
            Unit
        }
        _ => {
            assert false
            Unit
        }
    }
    match parse_float("1") {
        Err(std.float.ParseFloatError.InvalidSyntax) => Unit
        _ => {
            assert false
            Unit
        }
    }
    match parse_float("1e999") {
        Err(std.float.ParseFloatError.OutOfRange) => Unit
        _ => {
            assert false
            Unit
        }
    }
}

"#;
    let project = tempfile::tempdir().expect("create source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load project")
        .snapshot()
        .expect("analyze project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower executable MIR");
    let executable = project.path().join("program");
    emit_native(program, &executable, &EmitOptions::run("main"))
        .expect("emit native executable with float runtime");
    let output = Command::new(&executable)
        .output()
        .expect("run native executable");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
