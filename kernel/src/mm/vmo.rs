//! Virtual memory objects: a sized bag of physical pages (PLAN.md 2.1).
//!
//! At M2 a `Vmo` is a plain struct the kernel uses directly. M4 wraps it in
//! a kernel object reachable through a handle, and M5 maps it into address
//! spaces; its pages are already addressable for that through [`Vmo::frame`].

use alloc::vec::Vec;

use crate::arch::PAGE_SIZE;
use crate::mm::frame::{self, Frame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmoError {
    OutOfMemory,
    OutOfRange,
}

/// Zero-filled pages, allocated up front.
pub struct Vmo {
    frames: Vec<Frame>,
}

impl Vmo {
    /// A VMO of at least `size` bytes, rounded up to whole pages.
    pub fn new(size: usize) -> Result<Vmo, VmoError> {
        let mut vmo = Vmo { frames: Vec::new() };
        vmo.resize(size)?;
        Ok(vmo)
    }

    /// Size in bytes, always a multiple of the page size.
    pub fn size(&self) -> usize {
        self.frames.len() * PAGE_SIZE
    }

    /// The frame backing page `index`. Used from M5 when VMOs are mapped.
    #[allow(dead_code)]
    pub fn frame(&self, index: usize) -> Option<Frame> {
        self.frames.get(index).copied()
    }

    /// Grows with zeroed pages or shrinks by freeing the tail.
    pub fn resize(&mut self, size: usize) -> Result<(), VmoError> {
        let want = size.div_ceil(PAGE_SIZE);
        if want < self.frames.len() {
            for f in self.frames.drain(want..) {
                frame::free(f);
            }
            return Ok(());
        }
        let extra = want - self.frames.len();
        if self.frames.try_reserve(extra).is_err() {
            return Err(VmoError::OutOfMemory);
        }
        let mut added = 0;
        while added < extra {
            match frame::alloc_zeroed() {
                Some(f) => {
                    self.frames.push(f);
                    added += 1;
                }
                None => {
                    let keep = self.frames.len() - added;
                    for f in self.frames.drain(keep..) {
                        frame::free(f);
                    }
                    return Err(VmoError::OutOfMemory);
                }
            }
        }
        Ok(())
    }

    /// Visits the page slices covering `[offset, offset + len)`, with the
    /// offset into the caller's buffer each one corresponds to.
    fn pages(
        &self,
        offset: usize,
        len: usize,
        mut f: impl FnMut(&mut [u8], usize),
    ) -> Result<(), VmoError> {
        if offset.checked_add(len).is_none_or(|end| end > self.size()) {
            return Err(VmoError::OutOfRange);
        }
        let mut done = 0;
        while done < len {
            let at = offset + done;
            let in_page = at % PAGE_SIZE;
            let chunk = (PAGE_SIZE - in_page).min(len - done);
            let page = self.frames[at / PAGE_SIZE].as_bytes_mut();
            f(&mut page[in_page..in_page + chunk], done);
            done += chunk;
        }
        Ok(())
    }

    /// Copies `buf.len()` bytes out, starting at `offset`.
    pub fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), VmoError> {
        self.pages(offset, buf.len(), |page, at| {
            buf[at..at + page.len()].copy_from_slice(page);
        })
    }

    /// Copies `buf` in, starting at `offset`.
    pub fn write(&mut self, offset: usize, buf: &[u8]) -> Result<(), VmoError> {
        self.pages(offset, buf.len(), |page, at| {
            page.copy_from_slice(&buf[at..at + page.len()]);
        })
    }
}

impl Drop for Vmo {
    fn drop(&mut self) {
        for f in self.frames.drain(..) {
            frame::free(f);
        }
    }
}

crate::ktest! {
    fn vmo_round_trips_across_a_page_boundary() {
        let free_before = frame::stats().free;
        {
            let mut vmo = Vmo::new(3 * PAGE_SIZE).expect("vmo");
            assert_eq!(vmo.size(), 3 * PAGE_SIZE);
            let pattern: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
            vmo.write(PAGE_SIZE - 100, &pattern).expect("write");
            let mut back = alloc::vec![0u8; 300];
            vmo.read(PAGE_SIZE - 100, &mut back).expect("read");
            assert_eq!(back, pattern);
            let mut untouched = [0xFFu8; 16];
            vmo.read(0, &mut untouched).expect("read");
            assert_eq!(untouched, [0u8; 16], "new pages are zeroed");
            assert_eq!(frame::stats().free, free_before - 3);
        }
        assert_eq!(frame::stats().free, free_before, "drop returns the frames");
    }
}

crate::ktest! {
    fn vmo_resize_and_bounds() {
        let free_before = frame::stats().free;
        let mut vmo = Vmo::new(1).expect("vmo");
        assert_eq!(vmo.size(), PAGE_SIZE);
        vmo.write(0, b"keep").expect("write");
        vmo.resize(2 * PAGE_SIZE + 1).expect("grow");
        assert_eq!(vmo.size(), 3 * PAGE_SIZE);
        let mut zeros = [1u8; 8];
        vmo.read(2 * PAGE_SIZE, &mut zeros).expect("read");
        assert_eq!(zeros, [0u8; 8]);
        vmo.resize(PAGE_SIZE).expect("shrink");
        let mut kept = [0u8; 4];
        vmo.read(0, &mut kept).expect("read");
        assert_eq!(&kept, b"keep");
        assert_eq!(vmo.read(PAGE_SIZE - 2, &mut kept), Err(VmoError::OutOfRange));
        assert_eq!(vmo.write(usize::MAX, b"x"), Err(VmoError::OutOfRange));
        assert_eq!(frame::stats().free, free_before - 1);
    }
}
