//! Control and status registers, one small module per CSR.
//!
//! Reads are safe. Writes are `unsafe` because most CSRs change what the
//! hardware does with the next instruction; each caller states why the
//! write is sound.

macro_rules! csr {
    ($(#[$doc:meta])* $name:ident, $csr:literal) => {
        $(#[$doc])*
        pub mod $name {
            /// Reads the register.
            #[inline]
            pub fn read() -> usize {
                let value: usize;
                // SAFETY: reading a CSR has no effect beyond producing its value.
                unsafe {
                    core::arch::asm!(
                        concat!("csrr {}, ", $csr),
                        out(reg) value,
                        options(nomem, nostack),
                    )
                };
                value
            }

            /// Writes the register.
            ///
            /// # Safety
            /// The caller must know what the hardware does with the new value.
            #[inline]
            pub unsafe fn write(value: usize) {
                // SAFETY: the caller upholds this function's contract.
                unsafe {
                    core::arch::asm!(
                        concat!("csrw ", $csr, ", {}"),
                        in(reg) value,
                        options(nomem, nostack),
                    )
                };
            }
        }
    };
}

csr!(
    /// Supervisor trap vector base address.
    stvec,
    "stvec"
);
csr!(
    /// Supervisor status: interrupt enable bits, previous privilege, FPU state.
    sstatus,
    "sstatus"
);
csr!(
    /// Supervisor address translation: paging mode and root table.
    satp,
    "satp"
);
csr!(
    /// Supervisor interrupt enable: which sources may interrupt.
    sie,
    "sie"
);
csr!(
    /// Scratch register: the kernel stack top while user code runs, zero
    /// while the kernel runs (see `trap.rs`).
    sscratch,
    "sscratch"
);
csr!(
    /// Where `sret` goes.
    sepc,
    "sepc"
);
csr!(
    /// The platform timer counter (`rdtime`), read-only.
    time,
    "time"
);
