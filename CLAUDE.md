# CLAUDE.md — Rust-Tcp-Server

## What this project is
A benchmark teardown of TCP server I/O models, from accept-loop to io_uring,
built behind one `Server` trait. Proof-of-work artifact. Correctness and
measurement rigor matter more than features.

## Authoritative specs
- docs/specs/kickoff-brief.md  — strategy, full model list, Definition of Done
- docs/specs/phase0-spec.md    — the current phase's exact API and scope
These files are the source of truth. If a request conflicts with them, STOP
and ask — do not guess.

## Hard rules (never violate)
1. `core` is SANS-IO: no read/write/accept/epoll anywhere in the `core` crate.
2. No logging on the hot path (no per-request `println!`). Gate it behind a flag.
3. Dependency allowlist for Phase 0: `core` may use only `socket2`; `server`
   may use only `core` + std. Add nothing else without being told.
4. No async runtime (no tokio). The reactors are hand-rolled in later phases.
5. One abstraction, many implementations — no copy-pasted logic between models.

## Scope discipline
- Work ONLY on the session you were given. Do not implement future sessions
  or future phases. Do not implement any model other than the one named.
- Where the spec defers something to a later session, leave `todo!()`.
- End every session by: running `cargo build` + `cargo test` + `cargo clippy`,
  listing what you changed, and STOPPING. Do not continue to the next session.

## Commit discipline
- Commit after each file is complete and the crate compiles.
- Use clear messages: "phase0(core): implement RequestParser".
- Never amend or force-push. Never touch the `legacy-snapshot` branch.

## Verification before you say "done"
`cargo build` clean, `cargo clippy` clean (no warnings), `cargo test` green.
If any of these fail, the session is not done.
