//! `sys` — the OS I/O layer: thin, honest wrappers over `libc`.
//!
//! `sys` does **not** hide the semantic differences between mechanisms — that
//! is the whole point of keeping `poll`, `epoll-lt`, and `epoll-et` as separate
//! models. It only removes copy-pasted FFI and fd bookkeeping. Every syscall in
//! the project lives here (and never in `core`). See `phase1-spec.md` §3.
//!
//! `dead_code` is allowed module-wide: these primitives are built in Session 1
//! but first consumed by the models and the reactor in later Phase 1 sessions.
#![allow(dead_code)]

pub mod affinity;
pub mod conn_table;
pub mod epoll;
pub mod poll;
pub mod signal;
pub mod socket;
pub mod syscall;
