/*
 * Sprout kernel layout for riscv64 on QEMU `virt`.
 *
 * OpenSBI owns 0x80000000..0x80200000 and jumps to the lowest load address
 * of our ELF, 0x80200000, in supervisor mode with paging off. The kernel
 * is linked in the higher half: every section's virtual address (VMA) is
 * its physical address plus PHYS_MAP_BASE, and the AT() clauses keep the
 * load addresses (LMA) physical so QEMU places the image where the boot
 * code expects it. See kernel/src/arch/riscv64/boot.rs.
 */
OUTPUT_ARCH(riscv)
ENTRY(_start)

KERNEL_PA = 0x80200000;
PHYS_MAP_BASE = 0xffffffc000000000;
KERNEL_VA = KERNEL_PA + PHYS_MAP_BASE;

SECTIONS
{
    . = KERNEL_VA;
    __kernel_start = .;

    .text : AT(ADDR(.text) - PHYS_MAP_BASE) {
        __text_start = .;
        KEEP(*(.text.boot))
        *(.text .text.*)
        . = ALIGN(4K);
        __text_end = .;
    }

    .rodata : AT(ADDR(.rodata) - PHYS_MAP_BASE) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)

        /* Registry of kernel tests; see kernel/src/testing.rs. */
        . = ALIGN(8);
        __ktests_start = .;
        KEEP(*(.ktests))
        __ktests_end = .;

        . = ALIGN(4K);
        __rodata_end = .;
    }

    .data : AT(ADDR(.data) - PHYS_MAP_BASE) {
        __data_start = .;
        *(.data .data.*)
        . = ALIGN(8);
        PROVIDE(__global_pointer$ = . + 0x800);
        *(.sdata .sdata.*)
    }

    .bss (NOLOAD) : AT(ADDR(.bss) - PHYS_MAP_BASE) {
        . = ALIGN(16);
        __bss_start = .;
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        . = ALIGN(16);
        __bss_end = .;

        /* Boot stack for the first hart: 64 KiB, grows down from __stack_top. */
        . = ALIGN(16);
        __stack_bottom = .;
        . += 64K;
        __stack_top = .;
    }

    . = ALIGN(4K);
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
    }
}
