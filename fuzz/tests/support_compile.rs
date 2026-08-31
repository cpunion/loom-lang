#[path = "../fuzz_targets/support.rs"]
mod support;

#[test]
fn structured_standard_source_graph_compiles() {
    support::compile(support::STRUCTURED_BUILTIN_SOURCE)
        .expect("structured fuzz standard-library source graph must compile");
}
