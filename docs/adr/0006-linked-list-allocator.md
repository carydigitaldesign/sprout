# ADR-0006: first kernel dependency, `linked_list_allocator`

Status: accepted (2026-11-02)
Date: 2026-11-02

## Context

M2 needs a kernel heap so `alloc` types work. ADR-0001 allows
`linked_list_allocator` on the kernel's dependency list but asks that each
addition be written down.

## Decision

Use `linked_list_allocator` with default features off, wrapped in Sprout's
own `SpinLock`, as the `#[global_allocator]` (`kernel/src/mm/heap.rs`).

## Reasons

- A free-list allocator that is correct for every alignment, with splitting
  and coalescing, is about 500 lines once the corner cases are handled, and
  the corner cases are exactly where hand-written allocators go wrong.
- The crate is small, `no_std`, has no dependencies with default features
  off, and is readable in one sitting if anyone needs to.
- The lock stays ours, so the interrupt-safety work in M3 touches one place.

## Alternatives

- **Write it.** The "under 200 lines, write it" rule does not hold here.
- **Bump allocator.** Forty lines, but never frees; kernel objects come and
  go from M4 on.
- **Slab allocator.** The right eventual answer for fixed-size kernel
  objects; it can sit on top of this one when object churn justifies it.

## Consequences

- Kernel dependencies: `linked_list_allocator`. Still no `spin`, no
  `bitflags`.
- Heap size is fixed at boot (1 MiB). Growing it means handing more frames
  to the allocator, which the crate supports.
