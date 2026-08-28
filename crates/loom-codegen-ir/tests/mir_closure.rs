use std::collections::BTreeSet;

use loom_codegen_ir::{
    MirClosureError, SourceRoots, analyze_source_reachability, close_interpreted_executable,
};
use loom_core::{FileId, LOOM_LANGUAGE_VERSION, Name, PackageId, Span};
use loom_hir::{PackageSourceUnit, lower_package_files};
use loom_lowering::lower_to_mir;
use loom_mir::{
    CallTarget, CheckedProgram, ExprKind, Statement, StatementKind, WitnessId, check_program,
    decode_interpreted_executable_artifact, encode_interpreted_executable_artifact,
};
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fn compile_with_standard_resource(source: &str) -> CheckedProgram {
    let application = parse_with_file(FileId(0), source);
    let resource = parse_with_file(
        FileId(1),
        include_str!("../../../library/standard/src/resource.loom"),
    );
    assert!(
        application.diagnostics().is_empty() && resource.diagnostics().is_empty(),
        "syntax diagnostics: application={:#?} standard={:#?}",
        application.diagnostics(),
        resource.diagnostics()
    );

    let standard = PackageId::compiler_standard(LOOM_LANGUAGE_VERSION);
    let root = PackageId::legacy();
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: FileId(0),
            package: root.clone(),
            syntax: application.ast(),
        },
        PackageSourceUnit {
            file: FileId(1),
            package: standard.clone(),
            syntax: resource.ast(),
        },
    ]);
    lowered
        .program
        .register_package(standard.clone(), [], false);
    lowered
        .program
        .register_package(root, [(Name::new("standard"), standard)], true);
    assert!(
        lowered.diagnostics.is_empty(),
        "HIR diagnostics: {:#?}",
        lowered.diagnostics
    );

    let analysis = analyze(&lowered.program);
    assert!(
        analysis.diagnostics.is_empty(),
        "semantic diagnostics: {:#?}",
        analysis.diagnostics
    );
    lower_to_mir(&lowered.program, &analysis)
        .unwrap_or_else(|failure| panic!("MIR diagnostics: {:#?}", failure.diagnostics()))
}

fn leaf(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

fn function_names(program: &CheckedProgram) -> BTreeSet<&str> {
    program
        .functions
        .iter()
        .map(|function| leaf(&function.name))
        .collect()
}

fn assert_dense_global_ids(program: &CheckedProgram) {
    assert!(
        program
            .types
            .iter()
            .enumerate()
            .all(|(index, definition)| definition.id.0 as usize == index)
    );
    assert!(
        program
            .concepts
            .iter()
            .enumerate()
            .all(|(index, definition)| definition.id.0 as usize == index)
    );
    assert!(
        program
            .requirements
            .iter()
            .enumerate()
            .all(|(index, definition)| definition.id.0 as usize == index)
    );
    assert!(
        program
            .functions
            .iter()
            .enumerate()
            .all(|(index, definition)| definition.id.0 as usize == index)
    );
    assert!(
        program
            .witnesses
            .iter()
            .enumerate()
            .all(|(index, definition)| definition.id.0 as usize == index)
    );
}

const CLOSURE_SOURCE: &str = r"
module executable_closure

import standard.resource.MustScope
import standard.resource.NoSuspend

concept DeadOps {
    method dead(self) Int
}

record DeadMustScope {
    value Int
}

impl DeadOps for DeadMustScope {
    method dead(self) Int {
        self.value
    }
}

impl MustScope for DeadMustScope {}

record DeadNoSuspend {
    value Int
}

impl NoSuspend for DeadNoSuspend {}

concept LiveOps {
    method used(self) Int
    method unused(self) Int
}

record LiveValue {
    value Int
}

impl LiveOps for LiveValue {
    method used(self) Int {
        self.value
    }

    method unused(self) Int {
        self.value + 1
    }
}

fn mainHelper(value LiveValue) Int {
    value.used()
}

fn deadFree(value DeadNoSuspend) Int {
    value.value
}

pub fn main() {
    let value = LiveValue { value = 7 }
    let actual = mainHelper(value)
    assert actual == 7
}
";

#[test]
fn closes_and_densely_remaps_all_global_identity_domains() {
    let program = compile_with_standard_resource(CLOSURE_SOURCE);
    let live_type = program
        .types
        .iter()
        .find(|definition| leaf(&definition.name) == "LiveValue")
        .expect("LiveValue type");
    let live_concept = program
        .concepts
        .iter()
        .find(|definition| definition.name == "LiveOps")
        .expect("LiveOps concept");
    let live_witness = program
        .witnesses
        .iter()
        .find(|witness| witness.concept == live_concept.id)
        .expect("LiveOps witness");
    assert!(live_type.id.0 > 0, "fixture must leave a type-id hole");
    assert!(
        live_concept.id.0 > 0,
        "fixture must leave a concept-id hole"
    );
    assert!(
        live_witness.id.0 > 0,
        "fixture must leave a witness-id hole"
    );
    assert!(
        live_witness
            .methods
            .keys()
            .all(|requirement| requirement.0 > 0),
        "fixture must leave requirement-id holes"
    );

    let closed = close_interpreted_executable(&program, "main").expect("close executable MIR");

    assert_dense_global_ids(&closed);
    assert_eq!(
        closed
            .types
            .iter()
            .map(|definition| leaf(&definition.name))
            .collect::<Vec<_>>(),
        ["LiveValue"]
    );
    assert!(
        !closed
            .concepts
            .iter()
            .any(|concept| concept.name == "DeadOps")
    );
    assert!(
        closed
            .concepts
            .iter()
            .any(|concept| concept.name == "LiveOps")
    );
    assert_eq!(closed.witnesses.len(), 1);
    let witness = &closed.witnesses[0];
    assert_eq!(witness.methods.len(), 2, "retain the complete method table");
    let method_names = witness
        .methods
        .values()
        .map(|function| leaf(&closed.function(*function).expect("method function").name))
        .collect::<BTreeSet<_>>();
    assert_eq!(method_names, BTreeSet::from(["unused", "used"]));
    assert_eq!(
        function_names(&closed),
        BTreeSet::from(["main", "mainHelper", "unused", "used"])
    );
    assert_eq!(
        closed.exports,
        [(
            "main".to_owned(),
            *closed.exports.get("main").expect("main export")
        )]
        .into_iter()
        .collect()
    );
    assert!(closed.tests.is_empty());
    assert!(
        !closed.types.iter().any(|definition| {
            matches!(leaf(&definition.name), "DeadMustScope" | "DeadNoSuspend")
        }),
        "unrelated marker conformances must not keep dead concrete types"
    );

    let bytes =
        encode_interpreted_executable_artifact(&closed, "main").expect("encode closed executable");
    let (decoded, entry) =
        decode_interpreted_executable_artifact(&bytes).expect("decode closed executable");
    assert_eq!(entry, "main");
    assert_dense_global_ids(&decoded);
    assert_eq!(function_names(&decoded), function_names(&closed));
}

#[test]
fn closes_serialized_references_even_after_return() {
    let source = CLOSURE_SOURCE.replace(
        "pub fn main() {\n    let value = LiveValue { value = 7 }\n    let actual = mainHelper(value)\n    assert actual == 7\n}",
        "fn template() Int {\n    LiveValue { value = 7 }.used()\n}\n\npub fn main() {}",
    );
    let checked = compile_with_standard_resource(&source);
    let mut program = checked.as_program().clone();
    let template = program
        .functions
        .iter()
        .find(|function| leaf(&function.name) == "template")
        .cloned()
        .expect("template function");
    let serialized_only = template
        .body
        .tail
        .as_deref()
        .cloned()
        .expect("template call expression");
    assert!(matches!(
        serialized_only.kind,
        ExprKind::Call {
            target: CallTarget::StaticConcept { .. },
            ..
        }
    ));

    let main = program
        .functions
        .iter_mut()
        .find(|function| leaf(&function.name) == "main")
        .expect("main function");
    main.locals = template.locals;
    main.body.statements = vec![
        Statement {
            kind: StatementKind::Return(None),
            span: Span::default(),
        },
        Statement {
            kind: StatementKind::Evaluate(serialized_only),
            span: Span::default(),
        },
    ];
    main.body.tail = None;
    main.renumber_expr_ids().expect("renumber edited main");
    let checked = check_program(program).expect("valid post-return MIR fixture");

    let executable = analyze_source_reachability(
        &checked,
        &SourceRoots::one(*checked.exports.get("main").expect("main export")),
    )
    .expect("analyze executable source graph");
    assert_eq!(executable.functions.len(), 1);
    assert!(executable.witnesses.is_empty());

    let closed = close_interpreted_executable(&checked, "main").expect("close serialized MIR");

    assert_dense_global_ids(&closed);
    assert!(!function_names(&closed).contains("template"));
    assert!(function_names(&closed).contains("main"));
    assert!(function_names(&closed).contains("used"));
    assert!(function_names(&closed).contains("unused"));
    assert!(
        closed
            .types
            .iter()
            .any(|definition| leaf(&definition.name) == "LiveValue")
    );
    let witness = closed.witnesses.first().expect("serialized witness");
    assert_eq!(witness.id, WitnessId(0));
    assert_eq!(witness.methods.len(), 2);

    let bytes = encode_interpreted_executable_artifact(&closed, "main")
        .expect("encode closed post-return executable");
    decode_interpreted_executable_artifact(&bytes).expect("decode closed post-return executable");
}

#[test]
fn preserves_scoped_resource_and_task_metadata() {
    let program = compile_with_standard_resource(
        r"
module resource_task_closure

import standard.resource.Dispose
import standard.resource.MustScope
import standard.resource.NoSuspend

record DeadRecord {
    value Int
}

record Resource {
    value Int
}

impl Dispose for Resource {
    method dispose(mut self) {
        self.value = 0
    }
}

impl MustScope for Resource {}
impl NoSuspend for Resource {}

fn dead(value DeadRecord) Int {
    value.value
}

async fn child() Int {
    Task.sleep(1).await
    7
}

pub async fn main() {
    let value = child().await
    scoped resource = Resource { value = value }
}
",
    );
    let original_main = program
        .functions
        .iter()
        .find(|function| leaf(&function.name) == "main")
        .expect("original main");
    let original_suspensions =
        serde_json::to_value(&original_main.suspension_points).expect("serialize suspensions");

    let closed = close_interpreted_executable(&program, "main").expect("close resource task MIR");

    assert_dense_global_ids(&closed);
    assert_eq!(
        function_names(&closed),
        BTreeSet::from(["child", "dispose", "main"])
    );
    assert!(!closed.types.iter().any(|ty| leaf(&ty.name) == "DeadRecord"));
    assert!(closed.types.iter().any(|ty| leaf(&ty.name) == "Resource"));
    assert_eq!(closed.witnesses.len(), 3);
    let witness_concepts = closed
        .witnesses
        .iter()
        .map(|witness| {
            closed
                .concept(witness.concept)
                .expect("witness concept")
                .name
                .as_str()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        witness_concepts,
        BTreeSet::from(["Dispose", "MustScope", "NoSuspend"])
    );
    let closed_main = closed
        .functions
        .iter()
        .find(|function| leaf(&function.name) == "main")
        .expect("closed main");
    assert!(closed_main.is_async);
    assert_eq!(
        serde_json::to_value(&closed_main.suspension_points).expect("serialize suspensions"),
        original_suspensions
    );

    let bytes = encode_interpreted_executable_artifact(&closed, "main")
        .expect("encode resource task executable");
    let (decoded, _) =
        decode_interpreted_executable_artifact(&bytes).expect("decode resource task executable");
    assert_eq!(function_names(&decoded), function_names(&closed));
}

#[test]
fn rejects_an_unknown_entry_without_mutating_the_program() {
    let program = compile_with_standard_resource("module missing_entry\n\npub fn main() {}\n");
    let before = serde_json::to_value(program.as_program()).expect("serialize fixture");

    let error = close_interpreted_executable(&program, "missing").expect_err("unknown entry");

    assert!(matches!(error, MirClosureError::UnknownEntry { ref entry } if entry == "missing"));
    assert_eq!(
        serde_json::to_value(program.as_program()).expect("serialize input after closure"),
        before
    );
}
