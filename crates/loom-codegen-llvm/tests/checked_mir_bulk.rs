use std::fmt::Write as _;
use std::process::Command;

use loom_codegen_llvm::{
    EmitOptions, NativeRouteKind, NativeRoutePolicy, emit_prepared_native_object,
    prepare_native_object,
};
use loom_interpreter::{Interpreter, Value};

mod support;
use support::link_native_object;

fn compile_source(source: &str) -> loom_mir::CheckedProgram {
    let project = tempfile::tempdir().expect("create checked-MIR bulk source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write source fixture");
    let snapshot = support::analysis_host(project.path())
        .expect("load source project")
        .snapshot()
        .expect("analyze source project");
    assert!(
        !snapshot.has_errors(),
        "source diagnostics: {:#?}",
        snapshot.diagnostics()
    );
    snapshot.executable().expect("lower checked MIR").clone()
}

#[test]
fn duplicate_after_prior_insert_keeps_loop_output_roots_live_across_moving_collection() {
    const KEY_COUNT: usize = 384;
    let mut source = String::from("fn verify() Bool {\n    var entries = List[(Text, Int)]()\n");
    for index in 0..KEY_COUNT {
        writeln!(source, "    entries.add((\"key{index:04}\", {index}))")
            .expect("append unique source entry");
    }
    for index in 0..KEY_COUNT {
        writeln!(source, "    entries.add((\"key{index:04}\", {index}))")
            .expect("append duplicate source entry");
    }
    source.push_str(
        "    match entries.to_text_map() {\n        Err(key) => key == \"key0000\"\n        Ok(_) => false\n    }\n}\n\npub fn main() {\n    let valid = verify()\n    assert valid\n}\n",
    );

    let program = compile_source(&source);
    let main = program.exports["main"];
    let span = program.function(main).expect("source main").span;
    assert_eq!(
        Interpreter::new(&program).invoke(main, Vec::new(), span),
        Ok(Value::Unit)
    );

    let directory = tempfile::tempdir().expect("create checked-MIR bulk output directory");
    let object = directory.path().join("checked-mir-bulk-roots.o");
    let executable = directory.path().join("checked-mir-bulk-roots");
    let ir_path = directory.path().join("checked-mir-bulk-roots.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(ir_path.clone());
    let prepared = prepare_native_object(&program, options, NativeRoutePolicy::CheckedMirOnly)
        .expect("prepare checked-MIR bulk root regression");
    assert_eq!(prepared.route_kind(), NativeRouteKind::CheckedMir);
    emit_prepared_native_object(&prepared, &object).expect("emit checked-MIR bulk object");
    link_native_object(&object, &executable).expect("link checked-MIR bulk executable");
    let output = Command::new(executable)
        .output()
        .expect("run checked-MIR bulk executable");
    assert!(output.status.success(), "{output:#?}");
    assert_eq!(output.stdout, b"Unit\n");
    assert!(output.stderr.is_empty(), "{output:#?}");

    let ir = std::fs::read_to_string(ir_path).expect("read checked-MIR bulk LLVM IR");
    assert!(ir.contains("list.to_text_map.next_duplicates"), "{ir}");
    assert!(ir.contains("list.to_text_map.next_map"), "{ir}");
    assert!(ir.contains("list.to_text_map.record_duplicate"), "{ir}");
    assert!(ir.contains("list.to_text_map.insert"), "{ir}");
}
