/*
 * Sprout kernel layout for riscv64 on QEMU `virt`.
 *
 * OpenSBI owns 0x80000000..0x80200000 and jumps to us at 0x80200000 in
 * supervisor mode with a0 = hart id and a1 = device tree address.
 */
OUTPUT_ARCH(riscv)
ENTRY(_start)

BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;
    __kernel_start = .;

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    . = ALIGN(4K);
    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    /* Registry of kernel tests; see kernel/src/testing.rs. */
    . = ALIGN(8);
    .ktests : {
        __ktests_start = .;
        KEEP(*(.ktests))
        __ktests_end = .;
    }

    . = ALIGN(4K);
    __data_start = .;
    .data : {
        *(.data .data.*)
        . = ALIGN(8);
        PROVIDE(__global_pointer$ = . + 0x800);
        *(.sdata .sdata.*)
    }

    . = ALIGN(16);
    __bss_start = .;
    .bss : {
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        . = ALIGN(16);
    }
    __bss_end = .;

    /* Boot stack for the first hart: 64 KiB, grows down from __stack_top. */
    . = ALIGN(16);
    __stack_bottom = .;
    . += 64K;
    __stack_top = .;

    . = ALIGN(4K);
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
    }
}
