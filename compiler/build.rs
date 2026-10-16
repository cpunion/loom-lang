fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // The bridge walks bounded checked-expression trees and runs LLVM.
        // Use the same stack reserve as the native Loom compiler executable.
        println!("cargo:rustc-link-arg-bin=loom-native=/STACK:33554432");
    }
}
