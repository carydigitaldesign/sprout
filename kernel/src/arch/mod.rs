//! Architecture-specific code. Nothing outside this module may use `asm!`,
//! touch a hardware register or assume an address layout (ADR-0002).
//!
//! Every architecture provides the same small surface:
//!
//! - `NAME`: the architecture's name for logs.
//! - `init_early()`: whatever must happen before the first log line.
//! - `console_putc(u8)`: write one byte to the boot console.
//! - `console_backend()`: which console mechanism is in use, for logs.
//! - `exit(code) -> !`: stop the machine (or emulator) with a status code.
//! - `wait_forever() -> !`: park the current hart.

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
pub use riscv64::*;
