//! riscv64 on the QEMU `virt` machine, booted by OpenSBI.

mod backtrace;
mod boot;
mod context;
// Every CSR gets read and write; not every milestone uses both.
#[allow(dead_code)]
mod csr;
mod exit;
mod handoff;
pub mod interrupts;
// The full page-table API is exercised by tests; the kernel itself only
// needs all of it once address spaces are torn down.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod paging;
mod plic;
mod sbi;
mod trap;
mod uart;
mod user;

pub use backtrace::{boot_stack, frame_pointer, frames_from as stack_frames, set_current_stack};
pub use context::{Context, switch};
pub use exit::exit;
pub use handoff::handoff;
pub use paging::{PAGE_SIZE, PageTable, activate_root, phys_to_virt, virt_to_phys};
pub use user::enter_user;

use crate::mm::{MapError, MapFlags, PageSize};

/// Architecture name for logs.
pub const NAME: &str = "riscv64";

/// Exit code a process gets for executing an unmapped address: 128 plus
/// the instruction page fault cause.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
pub const EXIT_CODE_INSTRUCTION_FAULT: i32 = 128 + 12;

/// Runs before the first log line: find the SBI console, install the trap
/// vector so a fault from here on is reported rather than silent, and tell
/// the backtrace walker about the boot stack.
pub fn init_early() {
    sbi::init();
    trap::init();
    backtrace::init();
}

/// Brings up the interrupt controller and the console's receive interrupt.
/// Needs the direct map, so it runs after `mm::init`.
pub fn init_devices() {
    plic::init();
    uart::init();
}

/// Whether the MMU can map a page of this size. Sv39 does all three.
pub fn supports_page_size(_size: PageSize) -> bool {
    true
}

/// Adds the platform's own mappings to the kernel table: the first GiB of
/// physical space, where virt keeps its devices (UART, PLIC, virtio, test
/// finisher).
pub fn map_platform(
    table: &mut PageTable,
    _kernel_start_pa: usize,
    _kernel_end_pa: usize,
) -> Result<(), MapError> {
    table.map(
        phys_to_virt(0),
        0,
        PageSize::Size1G,
        MapFlags::READ | MapFlags::WRITE,
    )
}

/// Routes an external interrupt source to `handler`.
pub fn irq_register(irq: u32, handler: fn(u32)) {
    plic::enable(irq, 1, handler);
}

/// Stops `irq` from interrupting until [`irq_unmask`].
pub fn irq_mask(irq: u32) {
    plic::set_source_enabled(irq, false);
}

/// Lets `irq` interrupt again.
pub fn irq_unmask(irq: u32) {
    plic::set_source_enabled(irq, true);
}

/// The platform timer counter. Its rate comes from the device tree.
pub fn timer_cycles() -> u64 {
    csr::time::read() as u64
}

/// Raises a supervisor timer interrupt once the counter reaches `deadline`.
pub fn set_timer(deadline: u64) {
    sbi::set_timer(deadline);
}

/// Writes one byte to the firmware console.
pub fn console_putc(byte: u8) {
    sbi::console_putc(byte);
}

/// Which console mechanism `console_putc` uses.
pub fn console_backend() -> &'static str {
    if sbi::has_debug_console() {
        "sbi-dbcn"
    } else {
        "sbi-legacy"
    }
}

/// Parks this hart until an interrupt arrives, forever.
pub fn wait_forever() -> ! {
    loop {
        interrupts::wait_for_interrupt();
    }
}

crate::ktest! {
    fn boot_stack_is_16_byte_aligned() {
        let (_, top) = boot_stack();
        assert_eq!(top % 16, 0, "stack top = {top:#x}");
    }
}

crate::ktest! {
    fn device_gib_is_mapped_in_the_direct_map() {
        let finisher = phys_to_virt(0x10_0000);
        crate::mm::with_kernel_table(|t| {
            assert_eq!(
                t.translate(finisher),
                Some((0x10_0000, MapFlags::READ | MapFlags::WRITE))
            );
        });
    }
}
