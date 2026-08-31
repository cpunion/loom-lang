use loom_codegen_ir::{
    LoweringOutcome, SourceArtifactRequest, TargetLayout, dump_program, lower_typed_artifact,
};
use loom_codegen_llvm::{EmitOptions, emit_prepared_native_object, prepare_native_object};

mod support;

const SOURCE: &str = r"record Counter {
    value Int
}

record Inner {
    queued Task[Int]
    counter Counter
}

record Envelope {
    pending Task[Int]
    inner Inner
}

impl Counter {
    method add(mut self, value Int) {
        self.value = self.value + value
    }
}

async fn child(value Int) Int { value }

// This recursive sink consumes the complete affine carrier without requiring
// a projected Task move. The regression only emits an object and never runs it.
fn consume(value Envelope) { consume(value) }

pub async fn main() {
    var envelope = Envelope {
        pending = child(1),
        inner = Inner {
            queued = child(2),
            counter = Counter { value = 0 }
        }
    }
    envelope.inner.counter.add(1)
    consume(envelope)
}
";

fn compile_source() -> loom_mir::CheckedProgram {
    let project = tempfile::tempdir().expect("create Task-carrier source project");
    std::fs::write(project.path().join("main.loom"), SOURCE).expect("write Task-carrier source");
    let snapshot = support::analysis_host(project.path())
        .expect("load Task-carrier source project")
        .snapshot()
        .expect("analyze Task-carrier source");
    assert!(
        !snapshot.has_errors(),
        "Task-carrier source diagnostics: {:#?}",
        snapshot.diagnostics()
    );
    snapshot.executable().expect("lower checked MIR").clone()
}

#[test]
fn nested_task_carrier_projection_emits_typed_lcir_object() {
    let program = compile_source();
    let request = SourceArtifactRequest::Run {
        entry: "main".into(),
    };
    let layout = TargetLayout::new(
        u16::try_from(usize::BITS).expect("host pointer width must fit the LCIR target layout"),
    )
    .expect("supported host target layout");
    let outcome = lower_typed_artifact(&program, &request, layout)
        .expect("classify nested Task-carrier source");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("nested Task-carrier source must lower through typed LCIR: {outcome:?}")
    };
    let dump = dump_program(artifact.program());
    assert!(dump.contains("task_carrier.project"), "{dump}");
    assert!(dump.contains("task_carrier.update"), "{dump}");

    let output = tempfile::tempdir().expect("create native output directory");
    let object = output.path().join("task-carrier.o");
    let llvm_ir = output.path().join("task-carrier.ll");
    let mut options = EmitOptions::run("main");
    options.emit_ir = Some(llvm_ir.clone());
    let prepared = prepare_native_object(&program, options)
        .expect("prepare nested Task-carrier typed LCIR object");
    let emitted = emit_prepared_native_object(&prepared, &object)
        .expect("emit nested Task-carrier typed LCIR object");
    assert_eq!(emitted.object, object);
    assert!(object.is_file(), "native object was not written");

    let llvm_ir = std::fs::read_to_string(llvm_ir).expect("read emitted LLVM IR");
    assert!(llvm_ir.contains("@loom.lcir.fn."), "{llvm_ir}");
    for forbidden in ["@loom.fn.", "%loom.Value", "loom.Value", "ValueNode"] {
        assert!(
            !llvm_ir.contains(forbidden),
            "found `{forbidden}`:\n{llvm_ir}"
        );
    }
}
