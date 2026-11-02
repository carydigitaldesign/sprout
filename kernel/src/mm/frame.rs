//! Physical frame allocator: one bit per 4 KiB frame.
//!
//! Manages one contiguous range of RAM, normally everything after the
//! kernel image. The bitmap itself lives in the first frames of that range
//! and is marked allocated. A bump pointer (`next`) makes the common case
//! O(1); frees reset it so freed frames are reused.
//!
//! Buddy allocation (PLAN.md M2) waits until something needs large
//! contiguous runs often; `alloc_contiguous` scans linearly for now.

use crate::arch::{PAGE_SIZE, phys_to_virt};
use crate::sync::SpinLock;

/// A 4 KiB physical frame, identified by its physical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame(usize);

impl Frame {
    /// Reconstructs a frame from a physical address recorded earlier, for
    /// code that stores frames as raw addresses (page table entries).
    pub(crate) const fn from_pa(pa: usize) -> Frame {
        Frame(pa)
    }

    /// Physical address of the first byte.
    pub fn pa(self) -> usize {
        self.0
    }

    /// Kernel-accessible view of the frame through the direct map.
    pub fn as_bytes_mut(self) -> &'static mut [u8] {
        // SAFETY: the frame is RAM mapped in the direct map, and whoever
        // holds a `Frame` owns its contents until they free it.
        unsafe { core::slice::from_raw_parts_mut(phys_to_virt(self.0) as *mut u8, PAGE_SIZE) }
    }
}

/// Totals for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub total: usize,
    pub free: usize,
}

struct Allocator {
    bitmap: &'static mut [u64],
    base: usize,
    frames: usize,
    free: usize,
    next: usize,
}

static ALLOCATOR: SpinLock<Option<Allocator>> = SpinLock::new(None);

impl Allocator {
    fn is_used(&self, index: usize) -> bool {
        self.bitmap[index / 64] & (1 << (index % 64)) != 0
    }

    fn set_used(&mut self, index: usize) {
        self.bitmap[index / 64] |= 1 << (index % 64);
    }

    fn clear_used(&mut self, index: usize) {
        self.bitmap[index / 64] &= !(1 << (index % 64));
    }

    fn frame(&self, index: usize) -> Frame {
        Frame(self.base + index * PAGE_SIZE)
    }

    fn index(&self, frame: Frame) -> Option<usize> {
        if frame.0 < self.base || frame.0 % PAGE_SIZE != 0 {
            return None;
        }
        let index = (frame.0 - self.base) / PAGE_SIZE;
        (index < self.frames).then_some(index)
    }

    /// First run of `count` free frames at or after `self.next`, then from 0.
    fn find_run(&self, count: usize) -> Option<usize> {
        let search = |from: usize, to: usize| -> Option<usize> {
            let mut run_start = from;
            let mut i = from;
            while i < to {
                if self.is_used(i) {
                    run_start = i + 1;
                } else if i + 1 - run_start == count {
                    return Some(run_start);
                }
                i += 1;
            }
            None
        };
        search(self.next, self.frames).or_else(|| search(0, self.frames))
    }

    fn take(&mut self, start: usize, count: usize) {
        for i in start..start + count {
            self.set_used(i);
        }
        self.free -= count;
        self.next = start + count;
    }
}

/// Starts managing `[start, end)`. `reserved` ranges inside it (the device
/// tree blob, for one) are marked used and never handed out.
///
/// # Panics
/// If called twice, or the range is too small to hold its own bitmap.
pub fn init(start: usize, end: usize, reserved: &[(usize, usize)]) {
    let start = start.next_multiple_of(PAGE_SIZE);
    let end = end & !(PAGE_SIZE - 1);
    let frames = (end - start) / PAGE_SIZE;
    let bitmap_words = frames.div_ceil(64);
    let bitmap_frames = (bitmap_words * 8).div_ceil(PAGE_SIZE);
    assert!(frames > bitmap_frames, "not enough RAM to manage");

    // SAFETY: the bitmap occupies the first frames of a RAM range that is
    // ours alone from here on, reached through the direct map; it is
    // initialised below before anything reads it.
    let bitmap =
        unsafe { core::slice::from_raw_parts_mut(phys_to_virt(start) as *mut u64, bitmap_words) };
    bitmap.fill(0);

    let mut alloc = Allocator {
        bitmap,
        base: start,
        frames,
        free: frames,
        next: 0,
    };
    alloc.take(0, bitmap_frames);
    for &(lo, hi) in reserved {
        let lo = lo.max(start) & !(PAGE_SIZE - 1);
        let hi = hi.min(end).next_multiple_of(PAGE_SIZE);
        for pa in (lo..hi).step_by(PAGE_SIZE) {
            if let Some(i) = alloc.index(Frame(pa))
                && !alloc.is_used(i)
            {
                alloc.set_used(i);
                alloc.free -= 1;
            }
        }
    }

    let mut slot = ALLOCATOR.lock();
    assert!(slot.is_none(), "frame allocator initialised twice");
    *slot = Some(alloc);
}

fn with<R>(f: impl FnOnce(&mut Allocator) -> R) -> R {
    let mut slot = ALLOCATOR.lock();
    f(slot.as_mut().expect("frame allocator used before init"))
}

/// One frame, contents undefined.
pub fn alloc() -> Option<Frame> {
    alloc_contiguous(1)
}

/// One frame, filled with zeros.
pub fn alloc_zeroed() -> Option<Frame> {
    let frame = alloc()?;
    frame.as_bytes_mut().fill(0);
    Some(frame)
}

/// `count` physically contiguous frames; returns the first.
pub fn alloc_contiguous(count: usize) -> Option<Frame> {
    with(|a| {
        let start = a.find_run(count)?;
        a.take(start, count);
        Some(a.frame(start))
    })
}

/// Returns a frame. Freeing a frame twice or one that was never allocated
/// is a bug and panics.
pub fn free(frame: Frame) {
    with(|a| {
        let i = a
            .index(frame)
            .expect("freeing a frame outside the managed range");
        assert!(a.is_used(i), "double free of frame {:#x}", frame.0);
        a.clear_used(i);
        a.free += 1;
        a.next = a.next.min(i);
    });
}

/// Frees `count` frames starting at `first`, the inverse of `alloc_contiguous`.
pub fn free_contiguous(first: Frame, count: usize) {
    for i in 0..count {
        free(Frame(first.0 + i * PAGE_SIZE));
    }
}

pub fn stats() -> Stats {
    with(|a| Stats {
        total: a.frames,
        free: a.free,
    })
}

crate::ktest! {
    fn frames_are_distinct_aligned_and_returned() {
        let before = stats().free;
        let a = alloc().expect("frame");
        let b = alloc().expect("frame");
        assert_ne!(a, b);
        assert_eq!(a.pa() % PAGE_SIZE, 0);
        assert_eq!(stats().free, before - 2);
        free(a);
        free(b);
        assert_eq!(stats().free, before);
    }
}

crate::ktest! {
    fn zeroed_frame_is_zero() {
        let f = alloc().expect("frame");
        f.as_bytes_mut().fill(0xAA);
        free(f);
        let z = alloc_zeroed().expect("frame");
        assert!(z.as_bytes_mut().iter().all(|&b| b == 0));
        free(z);
    }
}

crate::ktest! {
    fn contiguous_run_is_contiguous() {
        let before = stats().free;
        let first = alloc_contiguous(4).expect("4 frames");
        // Every frame of the run is now taken, so allocating again must
        // not hand out any of them.
        let other = alloc().expect("frame");
        assert!(other.pa() < first.pa() || other.pa() >= first.pa() + 4 * PAGE_SIZE);
        free(other);
        free_contiguous(first, 4);
        assert_eq!(stats().free, before);
    }
}
