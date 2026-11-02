//! Dropping into user mode.

use super::gdt;

const RFLAGS_IF: usize = 1 << 9;
const RFLAGS_RESERVED: usize = 1 << 1;

/// Starts user code at `entry` with `user_sp`, on the current thread.
/// Traps from user mode land on `kernel_stack_top`, which must be this
/// thread's kernel stack. Never returns: the thread lives in user mode
/// from here, coming back only through the IDT.
pub fn enter_user(entry: usize, user_sp: usize, kernel_stack_top: usize) -> ! {
    gdt::set_rsp0(kernel_stack_top);
    // SAFETY: `iretq` consumes the five words pushed here: user ss and rsp,
    // flags with interrupts on, user cs and the entry point. The segment
    // selectors are the GDT's user entries with RPL 3.
    unsafe {
        core::arch::asm!(
            "push {ss}",
            "push {rsp}",
            "push {rflags}",
            "push {cs}",
            "push {rip}",
            "xor eax, eax",
            "xor ebx, ebx",
            "xor ecx, ecx",
            "xor edx, edx",
            "xor esi, esi",
            "xor edi, edi",
            "xor ebp, ebp",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xor r12d, r12d",
            "xor r13d, r13d",
            "xor r14d, r14d",
            "xor r15d, r15d",
            "iretq",
            ss = in(reg) gdt::USER_DATA as usize,
            rsp = in(reg) user_sp,
            rflags = in(reg) RFLAGS_IF | RFLAGS_RESERVED,
            cs = in(reg) gdt::USER_CODE as usize,
            rip = in(reg) entry,
            options(noreturn),
        )
    }
}
