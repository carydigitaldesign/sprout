//! Processes: an address space, a handle table and (for now) one thread,
//! created by [`spawn`] from an ELF image. There is no fork.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use sprout_abi::{HANDLE_SELF_PROCESS, Rights, USER_IMAGE_BASE, USER_STACK_SIZE, USER_STACK_TOP};

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

pub struct Process {
    pub pid: u64,
    pub name: String,
    pub table: SpinLock<PageTable>,
    pub handles: SpinLock<HandleTable>,
    /// VMOs mapped into the address space; they outlive their mappings.
    mapped: SpinLock<Vec<Arc<SpinLock<Vmo>>>>,
    exit_code: SpinLock<Option<i32>>,
    exited: WaitQueue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    Elf(ElfError),
    SegmentOutsideUserRange,
    Map(MapError),
    OutOfMemory,
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
        }
    }
}

/// What a new user thread needs to start running user code.
struct UserStart {
    entry: usize,
    stack_pointer: usize,
}

/// Maps every page of `vmo` at `va` with `flags`.
fn map_vmo(table: &mut PageTable, vmo: &Vmo, va: usize, flags: MapFlags) -> Result<(), MapError> {
    for i in 0..vmo.size() / PAGE_SIZE {
        let frame = vmo.frame(i).expect("vmo page exists");
        table.map(va + i * PAGE_SIZE, frame.pa(), PageSize::Size4K, flags)?;
    }
    Ok(())
}

/// Creates a process from a static ELF image and starts its first thread.
pub fn spawn(name: &str, image: &[u8]) -> Result<Arc<Process>, SpawnError> {
    let elf = Elf::parse(image).map_err(SpawnError::Elf)?;
    let mut table = PageTable::new_user(mm::kernel_root_pa()).map_err(SpawnError::Map)?;
    let mut mapped = Vec::new();

    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    for segment in elf.segments() {
        let segment = segment.map_err(SpawnError::Elf)?;
        let end = segment
            .vaddr
            .checked_add(segment.memsz)
            .ok_or(SpawnError::SegmentOutsideUserRange)?;
        if segment.vaddr < USER_IMAGE_BASE || end > stack_bottom {
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
        map_vmo(&mut table, &vmo, page_start, flags).map_err(SpawnError::Map)?;
        mapped.push(Arc::new(SpinLock::new(vmo)));
    }

    let stack = Vmo::new(USER_STACK_SIZE).map_err(|_| SpawnError::OutOfMemory)?;
    map_vmo(
        &mut table,
        &stack,
        stack_bottom,
        MapFlags::USER | MapFlags::READ | MapFlags::WRITE,
    )
    .map_err(SpawnError::Map)?;
    mapped.push(Arc::new(SpinLock::new(stack)));

    let process = Arc::new(Process {
        pid: NEXT_PID.fetch_add(1, Ordering::Relaxed),
        name: String::from(name),
        table: SpinLock::new(table),
        handles: SpinLock::new(HandleTable::new()),
        mapped: SpinLock::new(mapped),
        exit_code: SpinLock::new(None),
        exited: WaitQueue::new(),
    });

    // The process's first capability is itself. Cleared at exit so the
    // reference cycle does not keep it alive.
    let self_handle = process.handles.lock().insert(
        Object::Process(process.clone()),
        Rights::DESCRIBE | Rights::DUPLICATE | Rights::TRANSFER,
    );
    debug_assert_eq!(self_handle, HANDLE_SELF_PROCESS);

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

    /// Bytes of VMO memory mapped into the process.
    pub fn mapped_bytes(&self) -> usize {
        self.mapped.lock().iter().map(|v| v.lock().size()).sum()
    }
}

#[cfg(feature = "ktest")]
mod tests {
    use super::*;
    use crate::mm::frame;
    use crate::user_image::INIT_ELF;

    crate::ktest! {
        fn init_runs_to_completion_and_is_freed() {
            let frames_before = frame::stats().free;
            let threads_before = sched::live_threads();
            {
                let init = spawn("init", INIT_ELF).expect("spawn init");
                assert_eq!(init.wait(), 0, "init's checks all passed");
                assert_eq!(init.handles.lock().len(), 0, "handles dropped at exit");
            }
            // The thread is reaped by the next switch after it exits.
            sched::yield_now();
            assert_eq!(sched::live_threads(), threads_before);
            assert_eq!(frame::stats().free, frames_before, "process memory leaked");
        }
    }

    crate::ktest! {
        fn a_faulting_process_is_killed_not_the_kernel() {
            // Same image, entry point patched to an unmapped address.
            let mut image = INIT_ELF.to_vec();
            image[24..32].copy_from_slice(&0u64.to_le_bytes());
            let broken = spawn("broken", &image).expect("spawn");
            let code = broken.wait();
            assert_eq!(code, 128 + 12, "instruction page fault, reported as 128 + cause");
            sched::yield_now();
        }
    }
}
