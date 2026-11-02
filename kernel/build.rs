//! Hands the kernel's linker script to rust-lld.

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let script = std::path::Path::new(&dir).join("linker.ld");
    println!("cargo:rustc-link-arg-bins=-T{}", script.display());
    println!("cargo:rerun-if-changed=linker.ld");
}
