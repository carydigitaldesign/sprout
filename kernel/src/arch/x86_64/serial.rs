//! COM1, the 16550 at port 0x3F8: the console, both directions.

use super::ports::{inb, outb};
use crate::{console, log};

const COM1: u16 = 0x3F8;
const DATA: u16 = COM1;
const INTERRUPT_ENABLE: u16 = COM1 + 1;
const FIFO_CONTROL: u16 = COM1 + 2;
const LINE_CONTROL: u16 = COM1 + 3;
const MODEM_CONTROL: u16 = COM1 + 4;
const LINE_STATUS: u16 = COM1 + 5;
const LSR_DATA_READY: u8 = 1 << 0;
const LSR_TRANSMIT_EMPTY: u8 = 1 << 5;

/// 115200 baud, 8N1, FIFOs on, interrupts off until asked for.
pub fn init() {
    // SAFETY: standard 16550 setup on COM1's ports.
    unsafe {
        outb(INTERRUPT_ENABLE, 0);
        outb(LINE_CONTROL, 0x80);
        outb(DATA, 1);
        outb(INTERRUPT_ENABLE, 0);
        outb(LINE_CONTROL, 0x03);
        outb(FIFO_CONTROL, 0xC7);
        outb(MODEM_CONTROL, 0x0B);
    }
}

/// Lets received bytes raise IRQ 4.
pub fn enable_rx_interrupt() {
    // SAFETY: enables the receive interrupt only.
    unsafe { outb(INTERRUPT_ENABLE, 0x01) };
    super::pic::set_masked(4, false);
}

/// Blocks until the transmitter has room, then sends one byte.
pub fn putc(byte: u8) {
    // SAFETY: polling line status then writing the data register.
    unsafe {
        while inb(LINE_STATUS) & LSR_TRANSMIT_EMPTY == 0 {
            core::hint::spin_loop();
        }
        outb(DATA, byte);
    }
}

/// IRQ 4: drain whatever arrived into the console ring.
pub fn on_interrupt() {
    let mut bytes = 0usize;
    // SAFETY: reads consume received bytes, which is the intent.
    unsafe {
        while inb(LINE_STATUS) & LSR_DATA_READY != 0 {
            console::push(inb(DATA));
            bytes += 1;
        }
    }
    log::event("console_rx").num("bytes", bytes).end();
}
