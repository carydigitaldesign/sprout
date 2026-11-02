//! Hands the kernel's linker script to rust-lld and locates the initramfs
//! to embed. `cargo xtask build` builds the user programs, packs them into
//! a cpio archive and sets `SPROUT_INITRAMFS`; a bare `cargo build` falls
//! back to the path xtask would have used and fails clearly if it is
//! missing.

use std::path::{Path, PathBuf};

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let script = Path::new(&dir).join("linker.ld");
    println!("cargo:rustc-link-arg-bins=-T{}", script.display());
    println!("cargo:rerun-if-changed=linker.ld");

    println!("cargo:rerun-if-env-changed=SPROUT_INITRAMFS");
    let initramfs = match std::env::var("SPROUT_INITRAMFS") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            Path::new(&dir).join("../target/user/riscv64gc-unknown-none-elf/debug/initramfs.cpio")
        }
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
