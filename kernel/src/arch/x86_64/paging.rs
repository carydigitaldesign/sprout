//! Four-level x86_64 page tables: 4 KiB and 2 MiB pages (1 GiB pages are
//! optional hardware and not used), 48-bit virtual addresses, NX.
//!
//! The kernel image lives in the top 2 GiB (`IMAGE_OFFSET`, the "kernel"
//! code model) and reaches every physical address through the direct map
//! at `PHYS_MAP_BASE`. Page tables are ordinary frames from the frame
//! allocator, read and written through that map.
//!
//! User address spaces share the kernel's upper half by copying the
//! PML4's upper entries, so the kernel is mapped (supervisor only) in
//! every process and traps need no table switch.

use crate::mm::frame::{self, Frame};
use crate::mm::{MapError, MapFlags, PageSize};

/// Smallest page.
pub const PAGE_SIZE: usize = 4096;

/// Physical address `p` is mapped read-write at `PHYS_MAP_BASE + p` for
/// all RAM.
pub const PHYS_MAP_BASE: usize = 0xffff_ffc0_0000_0000;

/// The kernel image's virtual address is its physical address plus this.
pub const IMAGE_OFFSET: usize = 0xffff_ffff_8000_0000;

/// Direct-map address of a physical address.
pub const fn phys_to_virt(pa: usize) -> usize {
    PHYS_MAP_BASE + pa
}

/// Physical address behind a kernel-image or direct-map address.
pub fn virt_to_phys(va: usize) -> usize {
    if va >= IMAGE_OFFSET {
        va - IMAGE_OFFSET
    } else {
        debug_assert!(va >= PHYS_MAP_BASE, "{va:#x} is not a kernel address");
        va - PHYS_MAP_BASE
    }
}

const ENTRIES: usize = 512;
const LEVELS: usize = 4;
/// Top-level entries from here up are the kernel's half.
const KERNEL_HALF_START: usize = ENTRIES / 2;

const PTE_P: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_US: u64 = 1 << 2;
const PTE_A: u64 = 1 << 5;
const PTE_D: u64 = 1 << 6;
const PTE_PS: u64 = 1 << 7;
const PTE_NX: u64 = 1 << 63;
const PTE_ADDRESS: u64 = 0x000f_ffff_ffff_f000;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct Pte(u64);

impl Pte {
    const EMPTY: Pte = Pte(0);

    fn leaf(pa: usize, flags: MapFlags, level: usize) -> Pte {
        let mut bits = PTE_P | PTE_A | PTE_D;
        if flags.contains(MapFlags::WRITE) {
            bits |= PTE_RW;
        }
        if flags.contains(MapFlags::USER) {
            bits |= PTE_US;
        }
        if !flags.contains(MapFlags::EXEC) {
            bits |= PTE_NX;
        }
        if level > 0 {
            bits |= PTE_PS;
        }
        Pte((pa as u64 & PTE_ADDRESS) | bits)
    }

    /// An intermediate entry. Permissive: the leaf decides.
    fn table(pa: usize) -> Pte {
        Pte((pa as u64 & PTE_ADDRESS) | PTE_P | PTE_RW | PTE_US)
    }

    fn is_valid(self) -> bool {
        self.0 & PTE_P != 0
    }

    fn is_leaf(self, level: usize) -> bool {
        level == 0 || self.0 & PTE_PS != 0
    }

    fn pa(self) -> usize {
        (self.0 & PTE_ADDRESS) as usize
    }

    fn flags(self) -> MapFlags {
        // A present page is always readable on x86.
        let mut flags = MapFlags::NONE | MapFlags::READ;
        if self.0 & PTE_RW != 0 {
            flags = flags | MapFlags::WRITE;
        }
        if self.0 & PTE_NX == 0 {
            flags = flags | MapFlags::EXEC;
        }
        if self.0 & PTE_US != 0 {
            flags = flags | MapFlags::USER;
        }
        flags
    }
}

type Table = [Pte; ENTRIES];

fn table_at<'a>(pa: usize) -> &'a mut Table {
    // SAFETY: every `pa` passed here is a frame this module allocated as a
    // page table (or the root frame owned by a `PageTable`), reached
    // through the direct map. Only one walk touches a table at a time
    // because the owning `PageTable` is borrowed for the whole operation;
    // the kernel's shared sub-tables are only ever read through user
    // tables, never modified.
    unsafe { &mut *(phys_to_virt(pa) as *mut Table) }
}

fn index(va: usize, level: usize) -> usize {
    (va >> (12 + 9 * level)) & (ENTRIES - 1)
}

fn level_of(size: PageSize) -> usize {
    match size {
        PageSize::Size4K => 0,
        PageSize::Size2M => 1,
        PageSize::Size1G => 2,
    }
}

fn size_at(level: usize) -> PageSize {
    match level {
        0 => PageSize::Size4K,
        1 => PageSize::Size2M,
        _ => PageSize::Size1G,
    }
}

fn flush_tlb(va: usize) {
    // SAFETY: `invlpg` only invalidates a cached translation.
    unsafe { core::arch::asm!("invlpg [{}]", in(reg) va, options(nostack)) };
}

/// Switches to the address space rooted at `root_pa`.
///
/// # Safety
/// The table must map the currently executing code and stack at the
/// addresses in use. Every table built here maps the kernel half, so a
/// switch from kernel code is sound for any of them.
pub unsafe fn activate_root(root_pa: usize) {
    // SAFETY: forwarded from the caller's contract; loading CR3 also
    // flushes the TLB.
    unsafe { core::arch::asm!("mov cr3, {}", in(reg) root_pa, options(nostack)) };
}

/// An address space. Owns its root and the intermediate tables of its own
/// half; leaf pages belong to whoever mapped them.
pub struct PageTable {
    root: Frame,
    /// A user table: its upper half is borrowed from the kernel table and
    /// must not be freed with it.
    shares_kernel_half: bool,
}

impl PageTable {
    /// An empty address space.
    pub fn new() -> Result<PageTable, MapError> {
        let root = frame::alloc_zeroed().ok_or(MapError::OutOfMemory)?;
        Ok(PageTable {
            root,
            shares_kernel_half: false,
        })
    }

    /// A user address space: empty lower half, the kernel's upper half.
    pub fn new_user(kernel_root_pa: usize) -> Result<PageTable, MapError> {
        let table = PageTable::new()?;
        let (user, kernel) = (table_at(table.root.pa()), table_at(kernel_root_pa));
        user[KERNEL_HALF_START..].copy_from_slice(&kernel[KERNEL_HALF_START..]);
        Ok(PageTable {
            root: table.root_without_drop(),
            shares_kernel_half: true,
        })
    }

    fn root_without_drop(self) -> Frame {
        let root = self.root;
        core::mem::forget(self);
        root
    }

    /// Physical address of the root table, for CR3.
    pub fn root_pa(&self) -> usize {
        self.root.pa()
    }

    /// Maps one page of `size` at `va` to `pa`. Both must be aligned to
    /// `size`, and nothing may already be mapped there at any level.
    pub fn map(
        &mut self,
        va: usize,
        pa: usize,
        size: PageSize,
        flags: MapFlags,
    ) -> Result<(), MapError> {
        if size == PageSize::Size1G {
            return Err(MapError::Unsupported);
        }
        let bytes = size.bytes();
        if va % bytes != 0 || pa % bytes != 0 {
            return Err(MapError::Misaligned);
        }
        let target = level_of(size);
        let mut table_pa = self.root.pa();
        for level in (0..LEVELS).rev() {
            let entries = table_at(table_pa);
            let i = index(va, level);
            if level == target {
                if entries[i].is_valid() {
                    return Err(MapError::AlreadyMapped);
                }
                entries[i] = Pte::leaf(pa, flags, level);
                return Ok(());
            }
            let entry = entries[i];
            table_pa = if !entry.is_valid() {
                let next = frame::alloc_zeroed().ok_or(MapError::OutOfMemory)?;
                entries[i] = Pte::table(next.pa());
                next.pa()
            } else if entry.is_leaf(level) {
                return Err(MapError::AlreadyMapped);
            } else {
                entry.pa()
            };
        }
        unreachable!("target level is always reached")
    }

    /// Removes the mapping containing `va`, which must be the start of the
    /// page. Returns what it pointed at. Intermediate tables are kept.
    pub fn unmap(&mut self, va: usize) -> Result<(usize, PageSize), MapError> {
        let mut table_pa = self.root.pa();
        for level in (0..LEVELS).rev() {
            let entries = table_at(table_pa);
            let i = index(va, level);
            let entry = entries[i];
            if !entry.is_valid() {
                return Err(MapError::NotMapped);
            }
            if entry.is_leaf(level) {
                let size = size_at(level);
                if va % size.bytes() != 0 {
                    return Err(MapError::Misaligned);
                }
                entries[i] = Pte::EMPTY;
                flush_tlb(va);
                return Ok((entry.pa(), size));
            }
            table_pa = entry.pa();
        }
        Err(MapError::NotMapped)
    }

    /// The physical address and permissions behind `va`, if mapped.
    pub fn translate(&self, va: usize) -> Option<(usize, MapFlags)> {
        let mut table_pa = self.root.pa();
        for level in (0..LEVELS).rev() {
            let entry = table_at(table_pa)[index(va, level)];
            if !entry.is_valid() {
                return None;
            }
            if entry.is_leaf(level) {
                let offset = va & (size_at(level).bytes() - 1);
                return Some((entry.pa() + offset, entry.flags()));
            }
            table_pa = entry.pa();
        }
        None
    }

    /// Makes this the active address space.
    ///
    /// # Safety
    /// See [`activate_root`].
    pub unsafe fn activate(&self) {
        // SAFETY: forwarded from the caller's contract.
        unsafe { activate_root(self.root.pa()) };
    }
}

fn free_subtables(table_pa: usize, level: usize, limit: usize) {
    if level == 0 {
        return;
    }
    for entry in table_at(table_pa)[..limit].iter() {
        if entry.is_valid() && !entry.is_leaf(level) {
            free_subtables(entry.pa(), level - 1, ENTRIES);
            frame::free(Frame::from_pa(entry.pa()));
        }
    }
}

impl Drop for PageTable {
    fn drop(&mut self) {
        let limit = if self.shares_kernel_half {
            KERNEL_HALF_START
        } else {
            ENTRIES
        };
        free_subtables(self.root.pa(), LEVELS - 1, limit);
        frame::free(self.root);
    }
}

crate::ktest! {
    fn map_translate_unmap_4k_and_tables_are_freed() {
        let before = frame::stats().free;
        let page = frame::alloc().expect("frame");
        {
            let mut table = PageTable::new().expect("table");
            let va = 0x4000_0000;
            let rw = MapFlags::READ | MapFlags::WRITE;
            table.map(va, page.pa(), PageSize::Size4K, rw).expect("map");
            assert_eq!(table.translate(va + 0x123), Some((page.pa() + 0x123, rw)));
            assert_eq!(table.translate(va + PAGE_SIZE), None);
            assert_eq!(
                table.map(va, page.pa(), PageSize::Size4K, rw),
                Err(MapError::AlreadyMapped)
            );
            assert_eq!(table.unmap(va), Ok((page.pa(), PageSize::Size4K)));
            assert_eq!(table.translate(va), None);
            assert_eq!(table.unmap(va), Err(MapError::NotMapped));
        }
        frame::free(page);
        assert_eq!(frame::stats().free, before, "page table frames leaked");
    }
}

crate::ktest! {
    fn large_pages_map_and_translate() {
        let mut table = PageTable::new().expect("table");
        let r = MapFlags::READ;
        table.map(0x8000_0000, 0x200_0000, PageSize::Size2M, r).expect("2M");
        assert_eq!(table.translate(0x8010_0000), Some((0x210_0000, r)));
        assert_eq!(
            table.map(0x8000_1000, 0, PageSize::Size4K, r),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(
            table.map(0xC000_0000, 0x4000_0000, PageSize::Size1G, r),
            Err(MapError::Unsupported)
        );
        assert_eq!(table.unmap(0x8000_0000), Ok((0x200_0000, PageSize::Size2M)));
    }
}

crate::ktest! {
    fn user_table_shares_the_kernel_half_and_keeps_it_on_drop() {
        let before = frame::stats().free;
        let kernel_root = crate::mm::kernel_root_pa();
        let kernel_text = crate::kmain as *const () as usize;
        {
            let user = PageTable::new_user(kernel_root).expect("user table");
            let expected = crate::mm::with_kernel_table(|k| k.translate(kernel_text));
            assert_eq!(user.translate(kernel_text), expected, "kernel visible in user table");
            assert!(!expected.expect("mapped").1.contains(MapFlags::USER));
            assert_eq!(user.translate(0x1_0000), None, "user half starts empty");
        }
        assert_eq!(frame::stats().free, before, "dropping a user table freed kernel tables");
    }
}
