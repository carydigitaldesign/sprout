//! The VGA text console: 80 by 25 cells at physical 0xB8000, which every
//! BIOS-booted PC and the usual hypervisors leave in text mode. Lines
//! longer than the screen wrap; the screen scrolls. Output goes here as
//! well as to COM1 so a machine without a serial port still shows the
//! boot.

use core::cell::UnsafeCell;

use super::paging::phys_to_virt;
use super::ports::outb;

/// The text buffer. `map_platform` keeps it in the direct map.
pub const BUFFER_PA: usize = 0xB8000;
pub const BUFFER_BYTES: usize = 0x8000;
const COLUMNS: usize = 80;
const ROWS: usize = 25;
/// Light grey on black.
const ATTRIBUTE: u16 = 0x07 << 8;
const CRTC_INDEX: u16 = 0x3D4;
const CRTC_DATA: u16 = 0x3D5;
const CURSOR_HIGH: u8 = 0x0E;
const CURSOR_LOW: u8 = 0x0F;

struct Screen {
    row: usize,
    column: usize,
}

struct Shared(UnsafeCell<Screen>);

// SAFETY: the screen is only touched through `init` (boot, one CPU, before
// interrupts) and `putc`, which `log`'s console writer calls while holding
// the console lock with interrupts masked; see `arch::console_putc`.
unsafe impl Sync for Shared {}

static SCREEN: Shared = Shared(UnsafeCell::new(Screen { row: 0, column: 0 }));

fn cell(row: usize, column: usize) -> *mut u16 {
    (phys_to_virt(BUFFER_PA) as *mut u16).wrapping_add(row * COLUMNS + column)
}

fn write_cell(row: usize, column: usize, value: u16) {
    // SAFETY: row and column are within the 80 by 25 buffer, which is
    // mapped in the direct map at boot and by `map_platform` after.
    unsafe { core::ptr::write_volatile(cell(row, column), value) };
}

fn read_cell(row: usize, column: usize) -> u16 {
    // SAFETY: as for `write_cell`.
    unsafe { core::ptr::read_volatile(cell(row, column)) }
}

fn clear_row(row: usize) {
    for column in 0..COLUMNS {
        write_cell(row, column, ATTRIBUTE | b' ' as u16);
    }
}

fn move_cursor(row: usize, column: usize) {
    let position = (row * COLUMNS + column) as u16;
    // SAFETY: the CRT controller's cursor registers only move the cursor.
    unsafe {
        outb(CRTC_INDEX, CURSOR_HIGH);
        outb(CRTC_DATA, (position >> 8) as u8);
        outb(CRTC_INDEX, CURSOR_LOW);
        outb(CRTC_DATA, position as u8);
    }
}

impl Screen {
    fn newline(&mut self) {
        self.column = 0;
        if self.row + 1 < ROWS {
            self.row += 1;
            return;
        }
        for row in 1..ROWS {
            for column in 0..COLUMNS {
                write_cell(row - 1, column, read_cell(row, column));
            }
        }
        clear_row(ROWS - 1);
    }

    fn put(&mut self, byte: u8) {
        match byte {
            b'\n' => self.newline(),
            b'\r' => self.column = 0,
            b'\t' => {
                let next = (self.column / 4 + 1) * 4;
                while self.column < next.min(COLUMNS) {
                    self.put(b' ');
                }
            }
            // Continuation bytes of a UTF-8 sequence: the lead byte already
            // showed a placeholder.
            0x80..=0xBF => {}
            0x00..=0x1F | 0x7F => {}
            _ => {
                if self.column == COLUMNS {
                    self.newline();
                }
                let shown = if byte.is_ascii() { byte } else { b'?' };
                write_cell(self.row, self.column, ATTRIBUTE | shown as u16);
                self.column += 1;
            }
        }
    }
}

/// Clears the screen and homes the cursor.
pub fn init() {
    for row in 0..ROWS {
        clear_row(row);
    }
    // SAFETY: boot, one CPU, interrupts off; nothing else can hold the
    // screen.
    let screen = unsafe { &mut *SCREEN.0.get() };
    screen.row = 0;
    screen.column = 0;
    move_cursor(0, 0);
}

/// Shows one byte. The caller holds the console lock (see `Shared`).
pub fn putc(byte: u8) {
    // SAFETY: serialised by the console lock, as `Shared` documents.
    let screen = unsafe { &mut *SCREEN.0.get() };
    screen.put(byte);
    if byte == b'\n' {
        move_cursor(screen.row, screen.column);
    }
}
