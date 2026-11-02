//! Processes: an address space, a handle table and (for now) one thread,
//! created by [`spawn`] from an ELF image. There is no fork.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use sprout_abi::{
    Error, HANDLE_SELF_PROCESS, Rights, USER_IMAGE_BASE, USER_MMAP_BASE, USER_STACK_SIZE,
    USER_STACK_TOP, cpio,
};

use super::Object;
use super::handle::HandleTable;
use crate::arch::{self, PAGE_SIZE, PageTable};
use crate::elf::{Elf, ElfError};
use crate::mm::vmo::Vmo;
use crate::mm::{self, MapError, MapFlags, PageSize};
use crate::sched::wait::WaitQueue;
use crate::sync::SpinLock;
use crate::{log, sched};

static NEXT_PID: AtomicU64 = AtomicU64::new(1);

/// Something mapped into the address space: a VMO, or a device window
/// (no VMO; the pages belong to the hardware).
struct Mapping {
    va: usize,
    pages: usize,
    /// Held so the pages outlive the mapping; never read directly.
    #[allow(dead_code)]
    vmo: Option<Arc<SpinLock<Vmo>>>,
}

pub struct Process {
    pub pid: u64,
    pub name: String,
    pub table: SpinLock<PageTable>,
    pub handles: SpinLock<HandleTable>,
    mappings: SpinLock<Vec<Mapping>>,
    /// Where the next `vmo_map` or `device_map` goes.
    next_mmap: SpinLock<usize>,
    exit_code: SpinLock<Option<i32>>,
    exited: WaitQueue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    Elf(ElfError),
    SegmentOutsideUserRange,
    Map(MapError),
    OutOfMemory,
    NotInInitramfs,
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpawnError::Elf(e) => write!(f, "ELF: {e}"),
            SpawnError::SegmentOutsideUserRange => {
                f.write_str("a segment lies outside the user range")
            }
            SpawnError::Map(e) => write!(f, "mapping: {e:?}"),
            SpawnError::OutOfMemory => f.write_str("out of memory"),
            SpawnError::NotInInitramfs => f.write_str("program not in the initramfs"),
        }
    }
}

/// What a new user thread needs to start running user code.
struct UserStart {
    entry: usize,
    stack_pointer: usize,
}

/// Maps every page of `vmo` at `va` with `flags`.
fn map_pages(table: &mut PageTable, vmo: &Vmo, va: usize, flags: MapFlags) -> Result<(), MapError> {
    for i in 0..vmo.size() / PAGE_SIZE {
        let frame = vmo.frame(i).expect("vmo page exists");
        table.map(va + i * PAGE_SIZE, frame.pa(), PageSize::Size4K, flags)?;
    }
    Ok(())
}

/// Creates a process from a static ELF image and starts its first thread.
/// `extra` handles are installed after the process's own, from
/// `HANDLE_BOOTSTRAP` upwards, before the thread can run.
pub fn spawn(
    name: &str,
    image: &[u8],
    extra: Vec<(Object, Rights)>,
) -> Result<Arc<Process>, SpawnError> {
    let elf = Elf::parse(image).map_err(SpawnError::Elf)?;
    let mut table = PageTable::new_user(mm::kernel_root_pa()).map_err(SpawnError::Map)?;
    let mut mappings = Vec::new();

    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    for segment in elf.segments() {
        let segment = segment.map_err(SpawnError::Elf)?;
        let end = segment
            .vaddr
            .checked_add(segment.memsz)
            .ok_or(SpawnError::SegmentOutsideUserRange)?;
        if segment.vaddr < USER_IMAGE_BASE || end > USER_MMAP_BASE {
            return Err(SpawnError::SegmentOutsideUserRange);
        }
        let page_start = segment.vaddr & !(PAGE_SIZE - 1);
        let page_end = end.next_multiple_of(PAGE_SIZE);
        let mut vmo = Vmo::new(page_end - page_start).map_err(|_| SpawnError::OutOfMemory)?;
        vmo.write(segment.vaddr - page_start, elf.segment_data(&segment))
            .map_err(|_| SpawnError::OutOfMemory)?;

        let mut flags = MapFlags::USER;
        if segment.read {
            flags = flags | MapFlags::READ;
        }
        if segment.write {
            flags = flags | MapFlags::WRITE;
        }
        if segment.exec {
            flags = flags | MapFlags::EXEC;
        }
        map_pages(&mut table, &vmo, page_start, flags).map_err(SpawnError::Map)?;
        mappings.push(Mapping {
            va: page_start,
            pages: vmo.size() / PAGE_SIZE,
            vmo: Some(Arc::new(SpinLock::new(vmo))),
        });
    }

    let stack = Vmo::new(USER_STACK_SIZE).map_err(|_| SpawnError::OutOfMemory)?;
    map_pages(
        &mut table,
        &stack,
        stack_bottom,
        MapFlags::USER | MapFlags::READ | MapFlags::WRITE,
    )
    .map_err(SpawnError::Map)?;
    mappings.push(Mapping {
        va: stack_bottom,
        pages: USER_STACK_SIZE / PAGE_SIZE,
        vmo: Some(Arc::new(SpinLock::new(stack))),
    });

    let process = Arc::new(Process {
        pid: NEXT_PID.fetch_add(1, Ordering::Relaxed),
        name: String::from(name),
        table: SpinLock::new(table),
        handles: SpinLock::new(HandleTable::new()),
        mappings: SpinLock::new(mappings),
        next_mmap: SpinLock::new(USER_MMAP_BASE),
        exit_code: SpinLock::new(None),
        exited: WaitQueue::new(),
    });

    // The process's first capability is itself. Cleared at exit so the
    // reference cycle does not keep it alive. Then whatever the spawner
    // passed along, in order.
    {
        let mut handles = process.handles.lock();
        let self_handle = handles.insert(
            Object::Process(process.clone()),
            Rights::DESCRIBE | Rights::DUPLICATE | Rights::TRANSFER,
        );
        debug_assert_eq!(self_handle, HANDLE_SELF_PROCESS);
        for (object, rights) in extra {
            handles.insert(object, rights);
        }
    }

    let start = Box::new(UserStart {
        entry: elf.entry(),
        stack_pointer: USER_STACK_TOP,
    });
    let root_pa = process.table.lock().root_pa();
    sched::spawn_user(
        name,
        process.clone(),
        root_pa,
        user_thread_entry,
        Box::into_raw(start) as usize,
    );

    log::event("spawn")
        .num("pid", process.pid)
        .str("name", name)
        .hex("entry", elf.entry())
        .end();
    Ok(process)
}

/// Starts `init` from the initramfs. Its bootstrap handle is the whole
/// archive as a VMO; after it come every discovered device.
pub fn spawn_init() -> Result<Arc<Process>, SpawnError> {
    let archive = crate::user_image::INITRAMFS;
    let image = cpio::find(archive, "init").ok_or(SpawnError::NotInInitramfs)?;
    let initramfs = Vmo::from_bytes(archive).map_err(|_| SpawnError::OutOfMemory)?;
    let vmo_rights =
        Rights::READ | Rights::MAP | Rights::DUPLICATE | Rights::TRANSFER | Rights::DESCRIBE;
    let mut extra = alloc::vec![(Object::Vmo(Arc::new(SpinLock::new(initramfs))), vmo_rights)];
    for device in super::device::all() {
        extra.push((Object::Device(device), Rights::ALL));
    }
    spawn("init", image, extra)
}

/// First code of a user thread, on its kernel stack: drop into user mode.
fn user_thread_entry(arg: usize) {
    // SAFETY: `arg` is the `Box<UserStart>` leaked by `spawn` for exactly
    // this thread, and this is its only use.
    let start = unsafe { Box::from_raw(arg as *mut UserStart) };
    let kernel_stack_top = sched::current_stack_top();
    arch::enter_user(start.entry, start.stack_pointer, kernel_stack_top)
}

impl Process {
    /// Records the exit code, drops every handle, and wakes waiters. The
    /// calling thread still has to leave the scheduler itself.
    pub fn exit(&self, code: i32) {
        *self.exit_code.lock() = Some(code);
        self.handles.lock().clear();
        self.exited.wake_all();
        log::event("process_exit")
            .num("pid", self.pid)
            .str("name", &self.name)
            .num("code", code)
            .end();
    }

    pub fn exit_code(&self) -> Option<i32> {
        *self.exit_code.lock()
    }

    /// Blocks until the process exits, then returns its code.
    pub fn wait(&self) -> i32 {
        self.exited.wait_while(|| self.exit_code.lock().is_none());
        self.exit_code().expect("woken after exit")
    }

    /// Bytes mapped into the process, devices included.
    pub fn mapped_bytes(&self) -> usize {
        self.mappings
            .lock()
            .iter()
            .map(|m| m.pages * PAGE_SIZE)
            .sum()
    }

    /// Reserves `pages` of address space above `USER_MMAP_BASE`, with a
    /// guard page after, and returns its start.
    fn reserve(&self, pages: usize) -> Result<usize, Error> {
        if pages == 0 {
            return Err(Error::InvalidArgument);
        }
        let mut next = self.next_mmap.lock();
        let va = *next;
        if va + pages * PAGE_SIZE > USER_STACK_TOP - USER_STACK_SIZE {
            return Err(Error::OutOfMemory);
        }
        *next = va + (pages + 1) * PAGE_SIZE;
        Ok(va)
    }

    /// Maps the whole VMO at the next free address and returns it.
    pub fn map_vmo(&self, vmo: Arc<SpinLock<Vmo>>, flags: MapFlags) -> Result<usize, Error> {
        let pages = vmo.lock().size() / PAGE_SIZE;
        let va = self.reserve(pages)?;
        {
            let mut table = self.table.lock();
            let guard = vmo.lock();
            map_pages(&mut table, &guard, va, flags | MapFlags::USER).map_err(|e| match e {
                MapError::OutOfMemory => Error::OutOfMemory,
                _ => Error::InvalidArgument,
            })?;
        }
        self.mappings.lock().push(Mapping {
            va,
            pages,
            vmo: Some(vmo),
        });
        Ok(va)
    }

    /// Maps a physical range (a device's registers) and returns its address.
    pub fn map_physical(&self, pa: usize, size: usize, flags: MapFlags) -> Result<usize, Error> {
        if pa % PAGE_SIZE != 0 {
            return Err(Error::InvalidArgument);
        }
        let pages = size.div_ceil(PAGE_SIZE);
        let va = self.reserve(pages)?;
        {
            let mut table = self.table.lock();
            for i in 0..pages {
                table
                    .map(
                        va + i * PAGE_SIZE,
                        pa + i * PAGE_SIZE,
                        PageSize::Size4K,
                        flags | MapFlags::USER,
                    )
                    .map_err(|e| match e {
                        MapError::OutOfMemory => Error::OutOfMemory,
                        _ => Error::InvalidArgument,
                    })?;
            }
        }
        self.mappings.lock().push(Mapping {
            va,
            pages,
            vmo: None,
        });
        Ok(va)
    }

    /// Removes a mapping made by [`map_vmo`](Self::map_vmo) or
    /// [`map_physical`](Self::map_physical).
    pub fn unmap(&self, va: usize) -> Result<(), Error> {
        let mapping = {
            let mut mappings = self.mappings.lock();
            let i = mappings
                .iter()
                .position(|m| m.va == va && m.va >= USER_MMAP_BASE)
                .ok_or(Error::InvalidArgument)?;
            mappings.swap_remove(i)
        };
        let mut table = self.table.lock();
        for i in 0..mapping.pages {
            let _ = table.unmap(va + i * PAGE_SIZE);
        }
        Ok(())
    }
}

#[cfg(feature = "ktest")]
mod tests {
    use super::*;
    use crate::mm::frame;

    crate::ktest! {
        fn init_runs_to_completion_and_is_freed() {
            let frames_before = frame::stats().free;
            let threads_before = sched::live_threads();
            {
                let init = spawn_init().expect("spawn init");
                assert_eq!(init.wait(), 0, "init's checks all passed");
                assert_eq!(init.handles.lock().len(), 0, "handles dropped at exit");
            }
            // Threads are reaped by the next switch after they exit.
            sched::yield_now();
            sched::yield_now();
            assert_eq!(sched::live_threads(), threads_before);
            assert_eq!(frame::stats().free, frames_before, "process memory leaked");
        }
    }

    crate::ktest! {
        fn a_faulting_process_is_killed_not_the_kernel() {
            // init's image, entry point patched to an unmapped address.
            let mut image = cpio::find(crate::user_image::INITRAMFS, "init")
                .expect("init in initramfs")
                .to_vec();
            image[24..32].copy_from_slice(&0u64.to_le_bytes());
            let broken = spawn("broken", &image, Vec::new()).expect("spawn");
            let code = broken.wait();
            assert_eq!(code, 128 + 12, "instruction page fault, reported as 128 + cause");
            sched::yield_now();
        }
    }
}
