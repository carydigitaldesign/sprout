/*
 * Sprout user program layout. Static, non-PIE, linked at USER_IMAGE_BASE.
 * Each section starts on its own page so the kernel's ELF loader can give
 * text, rodata and data different permissions without sharing pages.
 */
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
    . = 0x10000;

    .text : {
        KEEP(*(.text.start))
        *(.text .text.*)
    }

    . = ALIGN(4K);
    .rodata : {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    . = ALIGN(4K);
    .data : {
        *(.data .data.*)
        . = ALIGN(8);
        PROVIDE(__global_pointer$ = . + 0x800);
        *(.sdata .sdata.*)
    }

    .bss : {
        *(.sbss .sbss.*)
        *(.bss .bss.*)
    }

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
    }
}
