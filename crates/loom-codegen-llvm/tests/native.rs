#![allow(clippy::default_trait_access)]

use std::collections::BTreeMap;
use std::process::Command;

use loom_codegen_llvm::{
    EmitOptions, OptimizationProfile, emit_native, emit_native_object, target_identity,
    validate_native_link_target,
};
use loom_driver::AnalysisHost;
use loom_mir::{Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, Program, Type};

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
const CROSS_TRIPLE: &str = "x86_64-unknown-linux-gnu";
#[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
const CROSS_TRIPLE: &str = "aarch64-unknown-linux-gnu";

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
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
}

#[test]
fn root_result_is_consumed_before_its_runtime_is_destroyed() {
    let source = r"module root_result_lifetime

pub fn main() Unit {
    var values = List[Int]()
    values.add(1)
    Unit
}

test fn list_result_lifetime() {
    var values = List[Int]()
    values.add(2)
    Unit
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
    let print = llvm
        .find("call void @loom.runtime.print")
        .expect("root must consume its result");
    let destroy = llvm[print..]
        .find("call i32 @loom_runtime_destroy_v1")
        .map(|offset| print + offset)
        .expect("allocating root must release its runtime");
    assert!(
        print < destroy,
        "root runtime was destroyed before result use:\n{llvm}"
    );
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
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");

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
    assert_ne!(development.optimization, release.optimization);

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
    let error = validate_native_link_target(&options).expect_err("cross link is unavailable");
    assert_eq!(error.code(), "CrossLinkUnavailable");
}

#[test]
fn compatibility_value_abi_rejects_32_bit_targets() {
    let error = target_identity(
        Some("i686-unknown-linux-gnu"),
        OptimizationProfile::Development,
    )
    .expect_err("the compatibility Value ABI is 64-bit only");
    assert_eq!(error.code(), "UnsupportedNativePointerWidth");
    assert!(error.to_string().contains("requires 64-bit pointers"));
}

#[test]
fn text_literals_are_immortal_versioned_objects_across_a_gc_safepoint() {
    let source = r#"module text_literal_object

pub async fn main() Unit {
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
    Unit
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
    assert!(!llvm.contains("loom_runtime_text_length"));

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
fn release_pipeline_folds_live_constants_and_eliminates_machine_dead_code() {
    let source = r"module optimize

fn folded() Int {
    40 + 2
}

fn unreachable() Int {
    100 + 23
}

pub fn main() Unit {
    let value = folded()
    assert value == 42
    Unit
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
    let development_definitions = development
        .lines()
        .filter(|line| line.starts_with("define "))
        .collect::<Vec<_>>();
    assert!(
        development.contains("define internal { i32, i64 } @loom.int.fn.0.optimize_folded"),
        "{development_definitions:#?}"
    );
    assert!(!development.contains("optimize_unreachable"));
    assert!(development.contains("llvm.sadd.with.overflow.i64"));
    assert!(!release.contains("optimize_folded"));
    assert!(!release.contains("optimize_unreachable"));
    assert!(!release.contains("llvm.sadd.with.overflow.i64"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn scalar_int_abi_is_recursive_checked_and_uses_runtime_at_the_root() {
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

pub fn main() Int {
    let recursive = fibonacci(20)
    assert recursive == 6765
    contracted(41)
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
    let fibonacci = llvm_integer_function(&llvm, "scalar_int_fibonacci");
    assert!(
        fibonacci
            .lines()
            .next()
            .is_some_and(|line| line.contains("i64 %0, ptr %1")),
        "{fibonacci}"
    );
    assert!(
        fibonacci.contains("call { i32, i64 } @loom.int.fn.0.scalar_int_fibonacci"),
        "{fibonacci}"
    );
    assert!(!fibonacci.contains("%loom.ArgNode"), "{fibonacci}");
    assert!(
        fibonacci.contains("llvm.ssub.with.overflow.i64"),
        "{fibonacci}"
    );
    assert!(
        fibonacci.contains("llvm.sadd.with.overflow.i64"),
        "{fibonacci}"
    );
    let contracted = llvm_integer_function(&llvm, "scalar_int_contracted");
    assert!(
        contracted.contains("call void @loom.runtime.clone"),
        "{contracted}"
    );
    assert!(llvm.contains("define internal i32 @loom.fn.2.scalar_int_main"));
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
    assert_eq!(output.stdout, b"42\n");

    let overflow_source = r"module scalar_int_fault

fn checkedAdd(left Int, right Int) Int {
    left + right
}

pub fn main() Int {
    checkedAdd(9223372036854775807, 1)
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

pub fn main() Int {
    choose(42)
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
    let identity = llvm_integer_function(&llvm, "pure_scalar_int_identity");
    let choose = llvm_integer_function(&llvm, "pure_scalar_int_choose");
    let main = llvm_integer_function(&llvm, "pure_scalar_int_main");
    assert!(
        identity
            .lines()
            .next()
            .is_some_and(|line| line.contains("define internal i64") && line.contains("i64 %0")),
        "{identity}"
    );
    assert!(
        choose.contains("call i64 @loom.int.fn.0.pure_scalar_int_identity"),
        "{choose}"
    );
    assert!(
        main.contains("call i64 @loom.int.fn.1.pure_scalar_int_choose"),
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
    assert_eq!(output.stdout, b"42\n");
}

#[test]
#[allow(clippy::too_many_lines)]
fn loop_temporaries_are_entry_allocated_and_large_release_loops_do_not_grow_stack() {
    let source = r"module stack_loop

record Counter {
    total Int
    calls Int
}

impl Counter {
    method add(mut self, value Int) Unit {
        self.total = self.total + value
        self.calls = self.calls + 1
        Unit
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

pub fn main() Unit {
    let state = spin(100000)
    assert state == 1405402365
    let counter = recordMethod(100000)
    assert counter.total == 51031728
    assert counter.calls == 100000
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
    Unit
}
";
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
                !line.contains(" alloca ") || block == "entry",
                "{function_name} contains a dynamic alloca in `{block}`: {line}"
            );
        }
    }
    let add = llvm_function(&llvm, "stack_loop_add");
    assert!(add.contains("copy.scalar"), "{add}");
    assert!(add.contains("assign.scalar"), "{add}");
    assert!(!add.contains("move = load %loom.Value"), "{add}");
    let record_method = llvm_function(&llvm, "stack_loop_recordMethod");
    assert!(record_method.contains("record.local"), "{record_method}");
    assert!(
        !record_method.contains("@loom_gc_alloc_value_node"),
        "{record_method}"
    );

    let executable = project.path().join("release-program");
    let release = EmitOptions::run("main").with_optimization(OptimizationProfile::Release);
    emit_native(program, &executable, &release).expect("emit release loop executable");
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
#[allow(clippy::too_many_lines)]
fn readonly_list_builtins_snapshot_the_header_and_clone_only_the_selected_value() {
    let source = r"module list_readonly

record Boxed {
    value Int
}

impl Boxed {
    method bump(mut self) Unit {
        self.value = self.value + 1
        Unit
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

pub async fn main() Unit {
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
    Unit
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
    assert!(!read_at[..get].contains("@loom.runtime.clone"), "{read_at}");
    assert!(read_at[get..].contains("@loom.runtime.clone"), "{read_at}");

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

pub fn main() Unit {
    let scan = renamedSameShape(40)
    let empty = renamedSameShape(0)
    let edges = edgeReads()
    let cleanup = cleanupOnFaultPath(true)
    assert scan == 2300
    assert empty == 0
    assert edges == 56
    assert cleanup == 42
    Unit
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
        let scan = llvm_integer_function(&llvm, "native_int_list_renamedSameShape");
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
        assert!(!scan.contains("@loom.runtime.clone"), "{scan}");

        if ir == &development_ir {
            let cleanup = llvm_function(&llvm, "native_int_list_cleanupOnFaultPath");
            assert!(cleanup.contains("@loom_int_list_drop_v1"), "{cleanup}");
            for block in cleanup
                .split("\n\n")
                .filter(|block| block.contains("ret i32"))
            {
                assert!(
                    block.contains("@loom_int_list_drop_v1"),
                    "status return omitted native list cleanup: {block}"
                );
                assert_eq!(
                    block.matches("@loom_int_list_drop_v1").count(),
                    1,
                    "status return cleaned native list more than once: {block}"
                );
            }
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

pub async fn main() Unit {
    let escaped = escaping()
    let text = textList()
    let observed = receiverObservationHazard()
    let waited = asynchronous().await
    assert escaped == 1
    assert text == 1
    assert observed == 1
    assert waited == 1
    Unit
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
        let body = llvm_integer_function(&llvm, function);
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

import standard.float.is_finite

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

pub fn main() Unit {
    let money = direct()
    assert money == 10.0
    match checked(-1.0) {
        Err(first) => match checked(-2.0) {
            Err(second) => {
                assert first != second
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

    let output = Command::new(executable)
        .output()
        .expect("run construction executable");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
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

fn llvm_integer_function<'source>(ir: &'source str, symbol_suffix: &str) -> &'source str {
    let marker = "define internal ";
    let start = ir
        .match_indices(marker)
        .map(|(index, _)| index)
        .find(|index| {
            ir[*index..]
                .lines()
                .next()
                .is_some_and(|line| line.contains("@loom.int.fn.") && line.contains(symbol_suffix))
        })
        .unwrap_or_else(|| panic!("missing scalar Int LLVM function containing `{symbol_suffix}`"));
    let rest = &ir[start + marker.len()..];
    let end = rest.find("\ndefine ").unwrap_or(rest.len());
    &ir[start..start + marker.len() + end]
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

fn unit_program() -> Program {
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
        locals: Vec::new(),
        return_ty: Type::Unit,
        receiver: None,
        body: Block {
            statements: Vec::new(),
            tail: Some(Box::new(Expr {
                kind: ExprKind::Constant(Constant::Unit),
                ty: Type::Unit,
                span: Default::default(),
            })),
            span: Default::default(),
        },
        call_plan: CallPlan::default(),
    });
    program.exports = BTreeMap::from([("main".into(), FunctionId(0))]);
    program
}

#[test]
fn core_examples_compile_and_run_as_native_programs() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    for version in ["core01", "core02", "core03"] {
        let source = workspace.join("examples").join(version);
        let snapshot = AnalysisHost::new(&source)
            .expect("load project")
            .snapshot()
            .expect("analyze project");
        assert!(
            !snapshot.has_errors(),
            "{version}: {:?}",
            snapshot.diagnostics()
        );
        let program = snapshot.executable().expect("lower executable MIR");
        let directory = tempfile::tempdir().expect("create temp directory");
        let executable = directory.path().join("program");
        let mut options = EmitOptions::run("main");
        if version == "core03" {
            options.emit_ir = Some(directory.path().join("program.ll"));
        }
        emit_native(program, &executable, &options)
            .unwrap_or_else(|error| panic!("{version}: {error}"));
        let output = Command::new(&executable).output().expect("run executable");
        assert!(
            output.status.success(),
            "{version}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
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
            .unwrap_or_else(|error| panic!("{version} tests: {error}"));
        let output = Command::new(&tests).output().expect("run native tests");
        assert!(
            output.status.success(),
            "{version} tests: stdout={} stderr={}",
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
    // Keep the completion ordering deterministic even when the full test
    // workspace is CPU-saturated and both timers are observed in one poll.
    Task.sleep(50).await
    1
}

async fn two() Int {
    Task.sleep(1).await
    2
}

pub async fn main() Unit {
    Task.waitWritable(1).await
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
    Unit
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
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
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

pub async fn main() Unit {
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
    Unit
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
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
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

import standard.time.milliseconds
import standard.file.open_read
import standard.file.create
import standard.net.connect

pub async fn main() Unit {{
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
    Unit
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
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
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
    Task.sleep(1).await
    2
}

pub async fn main() Unit {
    let winner = Task.any(slow(), fast()).await
    assert winner == 2
    Unit
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

pub async fn main() Unit {
    let selected = choose(true).await
    assert selected == 7
    Unit
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
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Unit\n");
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

pub fn main() Unit {
    let left = Boxed { value = Atom { value = 7 } }
    let right = Boxed { value = Atom { value = 7 } }
    let equal = same(left, right)
    assert equal
    Unit
}

test fn conditional_witness() {
    let left = Boxed { value = Atom { value = 7 } }
    let right = Boxed { value = Atom { value = 7 } }
    let equal = same(left, right)
    assert equal
    Unit
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

pub fn main() Unit {
    let holder = Holder { value = Left { value = 1 } }
    let other = Right { value = "ok" }
    let combined = holder.both(other)
    assert combined
    Unit
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
    emit_native(program, &executable, &EmitOptions::run("main")).expect("emit native executable");
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
        let source =
            format!("module sample\n\npub fn main() Unit {{\n    {statement}\n    Unit\n}}\n");
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
fn float_text_builtins_compile_and_run_natively() {
    let source = r#"module sample

import standard.float.parse_float
import standard.float.format_float

pub fn main() Unit {
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
        Err(standard.float.ParseFloatError.InvalidSyntax) => Unit
        _ => {
            assert false
            Unit
        }
    }
    match parse_float("1e999") {
        Err(standard.float.ParseFloatError.OutOfRange) => Unit
        _ => {
            assert false
            Unit
        }
    }
    Unit
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
