//! The two cascaded 8259 interrupt controllers, remapped so IRQ 0..16
//! land on vectors 32..48 (clear of the CPU exceptions).

use super::ports::{inb, outb};

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xA0;
const SLAVE_DATA: u16 = 0xA1;
const VECTOR_BASE: u8 = 32;
const END_OF_INTERRUPT: u8 = 0x20;

/// Remaps both PICs and masks every line except the cascade. Sources are
/// unmasked individually as their handlers come up.
pub fn init() {
    // SAFETY: the standard 8259 initialisation sequence on the standard
    // ports; nothing else drives these controllers.
    unsafe {
        outb(MASTER_COMMAND, 0x11);
        outb(SLAVE_COMMAND, 0x11);
        outb(MASTER_DATA, VECTOR_BASE);
        outb(SLAVE_DATA, VECTOR_BASE + 8);
        outb(MASTER_DATA, 0b0000_0100);
        outb(SLAVE_DATA, 2);
        outb(MASTER_DATA, 0x01);
        outb(SLAVE_DATA, 0x01);
        outb(MASTER_DATA, !0b0000_0100);
        outb(SLAVE_DATA, 0xFF);
    }
}

/// Masks or unmasks one IRQ line.
pub fn set_masked(irq: u8, masked: bool) {
    let (port, bit) = if irq < 8 {
        (MASTER_DATA, irq)
    } else {
        (SLAVE_DATA, irq - 8)
    };
    // SAFETY: the mask registers only gate interrupt delivery.
    unsafe {
        let current = inb(port);
        let next = if masked {
            current | (1 << bit)
        } else {
            current & !(1 << bit)
        };
        outb(port, next);
    }
}

/// Acknowledges an IRQ so the controller can raise the next one.
pub fn end_of_interrupt(irq: u8) {
    // SAFETY: EOI commands have no effect beyond re-arming the controller.
    unsafe {
        if irq >= 8 {
            outb(SLAVE_COMMAND, END_OF_INTERRUPT);
        }
        outb(MASTER_COMMAND, END_OF_INTERRUPT);
    }
}
