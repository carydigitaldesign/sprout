//! Sv39 page tables: three levels, 4 KiB / 2 MiB / 1 GiB pages, 39-bit
//! virtual addresses. Privileged spec chapter "Sv39".
//!
//! The kernel lives in the upper half and reaches every physical address
//! through the direct map at [`PHYS_MAP_BASE`]. Page tables are ordinary
//! frames from the frame allocator, read and written through that map.

use super::csr;
use crate::mm::frame::{self, Frame};
use crate::mm::{MapError, MapFlags, PageSize};

/// Smallest page.
pub const PAGE_SIZE: usize = 4096;

/// Physical address `p` is mapped read-write at `PHYS_MAP_BASE + p` for
/// all RAM and the device region. Chosen so the kernel image's virtual
/// addresses are also its direct-map addresses.
pub const PHYS_MAP_BASE: usize = 0xffff_ffc0_0000_0000;

/// Direct-map address of a physical address.
pub const fn phys_to_virt(pa: usize) -> usize {
    PHYS_MAP_BASE + pa
}

/// Physical address behind a direct-map (or kernel image) address.
pub fn virt_to_phys(va: usize) -> usize {
    debug_assert!(va >= PHYS_MAP_BASE, "{va:#x} is not a direct-map address");
    va - PHYS_MAP_BASE
}

const ENTRIES: usize = 512;
const LEVELS: usize = 3;

const PTE_V: u64 = 1 << 0;
const PTE_R: u64 = 1 << 1;
const PTE_W: u64 = 1 << 2;
const PTE_X: u64 = 1 << 3;
const PTE_U: u64 = 1 << 4;
const PTE_A: u64 = 1 << 6;
const PTE_D: u64 = 1 << 7;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct Pte(u64);

impl Pte {
    const EMPTY: Pte = Pte(0);

    fn leaf(pa: usize, flags: MapFlags) -> Pte {
        // A and D are set up front: QEMU may run without hardware A/D
        // updates, and a page without them faults on first touch.
        let mut bits = PTE_V | PTE_A | PTE_D;
        if flags.contains(MapFlags::READ) {
            bits |= PTE_R;
        }
        if flags.contains(MapFlags::WRITE) {
            bits |= PTE_W;
        }
        if flags.contains(MapFlags::EXEC) {
            bits |= PTE_X;
        }
        if flags.contains(MapFlags::USER) {
            bits |= PTE_U;
        }
        Pte(((pa as u64 >> 12) << 10) | bits)
    }

    fn table(pa: usize) -> Pte {
        Pte(((pa as u64 >> 12) << 10) | PTE_V)
    }

    fn is_valid(self) -> bool {
        self.0 & PTE_V != 0
    }

    fn is_leaf(self) -> bool {
        self.0 & (PTE_R | PTE_W | PTE_X) != 0
    }

    fn pa(self) -> usize {
        ((self.0 >> 10) << 12) as usize
    }

    fn flags(self) -> MapFlags {
        let mut flags = MapFlags::NONE;
        if self.0 & PTE_R != 0 {
            flags = flags | MapFlags::READ;
        }
        if self.0 & PTE_W != 0 {
            flags = flags | MapFlags::WRITE;
        }
        if self.0 & PTE_X != 0 {
            flags = flags | MapFlags::EXEC;
        }
        if self.0 & PTE_U != 0 {
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
    // because the owning `PageTable` is borrowed for the whole operation.
    unsafe { &mut *(phys_to_virt(pa) as *mut Table) }
}

fn vpn(va: usize, level: usize) -> usize {
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
    // SAFETY: `sfence.vma` only invalidates cached translations.
    unsafe { core::arch::asm!("sfence.vma {}, zero", in(reg) va, options(nostack)) };
}

fn flush_tlb_all() {
    // SAFETY: as above, for every translation.
    unsafe { core::arch::asm!("sfence.vma", options(nostack)) };
}

/// An Sv39 address space. Owns its root and intermediate tables; leaf
/// pages belong to whoever mapped them.
pub struct PageTable {
    root: Frame,
}

impl PageTable {
    /// An empty address space.
    pub fn new() -> Result<PageTable, MapError> {
        let root = frame::alloc_zeroed().ok_or(MapError::OutOfMemory)?;
        Ok(PageTable { root })
    }

    /// Physical address of the root table, for `satp`. Used from M4 when
    /// address spaces are switched per process.
    #[allow(dead_code)]
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
        let bytes = size.bytes();
        if va % bytes != 0 || pa % bytes != 0 {
            return Err(MapError::Misaligned);
        }
        let target = level_of(size);
        let mut table_pa = self.root.pa();
        for level in (0..LEVELS).rev() {
            let entries = table_at(table_pa);
            let index = vpn(va, level);
            if level == target {
                if entries[index].is_valid() {
                    return Err(MapError::AlreadyMapped);
                }
                entries[index] = Pte::leaf(pa, flags);
                return Ok(());
            }
            let entry = entries[index];
            table_pa = if !entry.is_valid() {
                let next = frame::alloc_zeroed().ok_or(MapError::OutOfMemory)?;
                entries[index] = Pte::table(next.pa());
                next.pa()
            } else if entry.is_leaf() {
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
            let index = vpn(va, level);
            let entry = entries[index];
            if !entry.is_valid() {
                return Err(MapError::NotMapped);
            }
            if entry.is_leaf() {
                let size = size_at(level);
                if va % size.bytes() != 0 {
                    return Err(MapError::Misaligned);
                }
                entries[index] = Pte::EMPTY;
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
            let entry = table_at(table_pa)[vpn(va, level)];
            if !entry.is_valid() {
                return None;
            }
            if entry.is_leaf() {
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
    /// The table must map the currently executing code, the stack, and
    /// everything the kernel will touch, at the addresses it will use.
    pub unsafe fn activate(&self) {
        const MODE_SV39: usize = 8 << 60;
        // SAFETY: the caller guarantees the table is complete for the
        // kernel; writing satp then flushing the TLB is the documented
        // sequence for switching address spaces.
        unsafe { csr::satp::write(MODE_SV39 | (self.root.pa() >> 12)) };
        flush_tlb_all();
    }
}

fn free_subtables(table_pa: usize, level: usize) {
    if level == 0 {
        return;
    }
    for entry in table_at(table_pa).iter() {
        if entry.is_valid() && !entry.is_leaf() {
            free_subtables(entry.pa(), level - 1);
            frame::free(Frame::from_pa(entry.pa()));
        }
    }
}

impl Drop for PageTable {
    fn drop(&mut self) {
        free_subtables(self.root.pa(), LEVELS - 1);
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
        table.map(0x8000_0000, 0x8000_0000, PageSize::Size2M, r).expect("2M");
        table.map(0xC000_0000, 0x4000_0000, PageSize::Size1G, r).expect("1G");
        assert_eq!(table.translate(0x8010_0000), Some((0x8010_0000, r)));
        assert_eq!(table.translate(0xC123_4567), Some((0x4123_4567, r)));
        // A 4K page inside a 2M mapping is refused rather than silently nested.
        assert_eq!(
            table.map(0x8000_1000, 0, PageSize::Size4K, r),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(table.unmap(0xC000_0000), Ok((0x4000_0000, PageSize::Size1G)));
    }
}

crate::ktest! {
    fn misaligned_mappings_are_rejected() {
        let mut table = PageTable::new().expect("table");
        assert_eq!(
            table.map(0x1000, 0x2000, PageSize::Size2M, MapFlags::READ),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            table.map(0x123, 0x1000, PageSize::Size4K, MapFlags::READ),
            Err(MapError::Misaligned)
        );
    }
}
