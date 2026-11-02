//! What the architecture hands the portable kernel at boot: where RAM is,
//! what to keep off, how fast the clock runs, and where the device tree is
//! if there is one. Built by `arch::handoff()` from the firmware's data
//! (a device tree on riscv64, Multiboot information on x86_64).

/// A physical address range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub base: usize,
    pub size: usize,
}

impl Region {
    pub const fn end(self) -> usize {
        self.base + self.size
    }
}

/// Most RAM or reserved regions a hand-off carries.
pub const MAX_REGIONS: usize = 4;

#[derive(Debug, Clone, Copy)]
pub struct Handoff {
    /// Usable RAM, in address order. The kernel image lives in the first.
    pub ram: [Option<Region>; MAX_REGIONS],
    /// Ranges inside RAM the kernel must not hand out (firmware data).
    pub reserved: [Option<Region>; MAX_REGIONS],
    /// Rate of `arch::timer_cycles()`.
    pub timebase_hz: u64,
    /// Physical address of a flattened device tree, if the platform has one.
    pub device_tree: Option<usize>,
}

impl Handoff {
    pub const fn empty() -> Handoff {
        Handoff {
            ram: [None; MAX_REGIONS],
            reserved: [None; MAX_REGIONS],
            timebase_hz: 0,
            device_tree: None,
        }
    }

    /// The first RAM region, which holds the kernel.
    pub fn main_ram(&self) -> Option<Region> {
        self.ram[0]
    }
}
