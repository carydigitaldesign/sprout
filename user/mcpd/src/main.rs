//! `mcpd`: the Model Context Protocol served from inside Sprout.
//!
//! It drives the virtio-net device, runs a TCP/IP stack on it (`smoltcp`,
//! fixed buffers, no heap), accepts HTTP on port 80 and answers JSON-RPC
//! per MCP's streamable HTTP transport: `initialize`, `tools/list`,
//! `tools/call`, `ping`. Its tools reach into the running OS: the kernel's
//! own description, the handles this process holds, a shell it was given
//! a channel to, and the kernel log.
//!
//! Setup over its bootstrap channel: a `net` message carrying the device
//! handle, then a `shell` message carrying a channel to a shell. `exit`
//! stops it (the test kernel asks for that).

#![no_std]
#![no_main]

mod json;
mod net;

use core::cell::UnsafeCell;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet, SocketStorage};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use sprout_rt as rt;
use sprout_virtio::{Mmio, NET_BUFFERS_BYTES, NET_QUEUES_BYTES, NetDevice};

use json::{Buf, raw_field, str_field, unescape};
use net::Phy;

const HTTP_PORT: u16 = 80;
/// QEMU user networking: the guest is 10.0.2.15, the gateway 10.0.2.2.
const IP: [u8; 4] = [10, 0, 2, 15];
const GATEWAY: [u8; 4] = [10, 0, 2, 2];
const PREFIX_LEN: u8 = 24;
/// Longest time the loop sleeps on the NIC before looking around again.
const POLL_MS: u64 = 10;
/// The MCP protocol revision this server speaks when the client names none.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Connections handled at once. One is always listening while others
/// finish closing, so back-to-back requests never find nobody home.
const CONNS: usize = 3;
const REQUEST_BYTES: usize = 8192;
const RESPONSE_BYTES: usize = 12288;
const SOCKET_RX_BYTES: usize = 8192;
const SOCKET_TX_BYTES: usize = 16384;

/// Memory that must outlive the stack: one owner, one thread, taken once.
struct Global<T>(UnsafeCell<T>);

// SAFETY: this process has one thread, and every `Global` is borrowed
// exactly once, at startup, by `take`.
unsafe impl<T> Sync for Global<T> {}

impl<T> Global<T> {
    /// # Safety
    /// Call at most once per `Global`.
    unsafe fn take(&'static self) -> &'static mut T {
        // SAFETY: forwarded from this function's contract.
        unsafe { &mut *self.0.get() }
    }
}

static SOCKET_RX: Global<[[u8; SOCKET_RX_BYTES]; CONNS]> =
    Global(UnsafeCell::new([[0; SOCKET_RX_BYTES]; CONNS]));
static SOCKET_TX: Global<[[u8; SOCKET_TX_BYTES]; CONNS]> =
    Global(UnsafeCell::new([[0; SOCKET_TX_BYTES]; CONNS]));
static SOCKETS: Global<[SocketStorage<'static>; CONNS]> =
    Global(UnsafeCell::new([SocketStorage::EMPTY; CONNS]));
static REQUESTS: Global<[Buf<REQUEST_BYTES>; CONNS]> =
    Global(UnsafeCell::new([const { Buf::new() }; CONNS]));
static RESPONSES: Global<[Buf<RESPONSE_BYTES>; CONNS]> =
    Global(UnsafeCell::new([const { Buf::new() }; CONNS]));
static SCRATCH: Global<[u8; 8192]> = Global(UnsafeCell::new([0; 8192]));

/// What the tools work with.
struct Tools {
    shell: rt::Handle,
}

fn now() -> Instant {
    Instant::from_millis((rt::clock_ns() / 1_000_000) as i64)
}

/// Receives the two setup messages and brings the device up.
fn setup() -> Result<(rt::Handle, Phy, Tools), &'static str> {
    let mut bytes = [0u8; 64];
    let mut handles = [0 as rt::Handle; 1];
    let mut device = None;
    let mut shell = None;
    while device.is_none() || shell.is_none() {
        let (n, nh) = rt::channel_recv(rt::HANDLE_BOOTSTRAP, &mut bytes, &mut handles, 5000)
            .map_err(|_| "setup message did not arrive")?;
        if nh != 1 {
            return Err("setup message without a handle");
        }
        match &bytes[..n] {
            b"net" => device = Some(handles[0]),
            b"shell" => shell = Some(handles[0]),
            _ => return Err("unexpected setup message"),
        }
    }
    let (device, shell) = (device.unwrap(), shell.unwrap());

    let regs = rt::device_map(device).map_err(|_| "device_map failed")?;
    let dma = rt::vmo_create_contiguous(NET_QUEUES_BYTES + NET_BUFFERS_BYTES)
        .map_err(|_| "no contiguous memory")?;
    let dma_va = rt::vmo_map(dma).map_err(|_| "vmo_map failed")?;
    let dma_pa = rt::vmo_physical(dma, 0).map_err(|_| "vmo_physical failed")? as u64;
    // SAFETY: `regs` is the device window the kernel mapped for us; the
    // DMA region is ours, zeroed, contiguous and mapped at `dma_va`.
    let dev = unsafe {
        NetDevice::new(
            Mmio::new(regs),
            dma_va,
            dma_pa,
            dma_va + NET_QUEUES_BYTES,
            dma_pa + NET_QUEUES_BYTES as u64,
        )
    }
    .map_err(|_| "virtio-net handshake failed")?;
    let mac = dev.mac();
    rt::log!(
        "mcpd: virtio-net up, mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    Ok((device, Phy { dev }, Tools { shell }))
}

/// Where the body starts and how long it is, once the whole request is in.
fn complete_request(req: &[u8]) -> Option<(usize, usize)> {
    let head_end = req.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head = &req[..head_end];
    let mut content_length = 0usize;
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() > 15 && line[..15].eq_ignore_ascii_case(b"content-length:") {
            let value = core::str::from_utf8(&line[15..]).ok()?.trim();
            content_length = value.parse().ok()?;
        }
    }
    (req.len() >= head_end + content_length).then_some((head_end, content_length))
}

/// Runs one tool and writes its text result into `out`. False if unknown.
fn call_tool(name: &[u8], body: &[u8], tools: &Tools, out: &mut Buf<RESPONSE_BYTES>) -> bool {
    // SAFETY: tools run one at a time; `SCRATCH` is only touched here.
    let scratch = unsafe { SCRATCH.take() };
    match name {
        b"kernel" => match rt::describe(rt::HANDLE_KERNEL, scratch) {
            Ok(n) => out.push_escaped(&scratch[..n.min(scratch.len())]),
            Err(e) => out.push_escaped(e.name().as_bytes()),
        },
        b"handles" => {
            let mut any = false;
            for handle in 1..16 {
                if let Ok(n) = rt::describe(handle, scratch) {
                    if any {
                        out.push(b"\n");
                    }
                    any = true;
                    out.push_escaped(&scratch[..n.min(scratch.len())]);
                }
            }
            true
        }
        b"shell" => {
            let Some(raw) = str_field(body, "command") else {
                return out.push_escaped(b"missing argument: command");
            };
            let len = unescape(raw, scratch);
            if rt::channel_send(tools.shell, &scratch[..len], &[]).is_err() {
                return out.push_escaped(b"the shell is gone");
            }
            let mut handles = [0 as rt::Handle; 1];
            match rt::channel_recv(tools.shell, scratch, &mut handles, 2000) {
                Ok((n, _)) => out.push_escaped(&scratch[..n]),
                Err(e) => out.push_escaped(e.name().as_bytes()),
            }
        }
        b"log" => {
            let Some(raw) = str_field(body, "message") else {
                return out.push_escaped(b"missing argument: message");
            };
            let len = unescape(raw, scratch).min(512);
            rt::log(core::str::from_utf8(&scratch[..len]).unwrap_or("<not utf-8>"));
            out.push_escaped(b"logged")
        }
        _ => false,
    }
}

const TOOLS_JSON: &str = r#"[{"name":"kernel","description":"The kernel's own description: name, version, mode, object types and every syscall with its schema.","inputSchema":{"type":"object","properties":{}}},{"name":"handles","description":"Describe every handle mcpd holds: its process, the channel to init, the network device, the channel to the shell.","inputSchema":{"type":"object","properties":{}}},{"name":"shell","description":"Run a command in the Sprout shell and return its reply. Commands: ping, echo <text>, describe, exit.","inputSchema":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}},{"name":"log","description":"Write a line to the kernel log, which the host sees on the serial console.","inputSchema":{"type":"object","properties":{"message":{"type":"string"}},"required":["message"]}}]"#;

enum Reply {
    Json,
    Accepted,
    BadRequest,
}

/// Answers one JSON-RPC message; `out` receives the response body.
fn handle_rpc(body: &[u8], tools: &Tools, out: &mut Buf<RESPONSE_BYTES>) -> Reply {
    let Some(method) = str_field(body, "method") else {
        return Reply::BadRequest;
    };
    let Some(id) = raw_field(body, "id") else {
        // A notification: nothing to answer.
        return Reply::Accepted;
    };
    if method.starts_with(b"notifications/") {
        return Reply::Accepted;
    }

    out.push(b"{\"jsonrpc\":\"2.0\",\"id\":");
    out.push(id);
    match method {
        b"initialize" => {
            let version = str_field(body, "protocolVersion").unwrap_or(PROTOCOL_VERSION.as_bytes());
            out.push(b",\"result\":{\"protocolVersion\":\"");
            out.push(version);
            out.push(b"\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"sprout\",\"version\":\"0.1.0\"},\"instructions\":\"This is the Sprout kernel, a capability microkernel, speaking from inside the running OS. Use kernel to learn the syscalls, handles to see what mcpd holds, shell to run commands.\"}}");
        }
        b"ping" => {
            out.push(b",\"result\":{}}");
        }
        b"tools/list" => {
            out.push(b",\"result\":{\"tools\":");
            out.push_str(TOOLS_JSON);
            out.push(b"}}");
        }
        b"tools/call" => {
            let name = str_field(body, "name").unwrap_or(b"");
            out.push(b",\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"");
            if call_tool(name, body, tools, out) {
                out.push(b"\"}]}}");
            } else {
                out.push(b"unknown tool\"}],\"isError\":true}}");
            }
        }
        _ => {
            out.push(b",\"error\":{\"code\":-32601,\"message\":\"method not found\"}}");
        }
    }
    Reply::Json
}

/// Builds the full HTTP response for a complete request into `out`.
fn handle_http(req: &[u8], tools: &Tools, out: &mut Buf<RESPONSE_BYTES>) {
    out.clear();
    let is_post = req.starts_with(b"POST ");
    let (head_end, content_length) = complete_request(req).unwrap_or((req.len(), 0));
    let body = &req[head_end..head_end + content_length];

    let mut rpc = Buf::<RESPONSE_BYTES>::new();
    let reply = if is_post {
        handle_rpc(body, tools, &mut rpc)
    } else {
        Reply::BadRequest
    };
    let (status, body): (&str, &[u8]) = match reply {
        Reply::Json => ("200 OK", rpc.as_bytes()),
        Reply::Accepted => ("202 Accepted", b""),
        Reply::BadRequest if is_post => {
            ("400 Bad Request", b"{\"error\":\"not a JSON-RPC request\"}")
        }
        Reply::BadRequest => ("405 Method Not Allowed", b"POST JSON-RPC here"),
    };
    let line_end = req
        .iter()
        .position(|&b| b == b'\r')
        .unwrap_or(req.len().min(60));
    rt::log!(
        "mcpd: {} -> {status}, {} bytes",
        core::str::from_utf8(&req[..line_end.min(60)]).unwrap_or("?"),
        body.len()
    );
    out.push(b"HTTP/1.1 ");
    out.push_str(status);
    out.push(b"\r\nContent-Type: application/json\r\nAllow: POST\r\nConnection: close\r\nContent-Length: ");
    out.push_num(body.len() as u64);
    out.push(b"\r\n\r\n");
    out.push(body);
}

/// Per-connection state besides the socket itself.
#[derive(Clone, Copy)]
struct Conn {
    sending: bool,
    sent: usize,
}

fn serve(device: rt::Handle, mut phy: Phy, tools: Tools) -> i32 {
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(phy.dev.mac())));
    config.random_seed = rt::clock_ns();
    let mut iface = Interface::new(config, &mut phy, now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(
            IpAddress::v4(IP[0], IP[1], IP[2], IP[3]),
            PREFIX_LEN,
        ));
    });
    let _ = iface.routes_mut().add_default_ipv4_route(Ipv4Address::new(
        GATEWAY[0], GATEWAY[1], GATEWAY[2], GATEWAY[3],
    ));

    // SAFETY: each `Global` is taken exactly once, here.
    let (rx, tx, storage, requests, responses) = unsafe {
        (
            SOCKET_RX.take(),
            SOCKET_TX.take(),
            SOCKETS.take(),
            REQUESTS.take(),
            RESPONSES.take(),
        )
    };
    let mut sockets = SocketSet::new(&mut storage[..]);
    let mut handles: [Option<SocketHandle>; CONNS] = [None; CONNS];
    for (i, (rx, tx)) in rx.iter_mut().zip(tx.iter_mut()).enumerate() {
        let socket = tcp::Socket::new(
            tcp::SocketBuffer::new(&mut rx[..]),
            tcp::SocketBuffer::new(&mut tx[..]),
        );
        handles[i] = Some(sockets.add(socket));
    }
    let mut conns = [Conn {
        sending: false,
        sent: 0,
    }; CONNS];

    rt::log!(
        "mcpd: listening on {}.{}.{}.{}:{HTTP_PORT}",
        IP[0],
        IP[1],
        IP[2],
        IP[3]
    );
    let _ = rt::channel_send(rt::HANDLE_BOOTSTRAP, b"ready", &[]);

    let mut control = [0u8; 16];
    let mut control_handles = [0 as rt::Handle; 1];
    loop {
        iface.poll(now(), &mut phy, &mut sockets);

        for i in 0..CONNS {
            let socket = sockets.get_mut::<tcp::Socket>(handles[i].expect("socket added"));
            let conn = &mut conns[i];
            let request = &mut requests[i];
            let response = &mut responses[i];

            if !socket.is_open() {
                let _ = socket.listen(HTTP_PORT);
                request.clear();
                conn.sending = false;
                conn.sent = 0;
            }

            if !conn.sending && socket.may_recv() {
                let mut chunk = [0u8; 512];
                while let Ok(n) = socket.recv_slice(&mut chunk) {
                    if n == 0 || !request.push(&chunk[..n]) {
                        break;
                    }
                }
                if complete_request(request.as_bytes()).is_some() || request.len() == REQUEST_BYTES
                {
                    handle_http(request.as_bytes(), &tools, response);
                    // One request per connection: anything more the client
                    // sends is ignored, and nothing is handled twice.
                    request.clear();
                    conn.sending = true;
                    conn.sent = 0;
                }
            }

            if conn.sending && socket.can_send() {
                if let Ok(n) = socket.send_slice(&response.as_bytes()[conn.sent..]) {
                    conn.sent += n;
                }
                if conn.sent == response.len() {
                    socket.close();
                    conn.sending = false;
                }
            }

            if !conn.sending && socket.is_active() && !socket.may_recv() && socket.may_send() {
                // The client went away mid-request.
                socket.close();
            }
        }

        if let Ok((n, _)) =
            rt::channel_recv(rt::HANDLE_BOOTSTRAP, &mut control, &mut control_handles, 0)
            && &control[..n] == b"exit"
        {
            rt::log("mcpd: asked to exit");
            return 0;
        }

        let _ = rt::device_irq_wait(device, POLL_MS);
        phy.dev.ack_interrupt();
        let _ = rt::device_irq_ack(device);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    match setup() {
        Ok((device, phy, tools)) => serve(device, phy, tools),
        Err(why) => {
            rt::log!("mcpd: {why}");
            let _ = rt::channel_send(rt::HANDLE_BOOTSTRAP, why.as_bytes(), &[]);
            1
        }
    }
}
