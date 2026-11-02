//! The kernel heap behind `alloc`: `Box`, `Vec`, `String` and friends.
//!
//! A first-fit free-list allocator (`linked_list_allocator`, ADR-0006) over
//! a fixed run of physically contiguous frames reached through the direct
//! map. Growing on demand and a slab for small objects can come when
//! something needs them.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;

use linked_list_allocator::Heap;

use crate::arch::{PAGE_SIZE, phys_to_virt};
use crate::mm::frame;
use crate::sync::SpinLock;

/// 4 MiB: process images are copied through the heap when spawning.
pub const DEFAULT_FRAMES: usize = 1024;

struct KernelHeap(SpinLock<Heap>);

#[global_allocator]
static HEAP: KernelHeap = KernelHeap(SpinLock::new(Heap::empty()));

// SAFETY: `Heap` never hands out overlapping blocks, the spinlock keeps
// concurrent callers apart, and `dealloc` only returns blocks that `alloc`
// produced, per the `GlobalAlloc` contract.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0
            .lock()
            .allocate_first_fit(layout)
            .map(NonNull::as_ptr)
            .unwrap_or(core::ptr::null_mut())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `GlobalAlloc` requires `ptr` to come from `alloc` with this
        // `layout`, so it is non-null and the heap recognises it.
        unsafe {
            self.0
                .lock()
                .deallocate(NonNull::new_unchecked(ptr), layout)
        };
    }
}

/// Gives the heap its memory. Call once, after the frame allocator is up.
pub fn init(frames: usize) {
    let first = frame::alloc_contiguous(frames).expect("no memory for the kernel heap");
    let bottom = phys_to_virt(first.pa()) as *mut u8;
    // SAFETY: the frames are ours, contiguous, mapped read-write in the
    // direct map, and belong to the heap from now on.
    unsafe { HEAP.0.lock().init(bottom, frames * PAGE_SIZE) };
}

/// Bytes in use and bytes free.
pub fn stats() -> (usize, usize) {
    let heap = HEAP.0.lock();
    (heap.used(), heap.free())
}

crate::ktest! {
    fn boxes_vectors_and_strings_work() {
        use alloc::boxed::Box;
        use alloc::string::String;
        use alloc::vec::Vec;

        let used_before = stats().0;
        {
            let b = Box::new(42u64);
            let v: Vec<u64> = (0..1000).collect();
            let mut s = String::from("sprout");
            s.push_str(" heap");
            assert_eq!(*b, 42);
            assert_eq!(v.iter().sum::<u64>(), 499_500);
            assert_eq!(s, "sprout heap");
        }
        assert_eq!(stats().0, used_before, "heap memory leaked");
    }
}

crate::ktest! {
    fn large_and_aligned_allocations() {
        use alloc::alloc::{alloc, dealloc};
        let layout = Layout::from_size_align(64, 4096).expect("layout");
        // SAFETY: a valid non-zero-size layout; freed below with the same one.
        let p = unsafe { alloc(layout) };
        assert!(!p.is_null());
        assert_eq!(p as usize % 4096, 0);
        // SAFETY: `p` came from `alloc` with `layout`.
        unsafe { dealloc(p, layout) };

        let big = alloc::vec![0u8; 256 * 1024];
        assert_eq!(big.len(), 256 * 1024);
    }
}
