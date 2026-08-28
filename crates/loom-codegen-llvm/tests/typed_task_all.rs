use loom_codegen_ir::{
    InstructionKind, LoweringOutcome, SourceArtifactRequest, TargetLayout, TerminatorKind,
    dump_program, lower_typed_artifact,
};
use loom_driver::AnalysisHost;

const SOURCE: &str = r"module typed_task_all_dump

async fn flagChild() Bool { true }

async fn numberChild() Int { 42 }

async fn directJoin() {
    let flag, number = Task.all(flagChild(), numberChild()).await
    assert flag
    assert number == 42
}

async fn storedJoin() {
    let combined = Task.all(flagChild(), numberChild())
    let flag, number = combined.await
    assert flag
    assert number == 42
}

pub async fn main() {
    directJoin().await
    storedJoin().await
}
";

fn compile_source(source: &str) -> loom_mir::CheckedProgram {
    let project = tempfile::tempdir().expect("create Task.all source project");
    std::fs::write(project.path().join("main.loom"), source).expect("write Task.all source");
    let snapshot = AnalysisHost::new(project.path())
        .expect("load Task.all source project")
        .snapshot()
        .expect("analyze Task.all source");
    assert!(
        !snapshot.has_errors(),
        "Task.all source diagnostics: {:#?}",
        snapshot.diagnostics()
    );
    snapshot.executable().expect("lower checked MIR").clone()
}

fn function_dump<'dump>(dump: &'dump str, function: &loom_codegen_ir::Function) -> &'dump str {
    let header = format!("fn {} mir=", function.id());
    let start = dump
        .find(&header)
        .unwrap_or_else(|| panic!("missing `{header}` in canonical LCIR dump:\n{dump}"));
    let end = dump[start..]
        .find("\nfn ")
        .map_or(dump.len(), |offset| start + offset);
    &dump[start..end]
}

fn join_widths(function: &loom_codegen_ir::Function) -> Vec<usize> {
    function
        .instructions()
        .iter()
        .filter_map(|instruction| match instruction.kind() {
            InstructionKind::TaskJoinAll { tasks } => Some(tasks.len()),
            _ => None,
        })
        .collect()
}

fn await_widths(function: &loom_codegen_ir::Function) -> Vec<usize> {
    function
        .blocks()
        .iter()
        .filter_map(
            |block| match block.terminator().map(loom_codegen_ir::Terminator::kind) {
                Some(TerminatorKind::AwaitTasks { tasks, .. }) => Some(tasks.len()),
                _ => None,
            },
        )
        .collect()
}

#[test]
fn canonical_dump_distinguishes_immediate_and_stored_task_all() {
    let program = compile_source(SOURCE);
    let outcome = lower_typed_artifact(
        &program,
        &SourceArtifactRequest::Run {
            entry: "main".into(),
        },
        TargetLayout::new(64).expect("64-bit Task ABI"),
    )
    .expect("classify fixed Task.all source");
    let LoweringOutcome::Complete(artifact) = outcome else {
        panic!("fixed Task.all source must lower through typed LCIR: {outcome:?}")
    };
    let direct = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("directJoin"))
        .expect("directJoin LCIR instance");
    let stored = artifact
        .functions()
        .iter()
        .find(|function| function.name().ends_with("storedJoin"))
        .expect("storedJoin LCIR instance");

    assert!(
        join_widths(direct).is_empty(),
        "an immediate Task.all must not allocate a composite Task"
    );
    assert_eq!(
        await_widths(direct),
        [2],
        "an immediate Task.all must await its two children directly"
    );

    assert_eq!(
        join_widths(stored),
        [2],
        "a stored Task.all must construct one exact composite Task"
    );
    assert_eq!(
        await_widths(stored),
        [1],
        "awaiting a stored Task.all must await only that composite Task"
    );

    let dump = dump_program(artifact.program());
    assert_eq!(
        dump,
        dump_program(artifact.program()),
        "LCIR dump must be canonical"
    );
    let direct_dump = function_dump(&dump, direct);
    let stored_dump = function_dump(&dump, stored);
    assert_eq!(
        direct_dump.matches("task.join_all(").count(),
        0,
        "{direct_dump}"
    );
    assert_eq!(
        direct_dump.matches("await_tasks all state ").count(),
        1,
        "{direct_dump}"
    );
    assert_eq!(
        stored_dump.matches("task.join_all(").count(),
        1,
        "{stored_dump}"
    );
    assert_eq!(
        stored_dump.matches("await_tasks all state ").count(),
        1,
        "{stored_dump}"
    );
}
