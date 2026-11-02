//! Virtio for Sprout user drivers: the modern (version 2) virtio-mmio
//! transport, one split virtqueue, and the virtio-blk request format.
//! Spec: <https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html>.
//!
//! The caller owns the memory: a mapped register window (`device_map`) and
//! a contiguous buffer whose physical address it knows (`vmo_physical`).
//! This crate never allocates and never talks to the kernel.

#![no_std]

use core::sync::atomic::{Ordering, fence};

/// Register offsets, virtio-mmio version 2.
mod reg {
    pub const MAGIC: usize = 0x000;
    pub const VERSION: usize = 0x004;
    pub const DEVICE_ID: usize = 0x008;
    pub const DEVICE_FEATURES: usize = 0x010;
    pub const DEVICE_FEATURES_SEL: usize = 0x014;
    pub const DRIVER_FEATURES: usize = 0x020;
    pub const DRIVER_FEATURES_SEL: usize = 0x024;
    pub const QUEUE_SEL: usize = 0x030;
    pub const QUEUE_NUM_MAX: usize = 0x034;
    pub const QUEUE_NUM: usize = 0x038;
    pub const QUEUE_READY: usize = 0x044;
    pub const QUEUE_NOTIFY: usize = 0x050;
    pub const INTERRUPT_STATUS: usize = 0x060;
    pub const INTERRUPT_ACK: usize = 0x064;
    pub const STATUS: usize = 0x070;
    pub const QUEUE_DESC_LOW: usize = 0x080;
    pub const QUEUE_DESC_HIGH: usize = 0x084;
    pub const QUEUE_DRIVER_LOW: usize = 0x090;
    pub const QUEUE_DRIVER_HIGH: usize = 0x094;
    pub const QUEUE_DEVICE_LOW: usize = 0x0a0;
    pub const QUEUE_DEVICE_HIGH: usize = 0x0a4;
    pub const CONFIG: usize = 0x100;
}

pub const MAGIC: u32 = 0x7472_6976;
pub const DEVICE_ID_BLOCK: u32 = 2;

mod status {
    pub const ACKNOWLEDGE: u32 = 1;
    pub const DRIVER: u32 = 2;
    pub const DRIVER_OK: u32 = 4;
    pub const FEATURES_OK: u32 = 8;
    pub const FAILED: u32 = 128;
}

/// Feature bit 32: the device speaks the modern (version 1) protocol.
const FEATURE_VERSION_1: u64 = 1 << 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    BadMagic(u32),
    UnsupportedVersion(u32),
    WrongDevice { wanted: u32, found: u32 },
    FeaturesRejected,
    QueueTooSmall(u32),
    QueueFull,
    RequestFailed(u8),
}

/// A mapped virtio-mmio register window.
pub struct Mmio {
    base: usize,
}

impl Mmio {
    /// Wraps the register window mapped at `base`.
    ///
    /// # Safety
    /// `base` must be the start of a virtio-mmio device's registers,
    /// mapped read-write in this process.
    pub unsafe fn new(base: usize) -> Mmio {
        Mmio { base }
    }

    fn read(&self, offset: usize) -> u32 {
        // SAFETY: `new`'s contract makes `base + offset` a mapped register.
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    fn write(&self, offset: usize, value: u32) {
        // SAFETY: as for `read`.
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, value) };
    }

    pub fn device_id(&self) -> u32 {
        self.read(reg::DEVICE_ID)
    }

    /// Checks the device is the kind we want and runs the status
    /// handshake up to `FEATURES_OK`, accepting only `VERSION_1`.
    pub fn initialize(&self, wanted_device: u32) -> Result<(), VirtioError> {
        let magic = self.read(reg::MAGIC);
        if magic != MAGIC {
            return Err(VirtioError::BadMagic(magic));
        }
        let version = self.read(reg::VERSION);
        if version != 2 {
            return Err(VirtioError::UnsupportedVersion(version));
        }
        let found = self.device_id();
        if found != wanted_device {
            return Err(VirtioError::WrongDevice {
                wanted: wanted_device,
                found,
            });
        }

        self.write(reg::STATUS, 0);
        self.write(reg::STATUS, status::ACKNOWLEDGE);
        self.write(reg::STATUS, status::ACKNOWLEDGE | status::DRIVER);

        let mut offered = 0u64;
        for sel in 0..2u32 {
            self.write(reg::DEVICE_FEATURES_SEL, sel);
            offered |= (self.read(reg::DEVICE_FEATURES) as u64) << (32 * sel);
        }
        let accepted = offered & FEATURE_VERSION_1;
        for sel in 0..2u32 {
            self.write(reg::DRIVER_FEATURES_SEL, sel);
            self.write(reg::DRIVER_FEATURES, (accepted >> (32 * sel)) as u32);
        }
        self.write(
            reg::STATUS,
            status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK,
        );
        if self.read(reg::STATUS) & status::FEATURES_OK == 0 {
            self.write(reg::STATUS, status::FAILED);
            return Err(VirtioError::FeaturesRejected);
        }
        Ok(())
    }

    /// Tells the device where queue `index` lives and marks it ready.
    fn install_queue(&self, index: u32, queue: &Virtqueue) -> Result<(), VirtioError> {
        self.write(reg::QUEUE_SEL, index);
        let max = self.read(reg::QUEUE_NUM_MAX);
        if max < QUEUE_SIZE as u32 {
            return Err(VirtioError::QueueTooSmall(max));
        }
        self.write(reg::QUEUE_NUM, QUEUE_SIZE as u32);
        let (desc, avail, used) = queue.physical_parts();
        self.write(reg::QUEUE_DESC_LOW, desc as u32);
        self.write(reg::QUEUE_DESC_HIGH, (desc >> 32) as u32);
        self.write(reg::QUEUE_DRIVER_LOW, avail as u32);
        self.write(reg::QUEUE_DRIVER_HIGH, (avail >> 32) as u32);
        self.write(reg::QUEUE_DEVICE_LOW, used as u32);
        self.write(reg::QUEUE_DEVICE_HIGH, (used >> 32) as u32);
        self.write(reg::QUEUE_READY, 1);
        Ok(())
    }

    /// Finishes the handshake: the driver is ready.
    fn driver_ok(&self) {
        self.write(
            reg::STATUS,
            status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK,
        );
    }

    fn notify(&self, queue: u32) {
        self.write(reg::QUEUE_NOTIFY, queue);
    }

    /// Reads and clears the interrupt status bits.
    pub fn ack_interrupt(&self) -> u32 {
        let bits = self.read(reg::INTERRUPT_STATUS);
        self.write(reg::INTERRUPT_ACK, bits);
        bits
    }

    /// A 32-bit word of device configuration space.
    pub fn config_u32(&self, offset: usize) -> u32 {
        self.read(reg::CONFIG + offset)
    }
}

/// Entries per queue. Small on purpose: one request at a time is plenty
/// for a first driver, and the whole queue fits in a page.
pub const QUEUE_SIZE: usize = 8;

const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Byte offsets of the three rings inside the queue memory. The used ring
/// needs 4-byte alignment; everything here is page-relative and generous.
const DESC_OFFSET: usize = 0;
const AVAIL_OFFSET: usize = DESC_OFFSET + QUEUE_SIZE * 16;
const USED_OFFSET: usize = 1024;
/// Bytes a queue needs: fits in one page.
pub const QUEUE_BYTES: usize = 2048;

/// One split virtqueue over caller-provided memory. Descriptors are handed
/// out in order and the queue is drained after each request, so there is
/// no free list.
pub struct Virtqueue {
    base: usize,
    physical: u64,
    next_desc: u16,
    avail_idx: u16,
    last_used: u16,
}

impl Virtqueue {
    /// Wraps `QUEUE_BYTES` of zeroed memory at `base` whose physical
    /// address is `physical`.
    ///
    /// # Safety
    /// The memory must be mapped, zeroed, physically contiguous, and used
    /// by nothing else.
    pub unsafe fn new(base: usize, physical: u64) -> Virtqueue {
        Virtqueue {
            base,
            physical,
            next_desc: 0,
            avail_idx: 0,
            last_used: 0,
        }
    }

    fn physical_parts(&self) -> (u64, u64, u64) {
        (
            self.physical + DESC_OFFSET as u64,
            self.physical + AVAIL_OFFSET as u64,
            self.physical + USED_OFFSET as u64,
        )
    }

    fn desc_ptr(&self, i: u16) -> *mut Desc {
        (self.base + DESC_OFFSET + i as usize * 16) as *mut Desc
    }

    fn write_u16(&self, offset: usize, value: u16) {
        // SAFETY: inside the queue memory per `new`'s contract.
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u16, value) };
    }

    fn read_u16(&self, offset: usize) -> u16 {
        // SAFETY: as above.
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u16) }
    }

    /// Queues a chain of buffers as one request. Each entry is
    /// `(physical address, length, device_writes)`.
    fn push_chain(&mut self, buffers: &[(u64, u32, bool)]) -> Result<u16, VirtioError> {
        if buffers.is_empty() || buffers.len() > QUEUE_SIZE {
            return Err(VirtioError::QueueFull);
        }
        let head = self.next_desc;
        for (i, &(addr, len, writable)) in buffers.iter().enumerate() {
            let index = (head + i as u16) % QUEUE_SIZE as u16;
            let last = i + 1 == buffers.len();
            let mut flags = if writable { DESC_F_WRITE } else { 0 };
            if !last {
                flags |= DESC_F_NEXT;
            }
            let desc = Desc {
                addr,
                len,
                flags,
                next: if last {
                    0
                } else {
                    (index + 1) % QUEUE_SIZE as u16
                },
            };
            // SAFETY: `desc_ptr` is inside the descriptor table.
            unsafe { core::ptr::write_volatile(self.desc_ptr(index), desc) };
        }
        self.next_desc = (head + buffers.len() as u16) % QUEUE_SIZE as u16;

        // avail: flags u16, idx u16, ring[QUEUE_SIZE] u16
        let slot = AVAIL_OFFSET + 4 + (self.avail_idx as usize % QUEUE_SIZE) * 2;
        self.write_u16(slot, head);
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.write_u16(AVAIL_OFFSET + 2, self.avail_idx);
        fence(Ordering::SeqCst);
        Ok(head)
    }

    /// Whether the device has finished something we have not collected.
    pub fn has_used(&self) -> bool {
        fence(Ordering::SeqCst);
        self.read_u16(USED_OFFSET + 2) != self.last_used
    }

    /// Collects one completed request; returns its head descriptor.
    fn pop_used(&mut self) -> Option<u16> {
        if !self.has_used() {
            return None;
        }
        // used: flags u16, idx u16, ring[QUEUE_SIZE] of { id u32, len u32 }
        let slot = USED_OFFSET + 4 + (self.last_used as usize % QUEUE_SIZE) * 8;
        // SAFETY: inside the used ring.
        let id = unsafe { core::ptr::read_volatile((self.base + slot) as *const u32) };
        self.last_used = self.last_used.wrapping_add(1);
        Some(id as u16)
    }
}

/// virtio-blk request header.
#[repr(C)]
#[derive(Clone, Copy)]
struct BlkRequestHeader {
    kind: u32,
    reserved: u32,
    sector: u64,
}

const BLK_T_IN: u32 = 0;
const BLK_T_OUT: u32 = 1;
const BLK_S_OK: u8 = 0;

/// Bytes per sector in the virtio-blk protocol.
pub const SECTOR_SIZE: usize = 512;

/// Layout of the request buffer the caller provides: header, one sector
/// of data, one status byte. Fits in one page with room to spare.
const REQ_HEADER_OFFSET: usize = 0;
const REQ_DATA_OFFSET: usize = 16;
const REQ_STATUS_OFFSET: usize = REQ_DATA_OFFSET + SECTOR_SIZE;
pub const REQUEST_BYTES: usize = REQ_STATUS_OFFSET + 1;

/// A virtio-blk device with a single queue, doing one sector at a time.
pub struct BlockDevice {
    mmio: Mmio,
    queue: Virtqueue,
    request: usize,
    request_physical: u64,
}

impl BlockDevice {
    /// Brings the device up. `queue_*` and `request_*` are two separate
    /// regions of caller memory: `QUEUE_BYTES` for the queue and
    /// `REQUEST_BYTES` for the request buffer, both physically contiguous.
    ///
    /// # Safety
    /// See [`Mmio::new`] and [`Virtqueue::new`]; the request region must
    /// be mapped, physically contiguous and otherwise unused.
    pub unsafe fn new(
        mmio: Mmio,
        queue_base: usize,
        queue_physical: u64,
        request_base: usize,
        request_physical: u64,
    ) -> Result<BlockDevice, VirtioError> {
        mmio.initialize(DEVICE_ID_BLOCK)?;
        // SAFETY: forwarded from this function's contract.
        let queue = unsafe { Virtqueue::new(queue_base, queue_physical) };
        mmio.install_queue(0, &queue)?;
        mmio.driver_ok();
        Ok(BlockDevice {
            mmio,
            queue,
            request: request_base,
            request_physical,
        })
    }

    /// Capacity in sectors, from the device's configuration space.
    pub fn capacity_sectors(&self) -> u64 {
        let low = self.mmio.config_u32(0) as u64;
        let high = self.mmio.config_u32(4) as u64;
        (high << 32) | low
    }

    /// Submits a one-sector read or write. The caller then waits for the
    /// device's interrupt (or polls [`Self::poll_complete`]).
    pub fn submit(
        &mut self,
        sector: u64,
        write: bool,
        data: &[u8; SECTOR_SIZE],
    ) -> Result<(), VirtioError> {
        let header = BlkRequestHeader {
            kind: if write { BLK_T_OUT } else { BLK_T_IN },
            reserved: 0,
            sector,
        };
        // SAFETY: the request region is ours per `new`'s contract.
        unsafe {
            core::ptr::write_volatile(
                (self.request + REQ_HEADER_OFFSET) as *mut BlkRequestHeader,
                header,
            );
            core::ptr::write_volatile((self.request + REQ_STATUS_OFFSET) as *mut u8, 0xff);
            if write {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    (self.request + REQ_DATA_OFFSET) as *mut u8,
                    SECTOR_SIZE,
                );
            }
        }
        let p = self.request_physical;
        self.queue.push_chain(&[
            (p + REQ_HEADER_OFFSET as u64, 16, false),
            (p + REQ_DATA_OFFSET as u64, SECTOR_SIZE as u32, !write),
            (p + REQ_STATUS_OFFSET as u64, 1, true),
        ])?;
        self.mmio.notify(0);
        Ok(())
    }

    /// Acknowledges the device interrupt and, if the request finished,
    /// returns its data (for a read) or `Ok(None)` if still in flight.
    pub fn poll_complete(
        &mut self,
        out: &mut [u8; SECTOR_SIZE],
    ) -> Result<Option<()>, VirtioError> {
        self.mmio.ack_interrupt();
        if self.queue.pop_used().is_none() {
            return Ok(None);
        }
        // SAFETY: the request region is ours; the device wrote status and data.
        let status =
            unsafe { core::ptr::read_volatile((self.request + REQ_STATUS_OFFSET) as *const u8) };
        if status != BLK_S_OK {
            return Err(VirtioError::RequestFailed(status));
        }
        // SAFETY: as above.
        unsafe {
            core::ptr::copy_nonoverlapping(
                (self.request + REQ_DATA_OFFSET) as *const u8,
                out.as_mut_ptr(),
                SECTOR_SIZE,
            );
        }
        Ok(Some(()))
    }
}
