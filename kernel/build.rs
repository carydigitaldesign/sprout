//! Hands the kernel's linker script to rust-lld and locates the `init`
//! program to embed. `cargo xtask build` builds `init` first and sets
//! `SPROUT_INIT_ELF`; a bare `cargo build` falls back to the path xtask
//! would have used and fails clearly if it is missing.

use std::path::{Path, PathBuf};

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let script = Path::new(&dir).join("linker.ld");
    println!("cargo:rustc-link-arg-bins=-T{}", script.display());
    println!("cargo:rerun-if-changed=linker.ld");

    println!("cargo:rerun-if-env-changed=SPROUT_INIT_ELF");
    let init = match std::env::var("SPROUT_INIT_ELF") {
        Ok(p) => PathBuf::from(p),
        Err(_) => Path::new(&dir).join("../target/user/riscv64gc-unknown-none-elf/debug/init"),
    };
    if !init.exists() {
        panic!(
            "init program not found at {}; build with `cargo xtask build`, which builds user/init first",
            init.display()
        );
    }
    println!("cargo:rustc-env=SPROUT_INIT_ELF={}", init.display());
    println!("cargo:rerun-if-changed={}", init.display());
}
