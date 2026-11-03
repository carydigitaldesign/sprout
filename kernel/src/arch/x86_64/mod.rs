//! x86_64 on QEMU's `pc` machine, booted by a Multiboot loader. Legacy
//! devices only for now: 8259 PICs, the 8254 timer, COM1 and the VGA
//! text buffer, all behind port I/O or one fixed page. The local APIC,
//! PCI and SMP come later.

mod backtrace;
mod boot;
mod context;
mod exit;
mod gdt;
mod handoff;
mod idt;
pub mod interrupts;
// The full page-table API is exercised by tests; the kernel itself only
// needs all of it once address spaces are torn down.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod paging;
mod pic;
mod pit;
mod ports;
mod serial;
mod user;
mod vga;

pub use backtrace::{boot_stack, frame_pointer, frames_from as stack_frames, set_current_stack};
pub use context::{Context, switch};
pub use exit::exit;
pub use handoff::handoff;
pub use paging::{PAGE_SIZE, PageTable, activate_root, phys_to_virt, virt_to_phys};
pub use user::enter_user;

use crate::mm::{self, MapError, MapFlags, PageSize};

/// Architecture name for logs.
pub const NAME: &str = "x86_64";

/// Exit code a process gets for executing an unmapped address: 128 plus
/// the page-fault vector.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
pub const EXIT_CODE_INSTRUCTION_FAULT: i32 = 128 + 14;

/// Runs before the first log line: the serial port and the screen, then a real GDT with
/// a TSS and the IDT so a fault from here on is reported, the interrupt
/// controllers with every line masked, and the backtrace walker's view
/// of the boot stack.
pub fn init_early() {
    serial::init();
    vga::init();
    gdt::init();
    idt::init();
    pic::init();
    backtrace::init();
}

/// Brings up the console's receive interrupt. The timer was started by
/// `handoff` for calibration and unmasked by `time::init`.
pub fn init_devices() {
    serial::enable_rx_interrupt();
}

/// 1 GiB pages are optional on x86_64 and QEMU's default CPU lacks them.
pub fn supports_page_size(size: PageSize) -> bool {
    size != PageSize::Size1G
}

/// Adds the platform's own mappings to the kernel table. The image sits
/// in the top 2 GiB rather than inside the direct map, so its physical
/// pages are mapped there too (writable, not executable): the direct map
/// then covers all of RAM, as the rest of the kernel expects. The VGA
/// text buffer, below the RAM the hand-off reports, is mapped as well.
pub fn map_platform(
    table: &mut PageTable,
    kernel_start_pa: usize,
    kernel_end_pa: usize,
) -> Result<(), MapError> {
    let rw = MapFlags::READ | MapFlags::WRITE;
    mm::map_range(
        table,
        phys_to_virt(kernel_start_pa),
        kernel_start_pa,
        kernel_end_pa - kernel_start_pa,
        rw,
    )?;
    mm::map_range(
        table,
        phys_to_virt(vga::BUFFER_PA),
        vga::BUFFER_PA,
        vga::BUFFER_BYTES,
        rw,
    )
}

/// Routes a legacy IRQ line (0..16) to `handler`.
pub fn irq_register(irq: u32, handler: fn(u32)) {
    idt::register_irq(irq, handler);
    pic::set_masked(irq as u8, false);
}

/// Stops `irq` from interrupting until [`irq_unmask`].
pub fn irq_mask(irq: u32) {
    pic::set_masked(irq as u8, true);
}

/// Lets `irq` interrupt again.
pub fn irq_unmask(irq: u32) {
    pic::set_masked(irq as u8, false);
}

/// The time-stamp counter. Its rate was measured against the PIT at boot.
pub fn timer_cycles() -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: `rdtsc` only reads the counter.
    unsafe { core::arch::asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack)) };
    ((high as u64) << 32) | low as u64
}

/// The PIT is periodic, so there is nothing to arm per tick.
pub fn set_timer(_deadline: u64) {}

/// Writes one byte to COM1 and the screen. Called by `log`'s console
/// writer under the console lock, which the screen relies on.
pub fn console_putc(byte: u8) {
    serial::putc(byte);
    vga::putc(byte);
}

pub fn console_backend() -> &'static str {
    "com1+vga"
}

crate::ktest! {
    fn boot_stack_is_16_byte_aligned() {
        let (_, top) = boot_stack();
        assert_eq!(top % 16, 0, "stack top = {top:#x}");
    }
}
