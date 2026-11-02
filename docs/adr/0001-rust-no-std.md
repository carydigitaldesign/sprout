# ADR-0001: Kernel language is Rust (no_std, stable)

Status: accepted (M0 built on it, 2026-11-02)
Date: 2026-11-02

## Context

The kernel will be written mostly by an AI pair with a human reviewing. The
language choice matters more than usual: the compiler is the first reviewer of
every line the AI writes.

## Decision

Rust, `#![no_std]`, on the **stable** toolchain. The bare-metal targets we need
(`riscv64gc-unknown-none-elf`, `x86_64-unknown-none`, `aarch64-unknown-none`)
are Tier 2 with prebuilt `core` and `alloc`, so no nightly and no `build-std`.

## Reasons

- **The borrow checker catches the bugs an AI is most likely to write** in a
  kernel: use-after-free, aliasing across interrupt boundaries, data races
  between cores. In C those surface as heisenbugs weeks later.
- **`unsafe` is greppable.** Every place the AI steps outside the type system is
  marked, and our working agreement requires a `// SAFETY:` comment there. A
  reviewer, human or AI, can audit the dangerous surface in minutes.
- **Tooling is structured.** `cargo` errors are machine-readable JSON, which the
  MCP dev server can hand back to the agent without scraping text.
- **Existing paths.** Phil Opp's "Writing an OS in Rust", `rCore`, Redox and
  Theseus mean the models have seen a lot of correct bare-metal Rust.

## Alternatives

- **C with the Zig toolchain.** `zig cc` cross-compiles trivially on Windows and
  C is what every kernel reference is written in. Rejected because it gives up
  the compiler-as-reviewer property, which is the main thing protecting a
  largely AI-written codebase.
- **Zig itself.** Attractive and small, but the language is still moving and the
  bare-metal ecosystem is thinner.

## Consequences

- Kernel crate dependency allowlist starts at `spin`, `bitflags`,
  `linked_list_allocator`. Anything else needs its own ADR.
- `rust-toolchain.toml` pins an exact stable version. Upgrades are a commit.
