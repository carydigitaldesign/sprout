//! Receive side of the ns16550 serial port on QEMU `virt`. Output still goes
//! through the SBI console; this only turns typed bytes into interrupts and
//! hands them to `console`.

use super::paging::phys_to_virt;
use super::plic;
use crate::{console, log};

const UART_PA: usize = 0x1000_0000;
/// PLIC source for the UART on `virt`.
pub const IRQ: u32 = 10;

const RBR: usize = 0;
const IER: usize = 1;
const IIR: usize = 2;
const FCR: usize = 2;
const LSR: usize = 5;
const IER_RECEIVED_DATA: u8 = 1 << 0;
/// Enable the FIFOs, clear both, receive trigger level 14 bytes. QEMU's
/// 16550 tells its host side it has room for only `trigger level - queued`
/// bytes, so a low trigger level makes a burst of typed bytes drop while
/// the first is still unread. Short bursts below the level still interrupt
/// through the character timeout.
const FCR_ENABLE_AND_CLEAR: u8 = 0b1100_0111;
const LSR_DATA_READY: u8 = 1 << 0;

fn read(offset: usize) -> u8 {
    // SAFETY: the UART's byte registers sit in the device GiB mapped in the
    // direct map; reading RBR consumes a byte, which is the intent.
    unsafe { core::ptr::read_volatile(phys_to_virt(UART_PA + offset) as *const u8) }
}

fn write(offset: usize, value: u8) {
    // SAFETY: as for `read`; only the interrupt-enable register is written.
    unsafe { core::ptr::write_volatile(phys_to_virt(UART_PA + offset) as *mut u8, value) };
}

/// Enables the receive interrupt and routes it through the PLIC.
pub fn init() {
    write(FCR, FCR_ENABLE_AND_CLEAR);
    write(IER, IER_RECEIVED_DATA);
    plic::enable(IRQ, 1, on_interrupt);
}

fn on_interrupt(_source: u32) {
    // Reading IIR acknowledges the interrupt; the FIFO state is in LSR.
    let _ = read(IIR);
    let mut bytes = 0usize;
    while read(LSR) & LSR_DATA_READY != 0 {
        console::push(read(RBR));
        bytes += 1;
    }
    log::event("console_rx").num("bytes", bytes).end();
}
