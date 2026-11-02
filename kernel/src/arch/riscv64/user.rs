//! Dropping into user mode.

use super::csr;

const SSTATUS_SPP: usize = 1 << 8;
const SSTATUS_SPIE: usize = 1 << 5;
const SSTATUS_SUM: usize = 1 << 18;

/// Starts user code at `entry` with `user_sp`, on the current thread.
/// Traps from user mode land on `kernel_stack_top`, which must be this
/// thread's kernel stack. Never returns: the thread lives in user mode
/// from here, coming back only through the trap entry.
pub fn enter_user(entry: usize, user_sp: usize, kernel_stack_top: usize) -> ! {
    let mut sstatus = csr::sstatus::read();
    sstatus &= !SSTATUS_SPP; // return to user mode
    sstatus |= SSTATUS_SPIE; // with interrupts on
    sstatus |= SSTATUS_SUM; // and let syscalls read user memory
    // SAFETY: `sret` below consumes these: the previous-privilege bits say
    // user mode and `sepc` says where. The kernel stack top goes into
    // `sscratch` for the trap entry to find.
    unsafe {
        csr::sstatus::write(sstatus);
        csr::sepc::write(entry);
        core::arch::asm!(
            "csrw sscratch, {kernel_sp}",
            "mv sp, {user_sp}",
            "sret",
            kernel_sp = in(reg) kernel_stack_top,
            user_sp = in(reg) user_sp,
            options(noreturn),
        )
    }
}
