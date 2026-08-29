#![no_main]

use std::sync::OnceLock;

mod support;

use libfuzzer_sys::fuzz_target;
use loom_mir::{
    decode_interpreted_artifact, decode_interpreted_executable_artifact,
    encode_interpreted_executable_artifact,
};

use support::{STRUCTURED_BUILTIN_SOURCE, compile};

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
        let program = compile(STRUCTURED_BUILTIN_SOURCE)
            .unwrap_or_else(|error| panic!("structured fuzz seed must compile: {error}"));
        assert!(program.prelude.text_map.is_some());
        assert!(program.prelude.json.is_some());
        assert!(program.prelude.io_error.is_some());
        assert!(program.prelude.log_level.is_some());
        encode_interpreted_executable_artifact(&program, "main").expect("valid fuzz seed")
    })
}
