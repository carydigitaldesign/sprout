//! `blkd`: a virtio-blk driver running as an ordinary process.
//!
//! Its first message carries a device handle. It maps the device, brings
//! it up over the modern virtio-mmio transport, reads sector 0, writes
//! sector 1 and reads it back, each request completed by the device's
//! interrupt. It answers `ok capacity=<sectors>` or `fail: <why>` on its
//! channel and exits.

#![no_std]
#![no_main]

use sprout_rt as rt;
use sprout_virtio::{BlockDevice, Mmio, QUEUE_BYTES, SECTOR_SIZE};

/// Byte offset of the request buffer inside the DMA page; the queue
/// occupies the first `QUEUE_BYTES`.
const REQUEST_OFFSET: usize = QUEUE_BYTES;

fn fail(why: &str) -> i32 {
    rt::log!("blkd: {why}");
    let mut s = rt::StackString::<128>::new();
    let _ = core::fmt::Write::write_fmt(&mut s, format_args!("fail: {why}"));
    let _ = rt::channel_send(rt::HANDLE_BOOTSTRAP, s.as_str().as_bytes(), &[]);
    1
}

/// Submits one request and waits for the device's interrupt to complete it.
fn transfer(
    blk: &mut BlockDevice,
    device: rt::Handle,
    sector: u64,
    write: bool,
    data: &mut [u8; SECTOR_SIZE],
) -> Result<(), &'static str> {
    blk.submit(sector, write, data)
        .map_err(|_| "submit failed")?;
    let irqs = rt::device_irq_wait(device, 2000).map_err(|_| "no interrupt within 2 s")?;
    let done = blk
        .poll_complete(data)
        .map_err(|_| "device reported an error")?;
    rt::device_irq_ack(device).map_err(|_| "irq ack failed")?;
    if done.is_none() {
        return Err("interrupt fired but the request was not complete");
    }
    rt::log!(
        "blkd: sector {sector} {} after {irqs} interrupt(s)",
        if write { "written" } else { "read" }
    );
    Ok(())
}

fn run() -> Result<u64, &'static str> {
    let mut bytes = [0u8; 64];
    let mut handles = [0 as rt::Handle; 1];
    let (_, nh) = rt::channel_recv(
        rt::HANDLE_BOOTSTRAP,
        &mut bytes,
        &mut handles,
        rt::TIMEOUT_INFINITE,
    )
    .map_err(|_| "no device message")?;
    if nh != 1 {
        return Err("first message had no device handle");
    }
    let device = handles[0];

    let regs = rt::device_map(device).map_err(|_| "device_map failed")?;
    let dma = rt::vmo_create_contiguous(4096).map_err(|_| "no contiguous memory")?;
    let dma_va = rt::vmo_map(dma).map_err(|_| "vmo_map failed")?;
    let dma_pa = rt::vmo_physical(dma, 0).map_err(|_| "vmo_physical failed")? as u64;
    rt::log!("blkd: registers at {regs:#x}, dma page at {dma_va:#x} (physical {dma_pa:#x})");

    // SAFETY: `regs` is the device window the kernel just mapped for us;
    // the DMA page is ours, zeroed, contiguous, mapped at `dma_va`.
    let mut blk = unsafe {
        let mmio = Mmio::new(regs);
        BlockDevice::new(
            mmio,
            dma_va,
            dma_pa,
            dma_va + REQUEST_OFFSET,
            dma_pa + REQUEST_OFFSET as u64,
        )
    }
    .map_err(|_| "virtio handshake failed")?;
    let capacity = blk.capacity_sectors();
    rt::log!("blkd: virtio-blk up, {capacity} sectors");

    let mut sector = [0u8; SECTOR_SIZE];
    transfer(&mut blk, device, 0, false, &mut sector)?;
    if !sector.starts_with(b"SPROUT-DISK") {
        return Err("sector 0 does not carry the disk image's magic");
    }

    let mut out = [0u8; SECTOR_SIZE];
    out[..15].copy_from_slice(b"hello from blkd");
    transfer(&mut blk, device, 1, true, &mut out)?;
    let mut back = [0u8; SECTOR_SIZE];
    transfer(&mut blk, device, 1, false, &mut back)?;
    if back != out {
        return Err("sector 1 read back differently from what was written");
    }
    Ok(capacity)
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(capacity) => {
            let mut s = rt::StackString::<64>::new();
            let _ = core::fmt::Write::write_fmt(&mut s, format_args!("ok capacity={capacity}"));
            let _ = rt::channel_send(rt::HANDLE_BOOTSTRAP, s.as_str().as_bytes(), &[]);
            0
        }
        Err(why) => fail(why),
    }
}
