# CLAUDE.md — Rust-Tcp-Server

## What this project is
A benchmark teardown of TCP server I/O models, from accept-loop to io_uring,
built behind one `Server` trait. Proof-of-work artifact. Correctness and
measurement rigor matter more than features.

## Authoritative specs
- docs/specs/kickoff-brief.md  — strategy, full model list, §4 common bar
- docs/specs/phase0-spec.md    — core foundation (FROZEN)
- docs/specs/phase1-spec.md    — sys, reactor, first 9 models, loadgen, bench
- docs/specs/phase2-spec.md    — CURRENT: multireactor, io-uring, sweep, writeups

## Hard rules
1. `core` is FROZEN. OS I/O lives in server/src/sys/. io_uring uses the raw
   `io-uring` crate only — never tokio-uring/tokio.
2. No hot-path logging. No async runtime. io_uring must be PURPOSE-BUILT.
3. Phase 2 deps: server -> `io-uring`. Nothing else.
4. WRITING STANDARD (BENCHMARKS/README/ARCHITECTURE/x-thread): authoritative,
   declarative, every number cites its committed source file with units and
   conditions. No marketing words (blazing/incredible/seamless/etc.), no emoji,
   no exclamation. Honesty about what underperformed is required. Build writeups
   ONLY from committed numbers — invent nothing.

## Scope discipline
Work ONLY on the given session. End with cargo build+clippy+test, list changes, STOP.

## Commit discipline
- Commit after each file is complete and the crate compiles.
- Use clear messages: "phase0(core): implement RequestParser".
- Never amend or force-push. Never touch the `legacy-snapshot` branch.

## Verification before you say "done"
`cargo build` clean, `cargo clippy` clean (no warnings), `cargo test` green.
If any of these fail, the session is not done.
