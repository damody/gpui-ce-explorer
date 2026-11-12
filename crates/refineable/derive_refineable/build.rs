fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Rust loads proc-macro DLLs directly, without an import library or exports file.
        // Avoid MSVC's artifact creation messages while retaining real linker diagnostics.
        println!("cargo:rustc-link-arg=/NOIMPLIB");
        println!("cargo:rustc-link-arg=/NOEXP");
    }
}
