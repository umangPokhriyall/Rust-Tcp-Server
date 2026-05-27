# Architecture — Rust-Tcp-Server

> **Status:** stub. This document is filled in during Phase 0 Session F (see
> `docs/specs/phase0-spec.md` §12). The sections below are placeholders that
> name what each will cover.

## The sans-IO principle

`core` contains zero socket I/O — it never calls `read`, `write`, `accept`, or
`epoll`. It is a pure library of an incremental HTTP parser, a response encoder,
a router, an asset cache, and a per-connection state machine, all operating only
on byte buffers in memory. Each concurrency model owns every syscall.

_To be expanded in Session F: why this is the only way one `core` can serve all
11 models unchanged, from a blocking accept loop to an io_uring completion ring._

## The `Connection` contract

`core::Connection` is the per-connection state machine every model drives. It is
sans-IO: the model performs all reads/writes and feeds/drains bytes through
`on_bytes` / `pending_write` / `on_written`, acting on the returned `ConnAction`
(`WantRead` / `WantWrite` / `Close`).

_To be expanded in Session F: the in-connection error-response rule, HEAD
handling, keep-alive deadline refresh, and the blocking vs. event-loop usage
skeletons (phase0-spec §8.1)._

## Crate layout

_To be expanded in Session F: the `core` / `server` split and the module map
from phase0-spec §2._
