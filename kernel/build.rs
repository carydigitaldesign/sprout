//! Hands the kernel's linker script (one per architecture) to rust-lld and
//! locates the initramfs to embed. `cargo xtask build` builds the user
//! programs, packs them into a cpio archive and sets `SPROUT_INITRAMFS`; a
//! bare `cargo build` falls back to the path xtask would have used and
//! fails clearly if it is missing.

use std::path::{Path, PathBuf};

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").expect("cargo sets CARGO_CFG_TARGET_ARCH");
    let (script, triple) = match arch.as_str() {
        "riscv64" => ("linker-riscv64.ld", "riscv64gc-unknown-none-elf"),
        "x86_64" => ("linker-x86_64.ld", "x86_64-unknown-none"),
        other => panic!("no linker script for target architecture {other}"),
    };
    let script = Path::new(&dir).join(script);
    println!("cargo:rustc-link-arg-bins=-T{}", script.display());
    println!("cargo:rerun-if-changed={}", script.display());

    println!("cargo:rerun-if-env-changed=SPROUT_INITRAMFS");
    let initramfs = match std::env::var("SPROUT_INITRAMFS") {
        Ok(p) => PathBuf::from(p),
        Err(_) => Path::new(&dir).join(format!("../target/user/{triple}/debug/initramfs.cpio")),
    };
    if !initramfs.exists() {
        panic!(
            "initramfs not found at {}; build with `cargo xtask build`, which builds user/ and packs it first",
            initramfs.display()
        );
    }
    println!("cargo:rustc-env=SPROUT_INITRAMFS={}", initramfs.display());
    println!("cargo:rerun-if-changed={}", initramfs.display());
}
