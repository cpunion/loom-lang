use std::fmt;

use loom_codegen_ir::{
    ARTIFACT_IDENTITY_ROUTE, ARTIFACT_IDENTITY_SCHEMA, ArtifactRootRequest, CheckedArtifact,
    CheckedProgram, Constant, Effects, FloatBinaryOp, InstructionKind, Origin, ProgramBuilder,
    Signature, TargetLayout, Terminator, TerminatorKind, artifact_identity,
    write_artifact_identity,
};
use loom_core::{FileId, Span};
use loom_mir::{ExprId, FunctionId as MirFunctionId, Type};

#[derive(Clone, Copy)]
struct BodyOrigins {
    function: Origin,
    instruction: Origin,
    terminator: Origin,
}

impl BodyOrigins {
    fn base() -> Self {
        Self {
            function: origin(None, 1, 10, 20),
            instruction: origin(Some(7), 1, 21, 30),
            terminator: origin(Some(8), 1, 31, 40),
        }
    }
}

fn origin(expression: Option<u32>, file: u32, start: u32, end: u32) -> Origin {
    Origin {
        source_function: MirFunctionId(41),
        expression: expression.map(ExprId),
        span: Span::new(FileId(file), start, end),
    }
}

fn scalar_artifact(
    pointer_bits: u16,
    origins: BodyOrigins,
    operation: FloatBinaryOp,
    left_bits: u64,
) -> CheckedArtifact {
    let mut builder =
        ProgramBuilder::new(TargetLayout::new(pointer_bits).expect("test target layout"));
    let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
    let float_ty = builder.type_id(&Type::Float).expect("Float type");
    let root = builder
        .declare_function(
            origins.function,
            "identity\nroot",
            Signature::new(Vec::new(), unit_ty),
            Effects::NONE,
        )
        .expect("declare root");
    {
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let left = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::FloatBits(left_bits)),
                &[float_ty],
                origins.instruction,
            )
            .expect("left constant")[0];
        let right = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::float(2.0)),
                &[float_ty],
                origins.instruction,
            )
            .expect("right constant")[0];
        function
            .append_instruction(
                entry,
                InstructionKind::FloatBinary {
                    op: operation,
                    left,
                    right,
                },
                &[float_ty],
                origins.instruction,
            )
            .expect("floating operation");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                origins.instruction,
            )
            .expect("Unit constant")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(unit), origins.terminator),
            )
            .expect("return");
    }
    builder
        .finish_checked()
        .expect("valid scalar LCIR")
        .into_artifact(ArtifactRootRequest::Run(root))
        .expect("closed run artifact")
}

fn unit_roots(names: &[&str]) -> (CheckedProgram, Vec<loom_codegen_ir::InstanceId>) {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
    let mut roots = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let function_origin = Origin::synthetic(MirFunctionId(
            u32::try_from(index).expect("test source index"),
        ));
        let root = builder
            .declare_function(
                function_origin,
                *name,
                Signature::new(Vec::new(), unit_ty),
                Effects::NONE,
            )
            .expect("declare root");
        let mut function = builder.function(root).expect("root builder");
        let entry = function.create_block().expect("entry");
        function.set_entry(entry).expect("set entry");
        let unit = function
            .append_instruction(
                entry,
                InstructionKind::Constant(Constant::Unit),
                &[unit_ty],
                function_origin,
            )
            .expect("Unit constant")[0];
        function
            .terminate(
                entry,
                Terminator::new(TerminatorKind::Return(unit), function_origin),
            )
            .expect("return");
        roots.push(root);
    }
    (builder.finish_checked().expect("valid roots"), roots)
}

#[test]
fn identity_is_brand_independent_and_repeatable() {
    let first = scalar_artifact(
        64,
        BodyOrigins::base(),
        FloatBinaryOp::Add,
        1.0_f64.to_bits(),
    );
    let second = scalar_artifact(
        64,
        BodyOrigins::base(),
        FloatBinaryOp::Add,
        1.0_f64.to_bits(),
    );

    assert_ne!(first.run_root(), second.run_root());
    let identity = artifact_identity(&first);
    assert_eq!(identity, artifact_identity(&second));
    assert_eq!(identity, artifact_identity(&first));
    assert!(identity.starts_with(&format!(
        "loom-checked-artifact-identity\nschema={ARTIFACT_IDENTITY_SCHEMA}\nroute={ARTIFACT_IDENTITY_ROUTE}\n"
    )));
    assert!(!identity.contains("ProgramBrand"));
    assert!(!identity.contains("function-origin f41/e7 file1:10..20"));
    assert!(identity.contains("function-origin f41 file1:10..20"));
    assert!(identity.contains("origin f41/e7 file1:21..30"));
    assert!(identity.contains("origin f41/e8 file1:31..40"));

    let mut written = String::new();
    write_artifact_identity(&first, &mut written).expect("String writer is infallible");
    assert_eq!(identity, written);
}

#[test]
fn kind_and_ordered_test_roots_are_identity_inputs() {
    let (single, roots) = unit_roots(&["only"]);
    let run = single
        .clone()
        .into_artifact(ArtifactRootRequest::Run(roots[0]))
        .expect("run artifact");
    let tests = single
        .into_artifact(ArtifactRootRequest::tests([roots[0]]))
        .expect("test artifact");
    assert_ne!(artifact_identity(&run), artifact_identity(&tests));

    let (multiple, roots) = unit_roots(&["first", "second"]);
    let forward = multiple
        .clone()
        .into_artifact(ArtifactRootRequest::tests([roots[0], roots[1]]))
        .expect("forward roots");
    let reverse = multiple
        .into_artifact(ArtifactRootRequest::tests([roots[1], roots[0]]))
        .expect("reverse roots");
    assert_ne!(artifact_identity(&forward), artifact_identity(&reverse));
}

#[test]
fn target_body_and_operation_changes_invalidate_identity() {
    let identity = |pointer_bits, operation, bits| {
        artifact_identity(&scalar_artifact(
            pointer_bits,
            BodyOrigins::base(),
            operation,
            bits,
        ))
    };
    let base = identity(64, FloatBinaryOp::Add, 1.0_f64.to_bits());

    assert_ne!(base, identity(32, FloatBinaryOp::Add, 1.0_f64.to_bits()));
    assert_ne!(base, identity(64, FloatBinaryOp::Add, 3.0_f64.to_bits()));
    assert_ne!(
        base,
        identity(64, FloatBinaryOp::Subtract, 1.0_f64.to_bits())
    );
}

#[test]
fn function_instruction_and_terminator_origins_invalidate_identity() {
    let base_origins = BodyOrigins::base();
    let encode = |origins| {
        artifact_identity(&scalar_artifact(
            64,
            origins,
            FloatBinaryOp::Add,
            1.0_f64.to_bits(),
        ))
    };
    let base = encode(base_origins);
    let variants = [
        BodyOrigins {
            function: origin(Some(1), 1, 10, 20),
            ..base_origins
        },
        BodyOrigins {
            instruction: origin(Some(7), 2, 21, 30),
            ..base_origins
        },
        BodyOrigins {
            terminator: origin(Some(8), 1, 31, 41),
            ..base_origins
        },
    ];

    for variant in variants {
        assert_ne!(base, encode(variant));
    }
}

#[test]
fn writer_propagates_destination_formatting_failure() {
    struct FailingWriter;

    impl fmt::Write for FailingWriter {
        fn write_str(&mut self, _value: &str) -> fmt::Result {
            Err(fmt::Error)
        }
    }

    let artifact = scalar_artifact(
        64,
        BodyOrigins::base(),
        FloatBinaryOp::Add,
        1.0_f64.to_bits(),
    );
    assert!(write_artifact_identity(&artifact, &mut FailingWriter).is_err());
}
