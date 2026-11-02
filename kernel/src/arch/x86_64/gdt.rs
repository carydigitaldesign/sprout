//! The global descriptor table and the task state segment. In long mode
//! segments are flat; the GDT exists to say which ring code runs in, and
//! the TSS to tell the CPU which stack to switch to on a trap from user
//! mode.

use core::cell::UnsafeCell;

/// Selectors, as `cs`/`ss` and the IDT want them.
pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
pub const USER_DATA: u16 = 0x18 | 3;
pub const USER_CODE: u16 = 0x20 | 3;
const TSS_SELECTOR: u16 = 0x28;

/// The 64-bit TSS. Only `rsp0` matters: the kernel stack for traps from
/// user mode.
#[repr(C, packed)]
struct Tss {
    _reserved0: u32,
    rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    _reserved1: u64,
    ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    iomap_base: u16,
}

/// Written by the scheduler with interrupts off, read by the CPU.
struct Shared<T>(UnsafeCell<T>);

// SAFETY: every write happens on the boot CPU with interrupts disabled
// (scheduler switches and boot), and the hardware reads are not Rust
// accesses; there is no concurrent Rust reader.
unsafe impl<T> Sync for Shared<T> {}

static TSS: Shared<Tss> = Shared(UnsafeCell::new(Tss {
    _reserved0: 0,
    rsp0: 0,
    rsp1: 0,
    rsp2: 0,
    _reserved1: 0,
    ist: [0; 7],
    _reserved2: 0,
    _reserved3: 0,
    iomap_base: size_of::<Tss>() as u16,
}));

/// Null, kernel code, kernel data, user data, user code, TSS (two slots).
static GDT: Shared<[u64; 7]> = Shared(UnsafeCell::new([
    0,
    0x00AF_9A00_0000_FFFF,
    0x00CF_9200_0000_FFFF,
    0x00CF_F200_0000_FFFF,
    0x00AF_FA00_0000_FFFF,
    0,
    0,
]));

#[repr(C, packed)]
struct DescriptorPointer {
    limit: u16,
    base: u64,
}

/// Installs the GDT, reloads the segment registers and loads the TSS.
pub fn init() {
    let tss = TSS.0.get() as u64;
    let limit = (size_of::<Tss>() - 1) as u64;
    let low = (limit & 0xffff)
        | ((tss & 0xff_ffff) << 16)
        | (0x89u64 << 40)
        | (((limit >> 16) & 0xf) << 48)
        | (((tss >> 24) & 0xff) << 56);
    let high = tss >> 32;
    // SAFETY: boot CPU, interrupts off, nothing else touches the GDT.
    unsafe {
        let gdt = &mut *GDT.0.get();
        gdt[5] = low;
        gdt[6] = high;
        let pointer = DescriptorPointer {
            limit: (size_of::<[u64; 7]>() - 1) as u16,
            base: GDT.0.get() as u64,
        };
        core::arch::asm!(
            "lgdt [{ptr}]",
            // Reload CS with a far return, then the data segments.
            "push {code}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {data:x}",
            "mov es, {data:x}",
            "mov ss, {data:x}",
            "ltr {tss:x}",
            ptr = in(reg) &pointer,
            code = const KERNEL_CODE as u64,
            data = in(reg) KERNEL_DATA,
            tss = in(reg) TSS_SELECTOR,
            tmp = out(reg) _,
            options(nostack),
        );
    }
}

/// Tells the CPU where traps from user mode should switch the stack to.
pub fn set_rsp0(kernel_stack_top: usize) {
    // SAFETY: called with interrupts off by the scheduler and boot; the
    // write is a plain store the hardware reads on the next trap.
    unsafe {
        let tss = TSS.0.get();
        core::ptr::addr_of_mut!((*tss).rsp0).write_unaligned(kernel_stack_top as u64);
    }
}
