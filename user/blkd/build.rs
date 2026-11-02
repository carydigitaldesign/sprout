//! Links the program with the shared user layout from `sprout-rt`.

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let script = std::path::Path::new(&dir).join("../../lib/sprout-rt/user.ld");
    println!("cargo:rustc-link-arg-bins=-T{}", script.display());
    println!("cargo:rerun-if-changed={}", script.display());
}
