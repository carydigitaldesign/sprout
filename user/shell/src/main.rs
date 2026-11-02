//! `shell`: reads commands from its bootstrap channel and answers on it.
//!
//! Commands are one message each:
//! - `ping` answers `pong`
//! - `echo <text>` answers `<text>`
//! - `read-vmo` with one VMO handle attached maps it and answers with the
//!   text up to the first NUL
//! - `describe` answers with the shell's own process description
//! - `exit` ends the shell with code 0
//! Anything else answers `unknown command`.

#![no_std]
#![no_main]

use sprout_rt as rt;

fn reply(text: &[u8]) {
    if let Err(e) = rt::channel_send(rt::HANDLE_BOOTSTRAP, text, &[]) {
        rt::log!("shell: reply failed: {}", e.name());
    }
}

fn read_vmo(handle: rt::Handle) {
    let addr = match rt::vmo_map(handle) {
        Ok(a) => a,
        Err(e) => {
            rt::log!("shell: vmo_map failed: {}", e.name());
            reply(b"vmo_map failed");
            return;
        }
    };
    // SAFETY: the kernel just mapped the whole VMO at `addr`, readable, and
    // a VMO is at least one page; we stop at the first NUL or 4096 bytes.
    let page = unsafe { core::slice::from_raw_parts(addr as *const u8, 4096) };
    let len = page.iter().position(|&b| b == 0).unwrap_or(page.len());
    reply(&page[..len]);
    let _ = rt::vmo_unmap(addr);
    let _ = rt::handle_close(handle);
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    rt::log("shell: ready");
    let mut bytes = [0u8; rt::CHANNEL_MAX_BYTES];
    let mut handles = [0 as rt::Handle; rt::CHANNEL_MAX_HANDLES];
    loop {
        let (n, nh) = match rt::channel_recv(
            rt::HANDLE_BOOTSTRAP,
            &mut bytes,
            &mut handles,
            rt::TIMEOUT_INFINITE,
        ) {
            Ok(got) => got,
            Err(rt::Error::PeerClosed) => {
                rt::log("shell: peer closed, leaving");
                return 0;
            }
            Err(e) => {
                rt::log!("shell: recv failed: {}", e.name());
                return 2;
            }
        };
        let line = core::str::from_utf8(&bytes[..n]).unwrap_or("");
        rt::log!("shell: got {line:?} with {nh} handle(s)");
        match line {
            "ping" => reply(b"pong"),
            "exit" => return 0,
            "describe" => {
                let mut buf = [0u8; 1024];
                match rt::describe(rt::HANDLE_SELF_PROCESS, &mut buf) {
                    Ok(len) => reply(&buf[..len.min(buf.len())]),
                    Err(e) => reply(e.name().as_bytes()),
                }
            }
            "read-vmo" if nh == 1 => read_vmo(handles[0]),
            _ if line.starts_with("echo ") => reply(&bytes[5..n]),
            _ => {
                for &h in &handles[..nh] {
                    let _ = rt::handle_close(h);
                }
                reply(b"unknown command");
            }
        }
    }
}
