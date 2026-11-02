//! `fsd`: the file service. Its first message carries the initramfs as a
//! VMO; after that each `open <name>` is answered with `ok` plus a fresh
//! VMO holding that file, or `not found`. `exit` stops it.

#![no_std]
#![no_main]

use sprout_rt as rt;

fn reply(text: &[u8], handles: &[rt::Handle]) {
    if let Err(e) = rt::channel_send(rt::HANDLE_BOOTSTRAP, text, handles) {
        rt::log!("fsd: reply failed: {}", e.name());
    }
}

/// Copies `data` into a new VMO and returns its handle.
fn vmo_of(data: &[u8]) -> Result<rt::Handle, rt::Error> {
    let vmo = rt::vmo_create(data.len().max(1))?;
    rt::vmo_write(vmo, 0, data)?;
    Ok(vmo)
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    let mut bytes = [0u8; 256];
    let mut handles = [0 as rt::Handle; 1];

    // The archive arrives first.
    let (_, nh) = match rt::channel_recv(
        rt::HANDLE_BOOTSTRAP,
        &mut bytes,
        &mut handles,
        rt::TIMEOUT_INFINITE,
    ) {
        Ok(got) => got,
        Err(e) => {
            rt::log!("fsd: no initramfs: {}", e.name());
            return 2;
        }
    };
    if nh != 1 {
        rt::log("fsd: first message had no VMO");
        return 2;
    }
    let archive_handle = handles[0];
    let size = match rt::vmo_size(archive_handle) {
        Ok(s) => s,
        Err(e) => {
            rt::log!("fsd: vmo_size failed: {}", e.name());
            return 2;
        }
    };
    let addr = match rt::vmo_map(archive_handle) {
        Ok(a) => a,
        Err(e) => {
            rt::log!("fsd: vmo_map failed: {}", e.name());
            return 2;
        }
    };
    // SAFETY: the kernel mapped `size` readable bytes at `addr`, and the
    // mapping lives as long as this process.
    let archive: &[u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    let count = rt::cpio::entries(archive).count();
    rt::log!("fsd: serving {count} file(s) from a {size}-byte initramfs");
    reply(b"ready", &[]);

    loop {
        let (n, nh) = match rt::channel_recv(
            rt::HANDLE_BOOTSTRAP,
            &mut bytes,
            &mut handles,
            rt::TIMEOUT_INFINITE,
        ) {
            Ok(got) => got,
            Err(rt::Error::PeerClosed) => return 0,
            Err(e) => {
                rt::log!("fsd: recv failed: {}", e.name());
                return 2;
            }
        };
        for &h in &handles[..nh] {
            let _ = rt::handle_close(h);
        }
        let line = core::str::from_utf8(&bytes[..n]).unwrap_or("");
        match line {
            "exit" => return 0,
            _ if line.starts_with("open ") => match rt::cpio::find(archive, &line[5..]) {
                Some(data) => match vmo_of(data) {
                    Ok(vmo) => {
                        reply(b"ok", &[vmo]);
                    }
                    Err(e) => reply(e.name().as_bytes(), &[]),
                },
                None => reply(b"not found", &[]),
            },
            _ => reply(b"unknown command", &[]),
        }
    }
}
