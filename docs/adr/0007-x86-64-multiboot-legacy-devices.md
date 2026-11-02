# ADR-0007: x86_64 boots through Multiboot 1 on legacy devices

Status: accepted (2026-11-02)
Date: 2026-11-02

## Context

ADR-0002 planned the x86_64 port around the Limine boot protocol. When M8
arrived, the cost that mattered was the development loop on Windows
(ADR-0005): Limine needs a bootable image (FAT or ISO) built on every run,
or firmware (OVMF) and a loader binary vendored into the repo. The riscv64
loop is `-kernel sprout-kernel` and nothing else.

The port also had to choose which interrupt controller, timer and syscall
entry to start with.

## Decision

- Boot through **Multiboot 1** with the a.out kludge address fields. QEMU's
  `-kernel` loads such an ELF directly; GRUB does too. The kernel's own
  32-bit stub builds the first page tables and enters long mode.
- Start with the **legacy platform devices**: the 8259 PICs remapped to
  vectors 32 to 47, the 8254 PIT at 100 Hz, COM1 as console, `int 0x80`
  as the syscall gate, the TSC (calibrated against the PIT) as the clock.
- Link the image in the **top 2 GiB** (`-C code-model=kernel`,
  `relocation-model=static`) and keep the direct map at the same
  `PHYS_MAP_BASE` riscv64 uses, so portable code sees one layout.

## Reasons

- The dev loop stays one command per architecture; nothing is generated
  or downloaded. CI needs only `qemu-system-x86`.
- Every x86 machine and hypervisor still emulates the legacy devices;
  each is under 100 lines and has no discovery step. The APIC, HPET and
  `syscall`/`sysret` are optimizations and can land one at a time.
- Multiboot 1 is 32 bytes of header; the loader does the ELF work.

## Alternatives

- **Limine.** Enters long mode with a higher-half map already set up, and
  boots UEFI machines. Costs the image build on every run. Revisit when
  booting real hardware is the goal.
- **Multiboot 2.** Same loop as Multiboot 1 with UEFI support and a
  tag-based info structure; a reasonable next step if GRUB on a laptop is
  wanted before Limine.
- **APIC and `syscall` from the start.** More code before the first test
  passes, for no gain under QEMU.

## Consequences

- No UEFI-only machines until a second boot path exists; no SMP without
  the local APIC.
- The kernel image must stay below 4 GiB physical and keep file and memory
  layout identical (the linker script page-aligns each section).
