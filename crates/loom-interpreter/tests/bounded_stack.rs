use loom_core::Span;
use loom_interpreter::{ExecutionFailure, ExecutionLimits, Interpreter};
use loom_mir::{
    Block, CallPlan, CallTarget, Constant, Expr, ExprKind, Function, FunctionId, MirValidationCode,
    Program, Type,
};

const ONE_MIB: usize = 1024 * 1024;
const LANGUAGE_CALL_DEPTH: u32 = 256;

fn span() -> Span {
    Span::default()
}

fn unit() -> Expr {
    Expr::new(ExprKind::Constant(Constant::Unit), Type::Unit, span())
}

fn function(id: u32, tail: Expr) -> Function {
    Function {
        id: FunctionId(id),
        name: format!("call{id}"),
        span: span(),
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
            tail: Some(Box::new(tail)),
            span: span(),
        },
        call_plan: CallPlan::default(),
    }
}

fn checked(mut program: Program) -> loom_mir::CheckedProgram {
    program
        .renumber_expr_ids()
        .expect("renumber bounded-stack MIR fixture");
    program
        .into_checked()
        .expect("bounded-stack MIR fixture must validate")
}

fn synchronous_call_chain(function_count: u32) -> loom_mir::CheckedProgram {
    let functions = (0..function_count)
        .map(|id| {
            let tail = if id + 1 == function_count {
                unit()
            } else {
                Expr::new(
                    ExprKind::Call {
                        target: CallTarget::Direct(FunctionId(id + 1)),
                        type_arguments: Vec::new(),
                        arguments: Vec::new(),
                        witnesses: Vec::new(),
                    },
                    Type::Unit,
                    span(),
                )
            };
            function(id, tail)
        })
        .collect();
    checked(Program {
        functions,
        ..Program::default()
    })
}

#[test]
fn synchronous_calls_hit_the_language_limit_on_a_one_mib_stack() {
    // The root occupies one logical call level. Function 255 therefore runs
    // at the configured limit and its call to function 256 must be rejected
    // through Loom's normal runtime-failure channel. Host-stack exhaustion is
    // never an acceptable implementation of this language limit.
    let program = synchronous_call_chain(LANGUAGE_CALL_DEPTH + 1);
    let outcome = std::thread::Builder::new()
        .name("loom-one-mib-call-depth".into())
        .stack_size(ONE_MIB)
        .spawn(move || {
            Interpreter::with_limits(
                &program,
                ExecutionLimits {
                    max_call_depth: LANGUAGE_CALL_DEPTH,
                    ..ExecutionLimits::default()
                },
            )
            .invoke(FunctionId(0), Vec::new(), span())
        })
        .expect("spawn interpreter on a Windows-sized stack")
        .join()
        .expect("interpreter must not overflow the host stack");

    let failure = outcome.expect_err("logical call-depth exhaustion must fail");
    assert!(
        matches!(
            &failure,
            ExecutionFailure::Runtime { fault }
                if fault.code == "LOOM_RUNTIME_CALL_DEPTH"
                    && fault.message == "call depth limit exceeded"
        ),
        "unexpected depth failure: {failure:?}"
    );
}

#[test]
fn unchecked_deep_expressions_stop_at_the_mir_boundary() {
    // Source nesting has its own public limit and lowering balances long
    // logical chains. Independently supplied MIR must not bypass the checked
    // boundary and turn an arbitrarily deep expression into interpreter work.
    let outcome = std::thread::Builder::new()
        .name("loom-one-mib-expression-validation".into())
        .stack_size(ONE_MIB)
        .spawn(|| {
            let mut expression = unit();
            for _ in 0..96 {
                expression = Expr::new(
                    ExprKind::Block(Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(expression)),
                        span: span(),
                    }),
                    Type::Unit,
                    span(),
                );
            }
            let mut program = Program {
                functions: vec![function(0, expression)],
                ..Program::default()
            };
            program
                .renumber_expr_ids()
                .expect("renumber adversarial deep expression");
            program
                .into_checked()
                .expect_err("deep unchecked MIR must not reach the interpreter")
        })
        .expect("spawn validator on a Windows-sized stack")
        .join()
        .expect("MIR validation must remain host-stack bounded");

    assert!(
        outcome.contains(MirValidationCode::NestingLimit),
        "missing MIR nesting diagnostic: {outcome}"
    );
}
