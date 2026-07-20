# Architecture

Most TCP server implementations intertwine protocol handling with their chosen
I/O model: blocking sockets call directly into request handlers, event loops
embed protocol parsing inside readiness callbacks, and `io_uring`
implementations often duplicate the entire connection pipeline around completion
events. That coupling makes it difficult to compare concurrency models fairly,
because changing the I/O mechanism also changes the application logic.

This repository was designed to isolate exactly one variable: how bytes move
between the kernel and the application. Every other part of the server—the HTTP
parser, routing, response generation, keep-alive handling, timeout logic, and
connection state machine—remains identical.

To achieve that, the project is organized around a frozen sans-IO
`core::Connection` state machine consumed unchanged by eleven independent TCP
server implementations, ranging from a simple blocking accept loop to a
purpose-built `io_uring` completion engine.

This document explains the architecture, the contracts between layers, and the
design decisions that allow one protocol implementation to drive every
concurrency model without modification.

## 1. Design goals

The architecture was designed around four goals.

1. Separate protocol logic completely from operating-system I/O.

2. Allow every concurrency model to reuse exactly the same HTTP
   implementation.

3. Keep operating-system mechanisms explicit rather than hiding them behind
   abstractions so each benchmark measures the intended kernel interface.

4. Make adding a new concurrency model require implementing only the transport
   layer rather than duplicating protocol code.

## 2. High-level architecture

Four layers, dependencies pointing downward only. The sans-IO boundary is the
line above which no code touches a socket.

```
                       ┌──────────────────────────────────────────────┐
   models/             │  iterative  forking  preforked  thread-per-   │
   (one strategy each, │  conn  thread-pool  poll  epoll-lt  epoll-et   │
    each impl Server)  │  event-loop  multireactor  io-uring            │
                       └───────────────┬───────────────┬───────────────┘
                                       │               │
                          (event-loop, │               │ (blocking models,
                           multireactor)│               │  io-uring use sys
                                       ▼               │  directly)
   reactor.rs           ┌──────────────────────────┐   │
   (epoll-ET assembly)  │  Reactor: epoll loop +    │   │
                        │  ConnTable, drives core   │   │
                        └───────────────┬───────────┘   │
                                        │               │
                                        ▼               ▼
   sys/                 ┌──────────────────────────────────────────────┐
   (raw OS I/O)         │  socket  poll  epoll  affinity  signal         │
                        │  conn_table  syscall      — every syscall here │
                        └───────────────┬──────────────────────────────┘
                                        │
   ─────────────────────────────────── │ ──── SANS-IO BOUNDARY ─────────
   nothing below touches a socket       ▼
   core/                ┌──────────────────────────────────────────────┐
   (sans-IO protocol)   │  http::{request,response}  router  asset  app  │
   FROZEN               │  Connection (state machine) + ConnAction       │
                        │  Server trait, ServerConfig, bind_listener     │
                        └──────────────────────────────────────────────┘
```

`core` depends on nothing in the project. `sys` depends on `libc` and `core`'s
types but performs no protocol logic. `reactor` assembles `sys` primitives and
drives a `core::Connection`. Each `models/` module selects one concurrency
strategy and reuses everything beneath it. No layer reaches upward.

## 3. Layer responsibilities

### `core/` — the sans-IO protocol library

- **Owns:** the HTTP request parser (`http::request`), response encoder
  (`http::response`), trie router (`router`), in-memory asset cache (`asset`),
  the per-connection `Connection` state machine and its `ConnAction` contract
  (`conn`), the `Server` trait and `ServerConfig` (`server`), bounded-input
  ceilings (`limits`), and atomic metrics (`metrics`).
- **Must never:** call `read`, `write`, `accept`, `epoll`, or any other
  per-connection syscall. It operates only on byte buffers in memory and the
  monotonic clock (for deadlines). It is **frozen** — its public contract and behavior
  do not change.
- **Public surface:** re-exported from `core::lib`. The model-facing pieces are
  `Connection`, `ConnAction`, `App`, `Server`, `ServerConfig`, and the
  setup-only helper `bind_listener`.
- **The one sanctioned exception** is `bind_listener` (`server.rs`): a _setup_
  helper that creates and binds a listening socket via `socket2`, including the
  `SO_REUSEPORT` path for `preforked` and `multireactor`. Binding and `listen()`
  are connection-_setup_, not per-connection I/O; no `read`, `write`, `accept`,
  or `epoll` lives in `core`.

### `server/src/sys/` — raw OS I/O

- **Owns:** thin `libc` wrappers — `socket` (non-blocking sockets,
  `SO_REUSEPORT`), `poll`, `epoll` (level- and edge-triggered), `affinity` (CPU
  pinning), `signal` (SIGINT/SIGTERM shutdown), `conn_table` (fd→connection
  slab), and `syscall` (retry/`EINTR` helpers). Every syscall in the project
  lives here.
- **Must never:** contain protocol or HTTP logic, and must not hide the semantic
  differences between mechanisms — `poll`, `epoll-lt`, and `epoll-et` stay
  distinct so the models can measure their difference. `sys` removes copy-pasted
  FFI and fd bookkeeping, nothing more.
- **Public surface:** one module per primitive (`affinity`, `conn_table`,
  `epoll`, `poll`, `signal`, `socket`, `syscall`).

### `server/src/reactor.rs` — event-loop assembly

- **Owns:** the epoll-ET readiness loop: an `epoll` instance plus a `ConnTable`,
  arming/disarming interest from each `ConnAction`, draining each socket to
  `EAGAIN`, managing `EPOLLOUT` for partial writes, and driving the
  `core::Connection` for every fd. `Reactor::new` builds it; `Reactor::run`
  runs it until the shared shutdown flag is set.
- **Must never:** select a model or own a concurrency policy. It is one reusable
  reactor; the `event-loop` model runs one of it on one thread, and each
  `multireactor` worker runs its own pinned instance over its own
  `SO_REUSEPORT` listener.
- **Public surface:** `Reactor`, `Reactor::new(...)`, `Reactor::run(&shutdown,
&app)`.

### `server/src/models/` — the eleven strategies

- **Owns:** one concurrency/I-O strategy per module, each implementing
  `core::Server`. The blocking models share one serve loop (`blocking.rs`); the
  event-loop models share the `reactor`. A model owns _only_ how readiness or
  completion is obtained and how work is dispatched.
- **Must never:** re-implement HTTP handling or copy-paste the serve loop. The
  only thing that varies between `iterative` and `io-uring` is the I/O strategy,
  not the protocol — that is what makes the benchmark a controlled comparison.
- **Public surface:** each model is a struct implementing `Server`; `main.rs`
  dispatches on the `--model` name.

## 4. The sans-IO contract

A blocking accept-loop model and an `io_uring` completion model have nothing in
common at the I/O layer: one waits on a blocking `read`, the other reaps
completions from a ring. The only way one `core` can serve all eleven models
without modification is if `core` never touches a socket — so the protocol logic
consumes and produces byte buffers and reports intent (`WantRead` / `WantWrite`
/ `Close`), and the model performs the actual syscalls.

The concrete payoff is that blocking `read`/`write`, epoll readiness, and
`io_uring` completion all reuse one `Connection` state machine. The legacy repo
coupled parsing to `TcpStream` (`route_client(stream)`), which is precisely why
it could not extend past a blocking loop. `core` reintroduces no such coupling,
and the benchmark confirms the payoff three ways: the same machine drove blocking
I/O, epoll readiness, and `io_uring` completion, unmodified
(`docs/BENCHMARKS.md` §7).

## 5. Evidence that one protocol implementation served all eleven models

The central architectural claim of this project is that changing the I/O model
never required changing protocol code.

Throughout development the `core` crate remained frozen while eleven server
implementations were added above it. Every model below consumes the same
`core::Connection`; only the mechanism that delivers bytes differs.

| Model           | I/O mechanism                                              | Consumes unmodified `core::Connection`? |
| --------------- | ---------------------------------------------------------- | --------------------------------------- |
| iterative       | Blocking serve loop, one connection at a time              | Y                                       |
| forking         | Blocking serve loop in a per-connection `fork()` child     | Y                                       |
| preforked       | Blocking serve loop in a fixed `SO_REUSEPORT` worker pool  | Y                                       |
| thread-per-conn | Blocking serve loop on a per-connection OS thread          | Y                                       |
| thread-pool     | Blocking serve loop on a bounded worker pool               | Y                                       |
| poll            | `poll(2)` readiness, non-blocking sockets                  | Y                                       |
| epoll-lt        | Level-triggered `epoll` readiness                          | Y                                       |
| epoll-et        | Edge-triggered `epoll` readiness, drain to `EAGAIN`        | Y                                       |
| event-loop      | epoll-ET via the reusable `reactor`                        | Y                                       |
| multireactor    | Pinned per-core `reactor`, `SO_REUSEPORT`, shared-nothing  | Y                                       |
| io-uring        | Single-ring completion: multishot accept, provided buffers | Y                                       |

All eleven rows are Y. The blocking models drive `Connection` through the shared
serve loop, the readiness models through their epoll/poll loops, the event-loop
and `multireactor` models through the reactor, and `io-uring` feeds completions
into the identical `on_bytes` / `on_written` contract.

## 5. Connection lifecycle

`core::Connection` is the per-connection state machine every model drives. It is
sans-IO: the model performs all reads and writes; the connection consumes and
produces byte buffers and reports the next action via `ConnAction`.

```
model: accept() ─▶ Connection::new(read_timeout)             [Reading]
        │
        ├─ read bytes ───▶ conn.on_bytes(&buf, &app) ──▶ ConnAction
        │                     RequestParser.push(bytes)
        │                       Incomplete ─▶ WantRead         [Reading]
        │                       Complete   ─▶ App::handle ─▶ encode ─▶ WantWrite   [Writing]
        │                       Failed     ─▶ error Response ─▶ encode ─▶ WantWrite then Close
        │
        ├─ write bytes ──▶ conn.on_written(n) ──▶ ConnAction
        │                     drained + keep-alive ─▶ WantRead (deadline refreshed) [KeepAlive→Reading]
        │                     drained + close      ─▶ Close                          [Close]
        │
        └─ timer tick ───▶ conn.is_expired(now) ─▶ Close if true
```

| `ConnAction` | Meaning for the model                                              |
| ------------ | ------------------------------------------------------------------ |
| `WantRead`   | Wait for readability, then call `on_bytes(&buf, &app)`.            |
| `WantWrite`  | Write `pending_write()`, then report progress via `on_written(n)`. |
| `Close`      | Close the fd and drop the `Connection`.                            |

The contracts the state machine guarantees:

- **Sans-IO.** No syscalls; it reads only the monotonic clock for deadlines.
- **In-connection error responses.** On a parse `Failed`, `on_bytes` builds the
  error response (status from `ParseError::status()`), encodes it with
  `keep_alive = false`, and the post-write action is `Close`. The model never
  sees the error — it gets `WantWrite` then `Close`. This is the one-place fix
  for the legacy "one bad request kills the server" bug: an error is a normal
  response followed by a close, not a propagated `?`.
- **HEAD handling.** A `HEAD` response is encoded with `include_body = false` —
  correct headers and `Content-Length`, no body bytes.
- **Keep-alive + deadline refresh.** When `on_written` fully drains the response
  and the request wanted keep-alive, the connection resets its parser, returns
  to `Reading`, and refreshes the read deadline. Bytes that arrived past the
  current request are retained by `RequestParser::reset` so a pipelined request
  is not lost.
- **Timeouts.** `is_expired(now)` lets event-loop models drop a stalled
  connection on a timer tick; blocking models additionally rely on socket
  read/write timeouts. A slow client never pins a worker forever.

## 6. Major design decisions

Each decision states the alternative it rejected and the tradeoff accepted.

- **No async runtime / no tokio.** _Rejected:_ building the models on `tokio` or
  `async-std`. _Reason:_ the project measures I/O _mechanisms_ — blocking,
  `poll`, level- and edge-triggered `epoll`, `io_uring` completion — and an async
  runtime would hide exactly the mechanism under study behind its own scheduler
  and reactor. `io_uring` uses the raw `io-uring` crate, never `tokio-uring`, for
  the same reason. _Tradeoff accepted:_ more hand-written event-loop and lifetime
  code, in exchange for a controlled, mechanism-level comparison.

- **Shared-nothing `SO_REUSEPORT` over single-acceptor + fd handoff.** The
  kickoff brief sketched "one acceptor + N reactors." _Rejected:_ a shared
  acceptor thread that accepts and hands fds to reactors over a queue. _Reason:_
  `SO_REUSEPORT` lets the kernel 4-tuple-hash connections directly to a
  per-reactor listener, removing the acceptor, the fd-handoff queue, and the only
  shared hot-path state. _Tradeoff accepted:_ the kernel's hash balancing has no
  work-stealing, so skewed connection lifetimes can imbalance reactors
  (`docs/BENCHMARKS.md` §7, the `multireactor` caveat) — accepted because the
  benchmark confirms zero shared-state contention and the best C10K median of any
  model, p50 = 70 µs (`bench/results/c10k_multireactor.csv`), at 1.002
  ctx-switches/req on disjoint cores (`bench/results/profiles/summary.csv`).

- **`Vec` header store over `HashMap`.** _Rejected:_ a `HashMap` keyed by header
  name. _Reason:_ requests carry a handful of headers; a small linear-scanned
  `Vec` avoids per-request hashing and allocation and is faster at that size.
  _Tradeoff accepted:_ O(n) header lookup, which is cheaper than hashing for the
  realistic header count and within the bounded `limits.rs` ceiling.

- **Provided buffer rings over per-read allocation (io-uring).** _Rejected:_
  posting a freshly allocated buffer with each read SQE. _Reason:_ a provided
  buffer ring lets the kernel select the read buffer and report it in the CQE,
  which — with multishot accept — removes the per-accept and per-read submission
  syscalls and is what drives `io_uring` to 2.021 syscalls/req against
  epoll-et's 4.028 (`bench/results/profiles/summary.csv`). _Tradeoff accepted:_
  ring/buffer bookkeeping and tighter coupling to the kernel ABI, in exchange for
  the syscall-elimination the model exists to demonstrate.

- **Single-ring, single-thread io-uring scope.** _Rejected:_ thread-per-core,
  multi-ring `io_uring` (the production form). _Reason:_ a single ring on a
  single thread isolates syscall-elimination from core count, making the fair
  comparison single-ring `io_uring` vs single-thread `epoll-et`. _Tradeoff
  accepted:_ this `io_uring` uses one core, so absolute-throughput leadership
  belongs to `multireactor` on N cores (`docs/BENCHMARKS.md` §8). On EPYC the
  single ring sustains a true C10K without shedding, but the AMD Zen4 pipeline
  data shows its halved syscall count does not out-execute epoll-ET (0.76 vs 1.20
  retired ops/cyc at C10K, `bench/results/profiles/perf_io-uring_c10k.txt`) — the
  syscall result it isolates holds on the apples-to-apples axis; the pipeline win
  does not follow on this frontend-latency-bound workload. Multi-ring `io_uring`
  is noted as future work, not built.

- **Bounded inputs enforced in the parser.** _Rejected:_ trusting client input
  sizes. _Reason:_ every ceiling in `limits.rs` (request-line, header, body,
  version) is enforced by the parser and mapped to an HTTP status (`414`, `431`,
  `413`, `505`), so an attacker cannot make the parser buffer unbounded input.
  _Tradeoff accepted:_ a fixed refusal point on oversized but legitimate inputs,
  in exchange for slow-loris and memory-DoS resistance.

## Relationship to the benchmark

The benchmark results reported elsewhere in the repository are a direct
consequence of this architecture. Because every model shares the same protocol
implementation, differences in throughput, latency, syscall count, and pipeline
utilization can be attributed to the concurrency mechanism rather than changes
in application logic.

This separation is the primary contribution of the project: eleven TCP server
architectures evaluated under one unchanged HTTP implementation.
