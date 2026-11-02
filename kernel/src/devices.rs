//! Hardware discovery: walks the device tree for virtio-mmio transports,
//! probes each for a device behind it, and registers what it finds as
//! [`Device`] objects for user drivers.

use crate::arch::phys_to_virt;
use crate::fdt::{Fdt, Token, be32, be64};
use crate::mm;
use crate::obj::device::Device;

const VIRTIO_MAGIC: u32 = 0x7472_6976;
const REG_MAGIC: usize = 0x000;
const REG_VERSION: usize = 0x004;
const REG_DEVICE_ID: usize = 0x008;

/// Deepest node nesting the walk keeps track of. QEMU's tree is three deep.
const MAX_DEPTH: usize = 8;

#[derive(Clone, Copy, Default)]
struct Node<'a> {
    name: &'a str,
    reg: Option<&'a [u8]>,
    interrupts: Option<&'a [u8]>,
    compatible: Option<&'a [u8]>,
}

fn mmio_read(base: usize, offset: usize) -> u32 {
    // SAFETY: the first physical GiB (where virt's MMIO lives) is mapped
    // read-write in the direct map; a virtio-mmio register read has no
    // side effect for these three registers.
    unsafe { core::ptr::read_volatile(phys_to_virt(base + offset) as *const u32) }
}

fn consider(node: &Node) {
    let compatible = node.compatible.unwrap_or(&[]);
    if !compatible.split(|&b| b == 0).any(|s| s == b"virtio,mmio") {
        return;
    }
    let (Some(reg), Some(interrupts)) = (node.reg, node.interrupts) else {
        return;
    };
    // QEMU virt: #address-cells = 2, #size-cells = 2, one interrupt cell.
    let (Some(base), Some(size), Some(irq)) = (be64(reg, 0), be64(reg, 8), be32(interrupts, 0))
    else {
        return;
    };
    let (base, size) = (base as usize, size as usize);
    if mmio_read(base, REG_MAGIC) != VIRTIO_MAGIC {
        return;
    }
    let device_id = mmio_read(base, REG_DEVICE_ID);
    if device_id == 0 {
        return; // an empty slot
    }
    let _version = mmio_read(base, REG_VERSION);
    let _ = node.name;
    Device::register("virtio-mmio", base, size, irq, device_id);
}

/// Finds and registers devices. Runs after memory and interrupts are up.
pub fn init() {
    let Some(dtb) = mm::boot_info().device_tree else {
        // No device tree (x86_64): no devices until PCI discovery exists.
        return;
    };
    // SAFETY: the device tree blob is reserved and mapped for the kernel's
    // whole life (see `mm::init`).
    let fdt = unsafe { Fdt::from_ptr(phys_to_virt(dtb) as *const u8) }
        .expect("device tree parsed at boot");

    // Devices may sit anywhere in the tree (QEMU puts them under /soc), so
    // keep a node per level and consider each one as it closes.
    let mut stack: [Option<Node>; MAX_DEPTH] = [None; MAX_DEPTH];
    let mut depth = 0usize;
    for token in fdt.tokens() {
        match token {
            Token::BeginNode(name) => {
                if depth < MAX_DEPTH {
                    stack[depth] = Some(Node {
                        name,
                        ..Node::default()
                    });
                }
                depth += 1;
            }
            Token::EndNode => {
                depth = depth.saturating_sub(1);
                if depth < MAX_DEPTH
                    && let Some(node) = stack[depth].take()
                {
                    consider(&node);
                }
            }
            Token::Prop { name, value } => {
                if depth >= 1
                    && depth <= MAX_DEPTH
                    && let Some(node) = stack[depth - 1].as_mut()
                {
                    match name {
                        "reg" => node.reg = Some(value),
                        "interrupts" => node.interrupts = Some(value),
                        "compatible" => node.compatible = Some(value),
                        _ => {}
                    }
                }
            }
            Token::End => break,
        }
    }
}

#[cfg(target_arch = "riscv64")]
mod riscv_tests {
    crate::ktest! {
        fn a_virtio_block_device_is_found() {
            let devices = crate::obj::device::all();
            let block = devices
                .iter()
                .find(|d| d.virtio_id == 2)
                .expect("xtask attaches a virtio-blk device");
            assert_eq!(block.kind, "virtio-mmio");
            assert!(block.base >= 0x1000_1000 && block.base < 0x1000_9000);
            assert!(block.irq >= 1 && block.irq <= 8);
            assert_eq!(block.pending_irqs(), 0);
        }
    }
}
