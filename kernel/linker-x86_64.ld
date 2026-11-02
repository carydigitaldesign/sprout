/*
 * Sprout kernel layout for x86_64, booted by a Multiboot loader (QEMU's
 * -kernel, or GRUB on real machines).
 *
 * The image is linked in the top 2 GiB (the "kernel" code model) at
 * IMAGE_OFFSET + its physical address, loaded at 1 MiB. The AT() clauses
 * keep load addresses physical, and the physical symbols at the bottom
 * feed the Multiboot header, which tells the loader to copy the file as a
 * flat image (so this layout must keep file and memory contiguous: every
 * section starts on a page both in memory and in the file).
 */
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start_phys)

KERNEL_PA = 0x100000;
IMAGE_OFFSET = 0xffffffff80000000;
KERNEL_VA = KERNEL_PA + IMAGE_OFFSET;

SECTIONS
{
    . = KERNEL_VA;
    __kernel_start = .;

    .text : AT(ADDR(.text) - IMAGE_OFFSET) {
        __text_start = .;
        KEEP(*(.multiboot))
        KEEP(*(.text.boot))
        *(.text .text.*)
        . = ALIGN(4K);
        __text_end = .;
    }

    .rodata : AT(ADDR(.rodata) - IMAGE_OFFSET) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        /* Resolved at link time (static relocation model): read-only. */
        *(.got .got.plt)

        /* Registry of kernel tests; see kernel/src/testing.rs. */
        . = ALIGN(8);
        __ktests_start = .;
        KEEP(*(.ktests))
        __ktests_end = .;

        . = ALIGN(4K);
        __rodata_end = .;
    }

    .data : AT(ADDR(.data) - IMAGE_OFFSET) {
        __data_start = .;
        *(.data .data.*)
        __data_end = .;
    }

    .bss (NOLOAD) : AT(ADDR(.bss) - IMAGE_OFFSET) {
        . = ALIGN(16);
        __bss_start = .;
        *(.bss .bss.*)
        . = ALIGN(4K);
        *(.bss.boot_tables)
        . = ALIGN(16);
        __bss_end = .;

        /* Boot stack for the first CPU: 64 KiB, grows down from __stack_top. */
        . = ALIGN(16);
        __stack_bottom = .;
        . += 64K;
        __stack_top = .;
    }

    . = ALIGN(4K);
    __kernel_end = .;

    /* Physical addresses for the 32-bit boot code and the Multiboot header. */
    __load_start_pa = ADDR(.text) - IMAGE_OFFSET;
    __load_end_pa = __data_end - IMAGE_OFFSET;
    __bss_end_pa = __kernel_end - IMAGE_OFFSET;
    _start_phys = _start - IMAGE_OFFSET;
    __stack_top_pa = __stack_top - IMAGE_OFFSET;
    __boot_gdt_ptr_pa = boot_gdt_ptr - IMAGE_OFFSET;
    __boot_gdt_pa = boot_gdt - IMAGE_OFFSET;
    __boot_pml4_pa = boot_pml4 - IMAGE_OFFSET;
    __boot_pdpt_low_pa = boot_pdpt_low - IMAGE_OFFSET;
    __boot_pdpt_high_pa = boot_pdpt_high - IMAGE_OFFSET;
    __boot_pd_pa = boot_pd - IMAGE_OFFSET;
    __long_mode_pa = long_mode - IMAGE_OFFSET;

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
        *(.note.GNU-stack)
    }
}
