//! Memory management: physical frames, the kernel address space, the kernel
//! heap, and virtual memory objects.
//!
//! Boot hands over with a compile-time page table that identity-maps and
//! direct-maps the first few GiB with large pages (`arch::boot`). [`init`]
//! replaces it with a table that maps the kernel image with per-section
//! permissions, all of RAM in the direct map, and whatever the platform
//! needs (`arch::map_platform`), and drops the identity mapping so a null
//! dereference faults.

pub mod frame;
pub mod heap;
pub mod vmo;

use core::fmt;
use core::ops::BitOr;

use crate::arch::{self, PAGE_SIZE, PageTable, phys_to_virt, virt_to_phys};
use crate::handoff::{Handoff, MAX_REGIONS, Region};
use crate::log;
use crate::sync::SpinLock;

/// Access permissions of a mapping.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MapFlags(u8);

impl MapFlags {
    pub const NONE: MapFlags = MapFlags(0);
    pub const READ: MapFlags = MapFlags(1 << 0);
    pub const WRITE: MapFlags = MapFlags(1 << 1);
    pub const EXEC: MapFlags = MapFlags(1 << 2);
    /// Accessible from user mode.
    pub const USER: MapFlags = MapFlags(1 << 3);

    pub const fn contains(self, other: MapFlags) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for MapFlags {
    type Output = MapFlags;
    fn bitor(self, rhs: MapFlags) -> MapFlags {
        MapFlags(self.0 | rhs.0)
    }
}

impl fmt::Debug for MapFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = [b'-'; 4];
        if self.contains(Self::READ) {
            s[0] = b'r';
        }
        if self.contains(Self::WRITE) {
            s[1] = b'w';
        }
        if self.contains(Self::EXEC) {
            s[2] = b'x';
        }
        if self.contains(Self::USER) {
            s[3] = b'u';
        }
        f.write_str(core::str::from_utf8(&s).unwrap_or("????"))
    }
}

/// Page sizes the architecture can map in one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSize {
    Size4K,
    Size2M,
    Size1G,
}

impl PageSize {
    pub const fn bytes(self) -> usize {
        match self {
            PageSize::Size4K => 4 << 10,
            PageSize::Size2M => 2 << 20,
            PageSize::Size1G => 1 << 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    Misaligned,
    AlreadyMapped,
    NotMapped,
    OutOfMemory,
    /// The architecture cannot map a page of that size here.
    #[cfg_attr(target_arch = "riscv64", allow(dead_code))]
    Unsupported,
}

unsafe extern "C" {
    static __kernel_start: u8;
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
}

/// Virtual address of a linker symbol.
macro_rules! sym {
    ($name:ident) => {
        core::ptr::addr_of!($name) as usize
    };
}

/// The kernel's own address space, active from [`init`] onwards.
static KERNEL_TABLE: SpinLock<Option<PageTable>> = SpinLock::new(None);

/// Facts from boot that later code and tests may want.
#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    /// The RAM region holding the kernel.
    pub ram: Region,
    /// Rate of the platform timer counter.
    pub timebase_hz: u64,
    /// Physical address of the device tree, on platforms that have one.
    pub device_tree: Option<usize>,
}

static BOOT_INFO: SpinLock<Option<BootInfo>> = SpinLock::new(None);

pub fn boot_info() -> BootInfo {
    BOOT_INFO.lock().expect("mm::init has not run")
}

/// Root of the kernel's page table, which user tables share their upper
/// half with.
pub fn kernel_root_pa() -> usize {
    with_kernel_table(|t| t.root_pa())
}

/// Runs a closure with the kernel page table.
pub fn with_kernel_table<R>(f: impl FnOnce(&mut PageTable) -> R) -> R {
    let mut slot = KERNEL_TABLE.lock();
    f(slot.as_mut().expect("mm::init has not run"))
}

/// Physical range of the kernel image, including its boot stack.
pub fn kernel_image_range() -> (usize, usize) {
    (
        virt_to_phys(sym!(__kernel_start)),
        virt_to_phys(sym!(__kernel_end)),
    )
}

/// Brings up memory management from what the architecture handed over.
pub fn init(handoff: &Handoff) {
    let ram = handoff.main_ram().expect("the boot hand-off names no RAM");
    let (_, kernel_end_pa) = kernel_image_range();

    let mut reserved = [(0usize, 0usize); MAX_REGIONS];
    let mut count = 0;
    for r in handoff.reserved.iter().flatten() {
        reserved[count] = (r.base, r.end());
        count += 1;
    }
    frame::init(kernel_end_pa, ram.end(), &reserved[..count]);

    let table =
        build_kernel_table(ram).unwrap_or_else(|e| panic!("building the kernel page table: {e:?}"));
    // SAFETY: the table maps text, rodata, data, bss and the stack at
    // their linked addresses, and all RAM plus devices in the direct map,
    // so nothing the kernel is using moves when it becomes active.
    unsafe { table.activate() };
    *KERNEL_TABLE.lock() = Some(table);

    heap::init(heap::DEFAULT_FRAMES);
    *BOOT_INFO.lock() = Some(BootInfo {
        ram,
        timebase_hz: handoff.timebase_hz,
        device_tree: handoff.device_tree,
    });

    let stats = frame::stats();
    log::info("mm", "memory management ready")
        .hex("ram_base", ram.base)
        .num("ram_mib", ram.size >> 20)
        .num("frames_total", stats.total)
        .num("frames_free", stats.free)
        .num("heap_kib", (heap::DEFAULT_FRAMES * PAGE_SIZE) >> 10)
        .num("timebase_hz", handoff.timebase_hz)
        .end();
}

/// Maps `len` bytes from `va` to `pa` using the largest page size the
/// alignment allows at each step.
pub fn map_range(
    table: &mut PageTable,
    va: usize,
    pa: usize,
    len: usize,
    flags: MapFlags,
) -> Result<(), MapError> {
    let mut done = 0;
    while done < len {
        let (v, p, left) = (va + done, pa + done, len - done);
        let size = [PageSize::Size1G, PageSize::Size2M]
            .into_iter()
            .filter(|s| arch::supports_page_size(*s))
            .find(|s| v % s.bytes() == 0 && p % s.bytes() == 0 && left >= s.bytes())
            .unwrap_or(PageSize::Size4K);
        table.map(v, p, size, flags)?;
        done += size.bytes();
    }
    Ok(())
}

fn build_kernel_table(ram: Region) -> Result<PageTable, MapError> {
    let mut table = PageTable::new()?;
    let r = MapFlags::READ;
    let rw = MapFlags::READ | MapFlags::WRITE;
    let rx = MapFlags::READ | MapFlags::EXEC;

    let (text_s, text_e) = (sym!(__text_start), sym!(__text_end));
    let (ro_s, ro_e) = (sym!(__rodata_start), sym!(__rodata_end));
    let (data_s, kernel_e) = (sym!(__data_start), sym!(__kernel_end));

    // The image, 4 KiB pages, per-section permissions.
    map_range(
        &mut table,
        text_s,
        virt_to_phys(text_s),
        text_e - text_s,
        rx,
    )?;
    map_range(&mut table, ro_s, virt_to_phys(ro_s), ro_e - ro_s, r)?;
    map_range(
        &mut table,
        data_s,
        virt_to_phys(data_s),
        kernel_e - data_s,
        rw,
    )?;

    // The rest of RAM in the direct map: whatever lies below the image
    // (firmware on riscv; harmless to map) and everything above it.
    let (kernel_s_pa, kernel_e_pa) = kernel_image_range();
    if kernel_s_pa > ram.base {
        map_range(
            &mut table,
            phys_to_virt(ram.base),
            ram.base,
            kernel_s_pa - ram.base,
            rw,
        )?;
    }
    map_range(
        &mut table,
        phys_to_virt(kernel_e_pa),
        kernel_e_pa,
        ram.end() - kernel_e_pa,
        rw,
    )?;

    // Devices and anything else the platform wants reachable.
    arch::map_platform(&mut table, kernel_s_pa, kernel_e_pa)?;
    Ok(table)
}

crate::ktest! {
    fn kernel_table_has_section_permissions_and_no_identity_map() {
        let text = sym!(__text_start);
        let rodata = sym!(__rodata_start);
        let data = sym!(__data_start);
        let (kernel_s_pa, _) = kernel_image_range();
        with_kernel_table(|t| {
            let (pa, flags) = t.translate(text).expect("text mapped");
            assert_eq!(pa, virt_to_phys(text));
            assert_eq!(flags, MapFlags::READ | MapFlags::EXEC);
            let (_, flags) = t.translate(rodata).expect("rodata mapped");
            assert_eq!(flags, MapFlags::READ);
            let (_, flags) = t.translate(data).expect("data mapped");
            assert_eq!(flags, MapFlags::READ | MapFlags::WRITE);
            assert_eq!(t.translate(kernel_s_pa), None, "identity map should be gone");
            assert_eq!(t.translate(0), None);
        });
    }
}

crate::ktest! {
    fn boot_info_matches_qemu() {
        let info = boot_info();
        assert!(info.ram.size > 100 << 20, "xtask boots QEMU with -m 128M");
        assert!(info.timebase_hz > 0);
        assert_eq!(info.device_tree.is_some(), cfg!(target_arch = "riscv64"));
    }
}

crate::ktest! {
    fn direct_map_reads_what_the_kernel_wrote() {
        static MARKER: [u8; 8] = *b"sproutmm";
        let via_direct_map = phys_to_virt(virt_to_phys(MARKER.as_ptr() as usize)) as *const [u8; 8];
        // SAFETY: the direct map aliases the kernel image, so this reads MARKER.
        assert_eq!(unsafe { *via_direct_map }, MARKER);
    }
}
