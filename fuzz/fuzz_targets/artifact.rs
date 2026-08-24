#![no_main]

use std::collections::BTreeMap;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use loom_mir::{
    Block, CallPlan, Constant, Expr, ExprKind, Function, FunctionId, Program, Type,
    decode_interpreted_artifact, decode_interpreted_executable_artifact,
    encode_interpreted_executable_artifact,
};

fuzz_target!(|input: &[u8]| {
    // Raw bytes exercise envelope/nesting/header rejection. A mutation of a
    // valid seed reaches serde reconstruction, Float restoration, entry
    // validation, and the complete checked-MIR validator much more often.
    let _ = decode_interpreted_artifact(input);
    let _ = decode_interpreted_executable_artifact(input);

    let seed = valid_seed();
    let mut mutated = seed.clone();
    for mutation in input.chunks_exact(3).take(64) {
        let index = (usize::from(mutation[0]) << 8 | usize::from(mutation[1])) % mutated.len();
        mutated[index] ^= mutation[2];
    }
    let _ = decode_interpreted_artifact(&mutated);
    let _ = decode_interpreted_executable_artifact(&mutated);
});

fn valid_seed() -> &'static Vec<u8> {
    static SEED: OnceLock<Vec<u8>> = OnceLock::new();
    SEED.get_or_init(|| {
        let mut program = Program::default();
        program.functions.push(Function {
            id: FunctionId(0),
            name: "fuzz.main".into(),
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
        encode_interpreted_executable_artifact(&program, "main").expect("valid fuzz seed")
    })
}
