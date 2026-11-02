//! The virtio-net device as a `smoltcp` link: frames copied out of the
//! receive buffers into a token, frames built into the transmit buffer.

use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use sprout_virtio::{NET_MTU, NetDevice};

pub struct Phy {
    pub dev: NetDevice,
}

/// A received frame, copied so the device buffer can go back on the ring
/// before the stack has finished with the data.
pub struct RxToken {
    frame: [u8; NET_MTU],
    len: usize,
}

pub struct TxToken<'a> {
    dev: &'a mut NetDevice,
}

impl Device for Phy {
    type RxToken<'a>
        = RxToken
    where
        Self: 'a;
    type TxToken<'a>
        = TxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, _now: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.dev.reap_tx();
        let (buffer, len) = self.dev.receive()?;
        let mut frame = [0u8; NET_MTU];
        frame[..len].copy_from_slice(self.dev.rx_frame(buffer, len));
        self.dev.recycle_rx(buffer);
        Some((RxToken { frame, len }, TxToken { dev: &mut self.dev }))
    }

    fn transmit(&mut self, _now: Instant) -> Option<Self::TxToken<'_>> {
        self.dev.reap_tx();
        Some(TxToken { dev: &mut self.dev })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = NET_MTU;
        caps.max_burst_size = Some(1);
        caps.medium = Medium::Ethernet;
        caps
    }
}

impl phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.frame[..self.len])
    }
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = [0u8; NET_MTU];
        let len = len.min(NET_MTU);
        let result = f(&mut buf[..len]);
        let _ = self.dev.transmit(&buf[..len]);
        result
    }
}
