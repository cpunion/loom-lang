fn main() {
    if let Err(error) = loom_lsp::run_stdio() {
        eprintln!("loom-lsp: {error}");
        std::process::exit(3);
    }
}
