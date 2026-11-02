//! The interrupt descriptor table and every trap's entry: exceptions
//! 0..32, legacy IRQs at 32..48, the syscall gate at 0x80.
//!
//! Each vector has a tiny stub that pushes a fake error code if the CPU
//! did not, then the vector number, then jumps to a common path that
//! saves all general registers into a [`TrapFrame`] and calls
//! [`trap_handler`]. In long mode the CPU always pushes `ss` and `rsp`,
//! so the frame has one shape whether the trap came from user or kernel
//! mode, and the TSS's `rsp0` provides the kernel stack for user traps.

use core::cell::UnsafeCell;

use super::{gdt, pic, serial};
use crate::log;
use crate::sync::SpinLock;

/// Saved context of an interrupted instruction stream, in push order
/// reversed (lowest address first).
#[repr(C)]
pub struct TrapFrame {
    pub r15: usize,
    pub r14: usize,
    pub r13: usize,
    pub r12: usize,
    pub r11: usize,
    pub r10: usize,
    pub r9: usize,
    pub r8: usize,
    pub rbp: usize,
    pub rdi: usize,
    pub rsi: usize,
    pub rdx: usize,
    pub rcx: usize,
    pub rbx: usize,
    pub rax: usize,
    pub vector: usize,
    pub error: usize,
    pub rip: usize,
    pub cs: usize,
    pub rflags: usize,
    pub rsp: usize,
    pub ss: usize,
}

core::arch::global_asm!(
    r#"
    .altmacro
    .macro VECTOR n
    .globl vector_\n
vector_\n:
    .if (\n == 8) || (\n == 10) || (\n == 11) || (\n == 12) || (\n == 13) || (\n == 14) || (\n == 17) || (\n == 21) || (\n == 29) || (\n == 30)
    .else
    push 0
    .endif
    push \n
    jmp trap_common
    .endm

    .macro TABLE_ENTRY n
    .quad vector_\n
    .endm

    .section .text
    .set i, 0
    .rept 256
    VECTOR %i
    .set i, i + 1
    .endr

trap_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    call trap_handler
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16
    iretq

    .section .rodata
    .align 8
    .globl vector_table
vector_table:
    .set i, 0
    .rept 256
    TABLE_ENTRY %i
    .set i, i + 1
    .endr
"#
);

unsafe extern "C" {
    static vector_table: [usize; 256];
}

const VECTOR_IRQ_BASE: usize = 32;
const VECTOR_SYSCALL: usize = 0x80;
const IRQ_TIMER: u32 = 0;
const IRQ_COM1: u32 = 4;
const EXC_PAGE_FAULT: usize = 14;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}

impl Gate {
    const fn empty() -> Gate {
        Gate {
            offset_low: 0,
            selector: 0,
            ist: 0,
            attributes: 0,
            offset_mid: 0,
            offset_high: 0,
            _reserved: 0,
        }
    }

    fn new(handler: usize, user_callable: bool) -> Gate {
        Gate {
            offset_low: handler as u16,
            selector: gdt::KERNEL_CODE,
            ist: 0,
            // Present, interrupt gate (interrupts off on entry), DPL 0 or 3.
            attributes: if user_callable { 0xEE } else { 0x8E },
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            _reserved: 0,
        }
    }
}

struct Idt(UnsafeCell<[Gate; 256]>);

// SAFETY: written once at boot with interrupts off, then only read by the CPU.
unsafe impl Sync for Idt {}

static IDT: Idt = Idt(UnsafeCell::new([Gate::empty(); 256]));

#[repr(C, packed)]
struct DescriptorPointer {
    limit: u16,
    base: u64,
}

/// Handlers for legacy IRQ lines other than the timer and COM1.
static IRQ_HANDLERS: SpinLock<[Option<fn(u32)>; 16]> = SpinLock::new([None; 16]);

/// Fills and loads the IDT.
pub fn init() {
    // SAFETY: boot CPU, interrupts off; `vector_table` is the assembly
    // table of 256 stub addresses.
    unsafe {
        let idt = &mut *IDT.0.get();
        for (vector, gate) in idt.iter_mut().enumerate() {
            *gate = Gate::new(vector_table[vector], vector == VECTOR_SYSCALL);
        }
        let pointer = DescriptorPointer {
            limit: (size_of::<[Gate; 256]>() - 1) as u16,
            base: IDT.0.get() as u64,
        };
        core::arch::asm!("lidt [{}]", in(reg) &pointer, options(nostack));
    }
}

/// Routes a legacy IRQ line to `handler`.
pub fn register_irq(irq: u32, handler: fn(u32)) {
    assert!(irq < 16, "IRQ {irq} out of range");
    IRQ_HANDLERS.lock()[irq as usize] = Some(handler);
}

fn exception_name(vector: usize) -> &'static str {
    match vector {
        0 => "divide error",
        1 => "debug",
        2 => "non-maskable interrupt",
        3 => "breakpoint",
        4 => "overflow",
        5 => "bound range exceeded",
        6 => "invalid opcode",
        7 => "device not available",
        8 => "double fault",
        10 => "invalid TSS",
        11 => "segment not present",
        12 => "stack segment fault",
        13 => "general protection fault",
        14 => "page fault",
        16 => "x87 floating point",
        17 => "alignment check",
        18 => "machine check",
        19 => "SIMD floating point",
        _ => "unknown exception",
    }
}

fn read_cr2() -> usize {
    let value: usize;
    // SAFETY: reading CR2 has no side effects.
    unsafe { core::arch::asm!("mov {}, cr2", out(reg) value, options(nomem, nostack)) };
    value
}

/// Called by `trap_common` with the saved context. Changes to `frame`
/// take effect when the assembly restores it.
#[unsafe(no_mangle)]
extern "C" fn trap_handler(frame: &mut TrapFrame) {
    let vector = frame.vector;
    let from_user = frame.cs & 3 == 3;

    if (VECTOR_IRQ_BASE..VECTOR_IRQ_BASE + 16).contains(&vector) {
        let irq = (vector - VECTOR_IRQ_BASE) as u32;
        pic::end_of_interrupt(irq as u8);
        match irq {
            IRQ_TIMER => {
                crate::time::on_tick();
                if crate::sched::on_tick(crate::time::ticks()) {
                    crate::sched::yield_now();
                }
            }
            IRQ_COM1 => serial::on_interrupt(),
            _ => {
                let handler = IRQ_HANDLERS.lock()[irq as usize];
                match handler {
                    Some(handler) => handler(irq),
                    None => log::warn("trap", "interrupt from an IRQ with no handler")
                        .num("irq", irq)
                        .end(),
                }
            }
        }
        return;
    }

    if vector == VECTOR_SYSCALL && from_user {
        // Run the syscall with interrupts on: it may sleep or block like
        // any kernel code.
        let args = [
            frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
        ];
        super::interrupts::enable();
        let result = crate::syscall::dispatch(frame.rax, args);
        super::interrupts::disable();
        frame.rax = result as usize;
        return;
    }

    let address = if vector == EXC_PAGE_FAULT {
        read_cr2()
    } else {
        0
    };
    if from_user {
        super::interrupts::enable();
        crate::syscall::user_fault(exception_name(vector), vector, frame.rip, address);
    }

    let cause = exception_name(vector);
    log::event("trap")
        .str("kind", "exception")
        .str("cause", cause)
        .num("code", vector)
        .hex("error", frame.error)
        .hex("rip", frame.rip)
        .hex("address", address)
        .hex("sp", frame.rsp)
        .hex_list(
            "frames",
            core::iter::once(frame.rip).chain(super::backtrace::frames_from(frame.rbp).take(32)),
        )
        .end();
    panic!("unhandled trap: {cause} at {:#x}", frame.rip);
}

crate::ktest! {
    fn the_loaded_idt_is_ours() {
        let mut stored = [0u8; 10];
        // SAFETY: `sidt` stores the 10-byte descriptor into our buffer.
        unsafe { core::arch::asm!("sidt [{}]", in(reg) stored.as_mut_ptr(), options(nostack)) };
        let base = u64::from_le_bytes(stored[2..10].try_into().unwrap());
        assert_eq!(base, IDT.0.get() as u64);
    }
}
