//! The initramfs: a `newc` cpio archive of the user programs, built by
//! `cargo xtask build` and embedded here until a disk can carry it. The
//! kernel pulls `init` out of it and hands the whole archive to `init`
//! as a VMO; `fsd` serves the rest.

/// The archive, exactly as `tools/xtask` wrote it.
pub static INITRAMFS: &[u8] = include_bytes!(env!("SPROUT_INITRAMFS"));

crate::ktest! {
    fn initramfs_holds_every_program() {
        for name in ["init", "shell", "fsd", "blkd"] {
            let data = sprout_abi::cpio::find(INITRAMFS, name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(&data[..4], b"\x7fELF", "{name} is an ELF");
        }
        assert!(sprout_abi::cpio::find(INITRAMFS, "nope").is_none());
    }
}
