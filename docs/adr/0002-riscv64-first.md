# ADR-0002: First target is riscv64 on QEMU virt; x86_64 is port #2

Status: accepted by the navigator on 2026-11-02 ("Yes, start M0")
Amended by ADR-0007 (2026-11-02): the x86_64 port boots through Multiboot 1,
not Limine.
Date: 2026-11-02

## Context

"Lite" is a stated goal. The first architecture sets how much boot and
interrupt plumbing exists before a single line of real kernel logic runs.

## Decision

Start on **riscv64** using the QEMU `virt` machine. Boot through OpenSBI, which
QEMU bundles (`-bios default`), and pass our kernel ELF with `-kernel`. Add
`x86_64` with the Limine boot protocol as the second architecture at M8.

## Reasons

| | riscv64 (QEMU virt) | x86_64 (Limine) |
|---|---|---|
| Boot tooling | None. QEMU loads the ELF. | Limine binaries, OVMF firmware, a FAT image or ISO. Doable on Windows without xorriso via QEMU's `fat:` drive, but it is a day of fiddling. |
| Early code | SBI calls for console and timer (about 30 lines), trap vector, PLIC. | GDT, IDT, TSS, APIC or x2APIC, HPET or TSC calibration, syscall MSRs. |
| Paging | Sv39: three levels, one page of spec. | Four or five levels plus many legacy bits. |
| Device discovery | Device tree blob handed over by OpenSBI. | ACPI tables. |
| Reference code | `xv6-riscv` (about 6k lines of C, MIT), `rCore`. | Phil Opp's blog, countless hobby kernels. |
| Real hardware | Scarce (VisionFive 2, Milk-V). | Any laptop, Hyper-V, VirtualBox, cloud VMs. |
| AI training data | Less, but the architecture is small enough that less is needed. | Most. |

The estimated saving is roughly 1,500 lines of architecture-specific code and
a week of boot debugging before M1.

## Alternatives

- **x86_64 first.** The right choice if booting on your own laptop early is a
  motivator, or if you expect to run in Hyper-V or a cloud VM soon. Everything
  else in `PLAN.md` holds; only `arch/` and the boot section of M0 and M1 change.
- **aarch64 first.** Middle ground (GIC, generic timer; runs on Raspberry Pi and
  Apple Silicon VMs). Not chosen because riscv64 is simpler and x86_64 is more
  useful, so aarch64 wins on neither axis.

## Consequences

- `kernel/src/arch/` is the only place architecture-specific code may live.
  Everything above it (`mm`, `sched`, `obj`, `ipc`, `syscall`) is written
  against a small trait surface so the x86_64 port is additive.
- Day-to-day development runs under QEMU regardless of architecture, which is
  true of nearly every hobby kernel anyway.
