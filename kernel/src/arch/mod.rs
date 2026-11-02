//! Architecture-specific code. Nothing outside this module may use `asm!`,
//! touch a hardware register or assume an address layout (ADR-0002).
//!
//! Every architecture provides the same surface:
//!
//! - `NAME`, `EXIT_CODE_INSTRUCTION_FAULT`
//! - `init_early()`, `handoff() -> Handoff`, `init_devices()`
//! - memory: `PAGE_SIZE`, `PageTable`, `activate_root`, `phys_to_virt`,
//!   `virt_to_phys`, `supports_page_size`, `map_platform`
//! - threads: `Context`, `switch`, `enter_user`, `boot_stack`,
//!   `frame_pointer`, `stack_frames`, `set_current_stack`
//! - interrupts: the `interrupts` module, `irq_register`, `irq_mask`,
//!   `irq_unmask`, `timer_cycles`, `set_timer`
//! - console and exit: `console_putc`, `console_backend`, `exit`
//!
//! Traps call back into `syscall::dispatch`, `syscall::user_fault`,
//! `time::on_tick`, `sched::on_tick` and `console::push`.

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
pub use riscv64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;
