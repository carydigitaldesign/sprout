//! User programs embedded in the kernel image, until an initramfs (M6)
//! carries them instead. `cargo xtask build` builds `user/init` first and
//! hands its path to `kernel/build.rs`.

/// The first user program.
pub static INIT_ELF: &[u8] = include_bytes!(env!("SPROUT_INIT_ELF"));
