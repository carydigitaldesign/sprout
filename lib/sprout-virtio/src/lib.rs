//! Virtio for Sprout user drivers: the modern (version 2) virtio-mmio
//! transport, split virtqueues, and the block and network device
//! protocols. Spec: <https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html>.
//!
//! The caller owns the memory: a mapped register window (`device_map`) and
//! contiguous buffers whose physical addresses it knows (`vmo_physical`).
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
pub const DEVICE_ID_NET: u32 = 1;
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
/// virtio-net feature bit 5: the device has a MAC address in config space.
const FEATURE_NET_MAC: u64 = 1 << 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    BadMagic(u32),
    UnsupportedVersion(u32),
    WrongDevice { wanted: u32, found: u32 },
    FeaturesRejected,
    QueueTooSmall(u32),
    QueueFull,
    RequestFailed(u8),
    FrameTooLarge,
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
    /// handshake up to `FEATURES_OK`, accepting `VERSION_1` plus whatever
    /// of `wanted_features` the device offers. Returns what was accepted.
    pub fn initialize(&self, wanted_device: u32, wanted_features: u64) -> Result<u64, VirtioError> {
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
        let accepted = offered & (FEATURE_VERSION_1 | wanted_features);
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
        Ok(accepted)
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
        if bits != 0 {
            self.write(reg::INTERRUPT_ACK, bits);
        }
        bits
    }

    /// A 32-bit word of device configuration space.
    pub fn config_u32(&self, offset: usize) -> u32 {
        self.read(reg::CONFIG + offset)
    }

    /// One byte of device configuration space.
    pub fn config_u8(&self, offset: usize) -> u8 {
        // SAFETY: as for `read`; config space is byte-addressable.
        unsafe { core::ptr::read_volatile((self.base + reg::CONFIG + offset) as *const u8) }
    }
}

/// Entries per queue. Small on purpose: a handful of buffers in flight is
/// plenty for a first driver, and the whole queue fits in half a page.
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
/// Bytes a queue needs: two fit in one page.
pub const QUEUE_BYTES: usize = 2048;

/// One split virtqueue over caller-provided memory, with a free list of
/// descriptors so several chains can be in flight.
pub struct Virtqueue {
    base: usize,
    physical: u64,
    free: [u16; QUEUE_SIZE],
    free_count: usize,
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
        let mut free = [0u16; QUEUE_SIZE];
        for (i, slot) in free.iter_mut().enumerate() {
            *slot = (QUEUE_SIZE - 1 - i) as u16;
        }
        Virtqueue {
            base,
            physical,
            free,
            free_count: QUEUE_SIZE,
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

    fn read_desc(&self, i: u16) -> Desc {
        // SAFETY: inside the descriptor table per `new`'s contract.
        unsafe { core::ptr::read_volatile(self.desc_ptr(i)) }
    }

    fn write_u16(&self, offset: usize, value: u16) {
        // SAFETY: inside the queue memory per `new`'s contract.
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u16, value) };
    }

    fn read_u16(&self, offset: usize) -> u16 {
        // SAFETY: as above.
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u16) }
    }

    /// Descriptors not in flight.
    pub fn free_descriptors(&self) -> usize {
        self.free_count
    }

    /// Queues a chain of buffers as one request and returns its head.
    /// Each entry is `(physical address, length, device_writes)`.
    pub fn push_chain(&mut self, buffers: &[(u64, u32, bool)]) -> Result<u16, VirtioError> {
        if buffers.is_empty() || buffers.len() > self.free_count {
            return Err(VirtioError::QueueFull);
        }
        let mut indices = [0u16; QUEUE_SIZE];
        for slot in indices.iter_mut().take(buffers.len()) {
            self.free_count -= 1;
            *slot = self.free[self.free_count];
        }
        for (i, &(addr, len, writable)) in buffers.iter().enumerate() {
            let last = i + 1 == buffers.len();
            let mut flags = if writable { DESC_F_WRITE } else { 0 };
            if !last {
                flags |= DESC_F_NEXT;
            }
            let desc = Desc {
                addr,
                len,
                flags,
                next: if last { 0 } else { indices[i + 1] },
            };
            // SAFETY: `desc_ptr` is inside the descriptor table.
            unsafe { core::ptr::write_volatile(self.desc_ptr(indices[i]), desc) };
        }
        let head = indices[0];

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

    /// Collects one completed chain: its head and the bytes the device
    /// wrote. The chain's descriptors return to the free list.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        if !self.has_used() {
            return None;
        }
        // used: flags u16, idx u16, ring[QUEUE_SIZE] of { id u32, len u32 }
        let slot = USED_OFFSET + 4 + (self.last_used as usize % QUEUE_SIZE) * 8;
        // SAFETY: inside the used ring.
        let (id, len) = unsafe {
            (
                core::ptr::read_volatile((self.base + slot) as *const u32),
                core::ptr::read_volatile((self.base + slot + 4) as *const u32),
            )
        };
        self.last_used = self.last_used.wrapping_add(1);

        let head = id as u16;
        let mut index = head;
        loop {
            let desc = self.read_desc(index);
            if self.free_count < QUEUE_SIZE {
                self.free[self.free_count] = index;
                self.free_count += 1;
            }
            if desc.flags & DESC_F_NEXT == 0 {
                break;
            }
            index = desc.next;
        }
        Some((head, len))
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
        mmio.initialize(DEVICE_ID_BLOCK, 0)?;
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

/// Receive buffers kept posted to a network device.
pub const NET_RX_BUFFERS: usize = 4;
/// Bytes per network buffer: a 12-byte virtio-net header plus a full
/// Ethernet frame, rounded up.
pub const NET_BUFFER_BYTES: usize = 2048;
/// virtio-net header size with `VERSION_1`.
const NET_HEADER_BYTES: usize = 12;
/// Largest Ethernet frame a buffer carries.
pub const NET_MTU: usize = 1514;
/// Bytes of buffer memory a [`NetDevice`] needs: receive buffers plus one
/// transmit buffer.
pub const NET_BUFFERS_BYTES: usize = (NET_RX_BUFFERS + 1) * NET_BUFFER_BYTES;
/// Bytes of queue memory a [`NetDevice`] needs: receive and transmit queues.
pub const NET_QUEUES_BYTES: usize = 2 * QUEUE_BYTES;

/// A virtio-net device: queue 0 receives, queue 1 transmits, one transmit
/// in flight at a time.
pub struct NetDevice {
    mmio: Mmio,
    rx: Virtqueue,
    tx: Virtqueue,
    buffers: usize,
    buffers_physical: u64,
    /// Which receive buffer each descriptor head carries.
    rx_buffer_of: [u8; QUEUE_SIZE],
    tx_busy: bool,
    mac: [u8; 6],
}

impl NetDevice {
    /// Brings the device up and posts every receive buffer. `queues_*` is
    /// `NET_QUEUES_BYTES` of memory, `buffers_*` is `NET_BUFFERS_BYTES`.
    ///
    /// # Safety
    /// See [`Mmio::new`] and [`Virtqueue::new`]; both regions must be
    /// mapped, physically contiguous and otherwise unused.
    pub unsafe fn new(
        mmio: Mmio,
        queues_base: usize,
        queues_physical: u64,
        buffers_base: usize,
        buffers_physical: u64,
    ) -> Result<NetDevice, VirtioError> {
        let accepted = mmio.initialize(DEVICE_ID_NET, FEATURE_NET_MAC)?;
        // SAFETY: forwarded from this function's contract.
        let (rx, tx) = unsafe {
            (
                Virtqueue::new(queues_base, queues_physical),
                Virtqueue::new(
                    queues_base + QUEUE_BYTES,
                    queues_physical + QUEUE_BYTES as u64,
                ),
            )
        };
        mmio.install_queue(0, &rx)?;
        mmio.install_queue(1, &tx)?;
        mmio.driver_ok();

        let mut mac = [0u8; 6];
        if accepted & FEATURE_NET_MAC != 0 {
            for (i, b) in mac.iter_mut().enumerate() {
                *b = mmio.config_u8(i);
            }
        }
        let mut device = NetDevice {
            mmio,
            rx,
            tx,
            buffers: buffers_base,
            buffers_physical,
            rx_buffer_of: [0; QUEUE_SIZE],
            tx_busy: false,
            mac,
        };
        for buffer in 0..NET_RX_BUFFERS {
            device.post_rx(buffer)?;
        }
        device.mmio.notify(0);
        Ok(device)
    }

    /// The device's MAC address (zeros if it offered none).
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn buffer_va(&self, buffer: usize) -> usize {
        self.buffers + buffer * NET_BUFFER_BYTES
    }

    fn buffer_pa(&self, buffer: usize) -> u64 {
        self.buffers_physical + (buffer * NET_BUFFER_BYTES) as u64
    }

    fn post_rx(&mut self, buffer: usize) -> Result<(), VirtioError> {
        let head =
            self.rx
                .push_chain(&[(self.buffer_pa(buffer), NET_BUFFER_BYTES as u32, true)])?;
        self.rx_buffer_of[head as usize] = buffer as u8;
        Ok(())
    }

    /// A received frame, if any: `(buffer, frame length)`. Read it with
    /// [`Self::rx_frame`], then hand the buffer back with [`Self::recycle_rx`].
    pub fn receive(&mut self) -> Option<(usize, usize)> {
        let (head, len) = self.rx.pop_used()?;
        let buffer = self.rx_buffer_of[head as usize] as usize;
        let len = (len as usize).saturating_sub(NET_HEADER_BYTES);
        Some((buffer, len.min(NET_MTU)))
    }

    /// The Ethernet frame in a received buffer.
    pub fn rx_frame(&self, buffer: usize, len: usize) -> &[u8] {
        // SAFETY: the buffer region is ours per `new`'s contract and the
        // device finished writing it (the used ring said so).
        unsafe {
            core::slice::from_raw_parts(
                (self.buffer_va(buffer) + NET_HEADER_BYTES) as *const u8,
                len,
            )
        }
    }

    /// Posts a receive buffer again once its frame has been consumed.
    pub fn recycle_rx(&mut self, buffer: usize) {
        if self.post_rx(buffer).is_ok() {
            self.mmio.notify(0);
        }
    }

    /// Collects finished transmissions.
    pub fn reap_tx(&mut self) {
        while self.tx.pop_used().is_some() {
            self.tx_busy = false;
        }
    }

    /// Sends one Ethernet frame. Waits for the previous one to finish.
    pub fn transmit(&mut self, frame: &[u8]) -> Result<(), VirtioError> {
        if frame.len() > NET_MTU {
            return Err(VirtioError::FrameTooLarge);
        }
        while self.tx_busy {
            self.reap_tx();
            core::hint::spin_loop();
        }
        let buffer = NET_RX_BUFFERS;
        let va = self.buffer_va(buffer);
        // SAFETY: the transmit buffer is ours and idle.
        unsafe {
            core::ptr::write_bytes(va as *mut u8, 0, NET_HEADER_BYTES);
            core::ptr::copy_nonoverlapping(
                frame.as_ptr(),
                (va + NET_HEADER_BYTES) as *mut u8,
                frame.len(),
            );
        }
        self.tx.push_chain(&[(
            self.buffer_pa(buffer),
            (NET_HEADER_BYTES + frame.len()) as u32,
            false,
        )])?;
        self.tx_busy = true;
        self.mmio.notify(1);
        Ok(())
    }

    /// Reads and clears the device's interrupt status.
    pub fn ack_interrupt(&self) -> u32 {
        self.mmio.ack_interrupt()
    }
}
