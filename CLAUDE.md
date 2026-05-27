# CLAUDE.md — Rust-Tcp-Server

## What this project is
A benchmark teardown of TCP server I/O models, from accept-loop to io_uring,
built behind one `Server` trait. Proof-of-work artifact. Correctness and
measurement rigor matter more than features.

## Authoritative specs
- docs/specs/kickoff-brief.md  — strategy, full model list, the §4 common bar
- docs/specs/phase0-spec.md    — the core foundation (reference; core is FROZEN)
- docs/specs/phase1-spec.md    — the CURRENT phase: sys, reactor, 8 models, loadgen, bench

## Hard rules (never violate)
1. `core` is FROZEN in Phase 1 — add nothing, change nothing in the core crate.
2. OS I/O syscalls live in `server/src/sys/`, never in `core`.
3. No logging on the hot path. No async runtime (no tokio).
4. Phase 1 dependency allowlist: core -> socket2 only; server -> core, socket2,
   libc; loadgen -> hdrhistogram + std. Add nothing else.
5. One abstraction, many implementations — no copy-pasted logic between models.

## Scope discipline
- Work ONLY on the session you were given. Do not implement future sessions or
  Phase 2 (multireactor, io-uring, the writeup). Leave `todo!()` where the spec
  defers to a later session.
- End every session by running cargo build + clippy + test, listing changes,
  and STOPPING.

## Commit discipline
- Commit after each file is complete and the crate compiles.
- Use clear messages: "phase0(core): implement RequestParser".
- Never amend or force-push. Never touch the `legacy-snapshot` branch.

## Verification before you say "done"
`cargo build` clean, `cargo clippy` clean (no warnings), `cargo test` green.
If any of these fail, the session is not done.
