//! `init`: the first thing that runs in user mode.
//!
//! Its bootstrap handle is the initramfs. It starts `fsd` and hands it the
//! archive, then gets every other program from `fsd`: `shell`, which it
//! drives over a channel; `blkd`, which it gives the virtio-blk device;
//! and `mcpd`, which it gives the virtio-net device and a shell of its own.
//! In the test kernel it shuts `mcpd` down and exits with the verdict (0
//! means every check passed); in a normal boot it leaves `mcpd` serving.

#![no_std]
#![no_main]

use sprout_rt as rt;

/// Logs a description, which is JSON the kernel produced.
fn show(what: &str, handle: rt::Handle) -> bool {
    let mut buf = [0u8; 8192];
    match rt::describe(handle, &mut buf) {
        Ok(needed) if needed <= buf.len() => {
            let text = core::str::from_utf8(&buf[..needed]).unwrap_or("<not utf-8>");
            rt::log!("{what}: {text}");
            true
        }
        Ok(needed) => {
            rt::log!(
                "{what}: description needs {needed} bytes, buffer has {}",
                buf.len()
            );
            false
        }
        Err(e) => {
            rt::log!("{what}: describe failed: {}", e.name());
            false
        }
    }
}

fn check(what: &str, ok: bool) -> bool {
    rt::log!("check {what}: {ok}");
    ok
}

/// The start of a handle's description, for cheap type checks.
fn describe_prefix(handle: rt::Handle, buf: &mut [u8; 256]) -> Option<usize> {
    let needed = rt::describe(handle, buf).ok()?;
    Some(needed.min(buf.len()))
}

/// Whether `handle` currently refers to a channel endpoint. Handle numbers
/// are reused once closed, so "is it still a channel" is the question, not
/// "is the number still valid".
fn is_channel(handle: rt::Handle) -> bool {
    let mut buf = [0u8; 256];
    describe_prefix(handle, &mut buf).is_some() && buf.starts_with(br#"{"object":"channel""#)
}

/// Whether this is the test kernel, which wants services shut down at the
/// end so the run can finish.
fn test_mode() -> bool {
    let mut buf = [0u8; 256];
    describe_prefix(rt::HANDLE_KERNEL, &mut buf).is_some_and(|n| {
        core::str::from_utf8(&buf[..n]).is_ok_and(|s| s.contains(r#""mode":"test""#))
    })
}

/// Sends one command and returns the reply as a string.
fn ask<'a>(chan: rt::Handle, cmd: &[u8], buf: &'a mut [u8; 4096]) -> Result<&'a str, rt::Error> {
    rt::channel_send(chan, cmd, &[])?;
    let mut handles = [0 as rt::Handle; 1];
    let (n, _) = rt::channel_recv(chan, buf, &mut handles, 2000)?;
    Ok(core::str::from_utf8(&buf[..n]).unwrap_or(""))
}

/// A file from `fsd`, as a VMO handle.
fn open(fs: rt::Handle, name: &str) -> Result<rt::Handle, rt::Error> {
    let mut cmd = rt::StackString::<64>::new();
    let _ = core::fmt::Write::write_fmt(&mut cmd, format_args!("open {name}"));
    rt::channel_send(fs, cmd.as_str().as_bytes(), &[])?;
    let mut buf = [0u8; 64];
    let mut handles = [0 as rt::Handle; 1];
    let (n, nh) = rt::channel_recv(fs, &mut buf, &mut handles, 2000)?;
    if nh == 1 && &buf[..n] == b"ok" {
        Ok(handles[0])
    } else {
        rt::log!(
            "open {name}: {}",
            core::str::from_utf8(&buf[..n]).unwrap_or("?")
        );
        Err(rt::Error::InvalidArgument)
    }
}

/// Spawns `name` from `fsd` with `bootstrap` as its bootstrap handle.
fn spawn(fs: rt::Handle, name: &str, bootstrap: rt::Handle) -> Result<rt::Handle, rt::Error> {
    let image = open(fs, name)?;
    let process = rt::process_spawn(image, bootstrap, name);
    let _ = rt::handle_close(image);
    process
}

/// The handle of the first device whose description names `kind` (a
/// virtio device name such as `block` or `network`). Devices come after
/// the bootstrap handle.
fn find_device(kind: &str) -> Option<rt::Handle> {
    let mut buf = [0u8; 256];
    let mut needle = rt::StackString::<48>::new();
    let _ = core::fmt::Write::write_fmt(&mut needle, format_args!(r#""virtio_device":"{kind}""#));
    (rt::HANDLE_BOOTSTRAP + 1..rt::HANDLE_BOOTSTRAP + 16).find(|&h| {
        describe_prefix(h, &mut buf).is_some_and(|n| {
            buf.starts_with(br#"{"object":"device""#)
                && core::str::from_utf8(&buf[..n]).is_ok_and(|s| s.contains(needle.as_str()))
        })
    })
}

fn syscall_checks() -> bool {
    let mut ok = true;
    ok &= show("kernel", rt::HANDLE_KERNEL);
    ok &= show("self", rt::HANDLE_SELF_PROCESS);

    match rt::vmo_create(8192) {
        Ok(vmo) => {
            let mut back = [0u8; 6];
            let round_trip = rt::vmo_write(vmo, 4090, b"sprout").is_ok()
                && rt::vmo_read(vmo, 4090, &mut back).is_ok()
                && &back == b"sprout";
            ok &= check("vmo round trip across a page boundary", round_trip);
            ok &= check(
                "vmo out-of-range read refused",
                rt::vmo_read(vmo, 8190, &mut back) == Err(rt::Error::OutOfRange),
            );
            ok &= rt::handle_close(vmo).is_ok();
            ok &= check(
                "closed handle refused",
                rt::vmo_read(vmo, 0, &mut back) == Err(rt::Error::InvalidHandle),
            );
        }
        Err(e) => {
            rt::log!("vmo_create failed: {}", e.name());
            ok = false;
        }
    }

    let kernel_ptr = 0xffff_ffc0_8020_0000usize;
    let bad = rt::syscall(rt::abi::sys::LOG, [kernel_ptr, 16, 0, 0, 0, 0]);
    ok &= check(
        "kernel address rejected",
        rt::abi::decode(bad) == Err(rt::Error::BadAddress),
    );
    ok
}

/// Starts `fsd`, hands it the initramfs, and returns our end of its channel.
fn start_fsd() -> Result<rt::Handle, rt::Error> {
    let mut initramfs = [0u8; 256];
    let n =
        describe_prefix(rt::HANDLE_BOOTSTRAP, &mut initramfs).ok_or(rt::Error::InvalidHandle)?;
    rt::log!(
        "initramfs: {}",
        core::str::from_utf8(&initramfs[..n]).unwrap_or("?")
    );

    // fsd's image comes straight out of the archive; after that fsd serves.
    let size = rt::vmo_size(rt::HANDLE_BOOTSTRAP)?;
    let addr = rt::vmo_map(rt::HANDLE_BOOTSTRAP)?;
    // SAFETY: the kernel mapped `size` readable bytes at `addr`.
    let archive: &[u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    let fsd_elf = rt::cpio::find(archive, "fsd").ok_or(rt::Error::InvalidArgument)?;
    let image = rt::vmo_create(fsd_elf.len())?;
    rt::vmo_write(image, 0, fsd_elf)?;
    rt::vmo_unmap(addr)?;

    let (ours, theirs) = rt::channel_create()?;
    rt::process_spawn(image, theirs, "fsd")?;
    rt::handle_close(image)?;
    let for_fsd = rt::handle_duplicate(
        rt::HANDLE_BOOTSTRAP,
        rt::Rights::READ | rt::Rights::MAP | rt::Rights::TRANSFER | rt::Rights::DESCRIBE,
    )?;
    rt::channel_send(ours, b"initramfs", &[for_fsd])?;
    let mut buf = [0u8; 64];
    let mut handles = [0 as rt::Handle; 1];
    let (n, _) = rt::channel_recv(ours, &mut buf, &mut handles, 2000)?;
    if &buf[..n] != b"ready" {
        return Err(rt::Error::InvalidArgument);
    }
    Ok(ours)
}

fn shell_checks(fs: rt::Handle) -> bool {
    let mut ok = true;
    let mut buf = [0u8; 4096];

    let (ours, theirs) = match rt::channel_create() {
        Ok(pair) => pair,
        Err(e) => {
            rt::log!("channel_create failed: {}", e.name());
            return false;
        }
    };
    let shell = match spawn(fs, "shell", theirs) {
        Ok(h) => h,
        Err(e) => {
            rt::log!("spawning shell failed: {}", e.name());
            return false;
        }
    };
    ok &= check("bootstrap handle moved out", !is_channel(theirs));
    ok &= check(
        "ping answers pong",
        ask(ours, b"ping", &mut buf) == Ok("pong"),
    );
    ok &= check(
        "echo",
        ask(ours, b"echo hello over ipc", &mut buf) == Ok("hello over ipc"),
    );

    let port_ok = (|| -> Result<bool, rt::Error> {
        let port = rt::port_create()?;
        rt::port_attach(port, ours, 7)?;
        let idle = rt::port_wait(port, 0);
        rt::channel_send(ours, b"echo via port", &[])?;
        let packet = rt::port_wait(port, 2000)?;
        let mut handles = [0 as rt::Handle; 1];
        let (n, _) = rt::channel_recv(ours, &mut buf, &mut handles, 0)?;
        let after = rt::port_wait(port, 0);
        rt::handle_close(port)?;
        Ok(idle == Err(rt::Error::TimedOut)
            && packet.key == 7
            && packet.signals & rt::signals::READABLE != 0
            && &buf[..n] == b"via port"
            && after == Err(rt::Error::TimedOut))
    })();
    ok &= check("port waits and wakes", port_ok == Ok(true));

    let shared_ok = (|| -> Result<bool, rt::Error> {
        let vmo = rt::vmo_create(4096)?;
        rt::vmo_write(vmo, 0, b"shared memory works\0")?;
        let addr = rt::vmo_map(vmo)?;
        // SAFETY: the kernel mapped one readable, writable page at `addr`.
        let page = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, 4096) };
        let seen_here = &page[..19] == b"shared memory works";
        page[7..13].copy_from_slice(b"MEMORY");
        let mut back = [0u8; 19];
        rt::vmo_read(vmo, 0, &mut back)?;
        let written_through = &back == b"shared MEMORY works";
        rt::vmo_unmap(addr)?;
        let for_shell = rt::handle_duplicate(
            vmo,
            rt::Rights::READ | rt::Rights::MAP | rt::Rights::TRANSFER,
        )?;
        rt::channel_send(ours, b"read-vmo", &[for_shell])?;
        let mut handles = [0 as rt::Handle; 1];
        let (n, _) = rt::channel_recv(ours, &mut buf, &mut handles, 2000)?;
        let seen_there = &buf[..n] == b"shared MEMORY works";
        rt::handle_close(vmo)?;
        Ok(seen_here && written_through && seen_there)
    })();
    ok &= check("shared memory across processes", shared_ok == Ok(true));

    ok &= check(
        "shell exits 0",
        rt::channel_send(ours, b"exit", &[]).is_ok() && rt::process_wait(shell) == Ok(0),
    );
    let mut handles = [0 as rt::Handle; 1];
    ok &= check(
        "peer closed after exit",
        rt::channel_recv(ours, &mut buf, &mut handles, 100) == Err(rt::Error::PeerClosed),
    );
    ok &= rt::handle_close(ours).is_ok() && rt::handle_close(shell).is_ok();
    ok
}

fn blkd_checks(fs: rt::Handle) -> bool {
    let Some(device) = find_device("block") else {
        // No virtio-mmio block device on this platform (x86_64 has no
        // device discovery yet): nothing to drive, nothing to fail.
        return check("blkd skipped: no block device", true);
    };
    let mut ok = show("block device", device);
    let result = (|| -> Result<bool, rt::Error> {
        let (ours, theirs) = rt::channel_create()?;
        let blkd = spawn(fs, "blkd", theirs)?;
        let for_blkd = rt::handle_duplicate(device, rt::Rights::ALL)?;
        rt::channel_send(ours, b"device", &[for_blkd])?;
        let mut buf = [0u8; 256];
        let mut handles = [0 as rt::Handle; 1];
        let (n, _) = rt::channel_recv(ours, &mut buf, &mut handles, 5000)?;
        let reply = core::str::from_utf8(&buf[..n]).unwrap_or("?");
        rt::log!("blkd says: {reply}");
        let code = rt::process_wait(blkd)?;
        rt::handle_close(ours)?;
        rt::handle_close(blkd)?;
        Ok(reply.starts_with("ok capacity=") && code == 0)
    })();
    ok &= check(
        "blkd drives the disk through interrupts",
        result == Ok(true),
    );
    ok
}

/// Starts `mcpd` with the network device and a shell of its own. In test
/// mode it is told to exit once ready; otherwise this never returns.
fn mcpd_checks(fs: rt::Handle, test: bool) -> bool {
    let Some(device) = find_device("network") else {
        return check("mcpd skipped: no network device", true);
    };
    let mut ok = show("network device", device);
    let result = (|| -> Result<bool, rt::Error> {
        let (shell_ours, shell_theirs) = rt::channel_create()?;
        let shell = spawn(fs, "shell", shell_theirs)?;
        let (ours, theirs) = rt::channel_create()?;
        let mcpd = spawn(fs, "mcpd", theirs)?;
        let for_mcpd = rt::handle_duplicate(device, rt::Rights::ALL)?;
        rt::channel_send(ours, b"net", &[for_mcpd])?;
        rt::channel_send(ours, b"shell", &[shell_ours])?;
        let mut buf = [0u8; 256];
        let mut handles = [0 as rt::Handle; 1];
        let (n, _) = rt::channel_recv(ours, &mut buf, &mut handles, 10_000)?;
        let reply = core::str::from_utf8(&buf[..n]).unwrap_or("?");
        rt::log!("mcpd says: {reply}");
        if reply != "ready" {
            return Ok(false);
        }
        if test {
            rt::channel_send(ours, b"exit", &[])?;
            let mcpd_code = rt::process_wait(mcpd)?;
            // mcpd's exit closes its end of the shell channel; the shell leaves.
            let shell_code = rt::process_wait(shell)?;
            rt::handle_close(ours)?;
            rt::handle_close(mcpd)?;
            rt::handle_close(shell)?;
            Ok(mcpd_code == 0 && shell_code == 0)
        } else {
            rt::log("init: mcpd is serving; staying up as its parent");
            let _ = rt::process_wait(mcpd);
            Ok(true)
        }
    })();
    ok &= check("mcpd comes up on the network", result == Ok(true));
    ok
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    rt::log("hello from userspace");
    let test = test_mode();
    let mut ok = syscall_checks();
    let fs = match start_fsd() {
        Ok(fs) => fs,
        Err(e) => {
            rt::log!("starting fsd failed: {}", e.name());
            return 1;
        }
    };
    ok &= check("fsd is serving", true);
    ok &= shell_checks(fs);
    ok &= blkd_checks(fs);
    ok &= mcpd_checks(fs, test);
    ok &= check("fsd exits 0", rt::channel_send(fs, b"exit", &[]).is_ok());
    rt::yield_now();
    rt::sleep_ms(5);
    rt::log!("init done, ok={ok}");
    if ok { 0 } else { 1 }
}
