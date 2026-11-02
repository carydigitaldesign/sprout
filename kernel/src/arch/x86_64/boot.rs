//! The first instructions: a Multiboot loader (QEMU's `-kernel`, GRUB)
//! jumps here in 32-bit protected mode, paging off, with the Multiboot
//! magic in `eax` and the information structure's physical address in
//! `ebx`.
//!
//! The Multiboot header uses the "a.out kludge" address fields, so the
//! loader copies the file as a flat image at 1 MiB and zeroes `.bss`; the
//! linker script keeps file and memory layout identical for that. The
//! code then builds page tables (identity, direct map and the top-2 GiB
//! image alias, all 2 MiB pages over the first GiB), enters long mode and
//! jumps to the high alias before any Rust runs.

core::arch::global_asm!(
    r#"
    .section .multiboot, "a"
    .align 4
multiboot_header:
    .long 0x1BADB002
    .long 0x00010003
    .long -(0x1BADB002 + 0x00010003)
    .long __load_start_pa
    .long __load_start_pa
    .long __load_end_pa
    .long __bss_end_pa
    .long _start_phys

    .section .bss.boot_tables, "aw", @nobits
    .align 4096
    .globl boot_pml4
boot_pml4:
    .skip 4096
    .globl boot_pdpt_low
boot_pdpt_low:
    .skip 4096
    .globl boot_pdpt_high
boot_pdpt_high:
    .skip 4096
    .globl boot_pd
boot_pd:
    .skip 4096

    .section .text.boot, "ax"
    .code32
    .globl _start
    .type _start, @function
_start:
    cli
    mov esp, offset __stack_top_pa
    // Keep what the loader told us; edi/esi survive into long mode.
    mov edi, eax
    mov esi, ebx

    // boot_pd: 512 entries of 2 MiB, present | writable | huge.
    xor ecx, ecx
    mov edx, 0x83
1:
    mov [__boot_pd_pa + ecx * 8], edx
    mov dword ptr [__boot_pd_pa + ecx * 8 + 4], 0
    add edx, 0x200000
    inc ecx
    cmp ecx, 512
    jne 1b

    // boot_pd is the first GiB; it appears three times. PDPT_LOW[0]: the
    // identity map this code runs under. PDPT_HIGH[256]: the direct map at
    // PHYS_MAP_BASE (0xffffffc000000000). PDPT_HIGH[510]: the image alias
    // at IMAGE_OFFSET (0xffffffff80000000). Both high ones share PML4[511].
    mov eax, offset __boot_pd_pa
    or eax, 0x3
    mov [__boot_pdpt_low_pa], eax
    mov [__boot_pdpt_high_pa + 256 * 8], eax
    mov [__boot_pdpt_high_pa + 510 * 8], eax
    mov eax, offset __boot_pdpt_low_pa
    or eax, 0x3
    mov [__boot_pml4_pa], eax
    mov eax, offset __boot_pdpt_high_pa
    or eax, 0x3
    mov [__boot_pml4_pa + 511 * 8], eax

    // PAE on, root table loaded, long mode and no-execute enabled in EFER,
    // then paging and write protection on.
    mov eax, cr4
    or eax, 1 << 5
    mov cr4, eax
    mov eax, offset __boot_pml4_pa
    mov cr3, eax
    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 8) | (1 << 11)
    wrmsr
    mov eax, cr0
    or eax, (1 << 31) | (1 << 16)
    mov cr0, eax

    lgdt [__boot_gdt_ptr_pa]
    push 0x08
    mov eax, offset __long_mode_pa
    push eax
    retf

    .code64
    .globl long_mode
long_mode:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    xor ax, ax
    mov fs, ax
    mov gs, ax
    // Over to the image alias; everything from here uses linked addresses.
    mov rax, offset high_entry
    jmp rax
high_entry:
    mov rsp, offset __stack_top
    xor ebp, ebp
    // rdi = Multiboot magic, rsi = information structure (physical).
    call x86_entry
2:  hlt
    jmp 2b
    .size _start, . - _start

    .align 8
    .globl boot_gdt
boot_gdt:
    .quad 0
    .quad 0x00209A0000000000
    .quad 0x0000920000000000
boot_gdt_end:
    .globl boot_gdt_ptr
boot_gdt_ptr:
    .word boot_gdt_end - boot_gdt - 1
    .long __boot_gdt_pa
"#
);
