# Architecture — Rust-Tcp-Server

A benchmark teardown of TCP server I/O models, from an `accept()`-in-a-loop to
`io_uring`, built behind **one** `Server` trait. The whole point is that the
HTTP work lives in exactly one place and every concurrency model reuses it
unchanged. This document describes the design that makes that possible: the
**sans-IO** `core` crate and the per-connection **`Connection`** state machine.

## The sans-IO principle

`core` contains **zero socket I/O**. It never calls `read`, `write`, `accept`,
or `epoll`. It is a pure library of:

- an incremental HTTP **request parser** (`http::request`),
- a **response encoder** (`http::response`),
- a trie **router** (`router`),
- an in-memory **asset cache** (`asset`),
- a per-connection **state machine** (`conn`),

all of which operate **only on byte buffers in memory**.

The *models* (`iterative` today; `epoll-et`, `io-uring`, … later) own every
syscall. They obtain readiness however their strategy dictates, perform the
reads and writes, and feed bytes to / drain bytes from `core`.

**Why this is non-negotiable.** A blocking accept-loop model and an `io_uring`
completion-based model have nothing in common at the I/O layer — one waits on a
blocking `read`, the other reaps completions from a ring. The *only* way one
`core` can serve all eleven models without modification is if `core` never
touches a socket. The legacy repo coupled parsing to `TcpStream`
(`route_client(stream)`), which is precisely why it could not extend past a
blocking loop. `core` reintroduces no such coupling anywhere.

> The one sanctioned exception is `core::bind_listener` (`server.rs`), a
> *setup* helper that creates and binds a listening socket via `socket2`
> (including the `SO_REUSEPORT` path for preforked/multireactor models). Binding
> and `listen()` are connection-setup, not per-connection I/O; no `read`,
> `write`, `accept`, or `epoll` lives in `core`.

## Crate layout

```
core/                         # the sans-IO library — the single source of truth
  src/
    lib.rs                    # re-exports the public surface (spec §10)
    limits.rs                 # bounded-input ceilings (DoS defense)
    http/{method,headers,request,response}.rs
    router.rs                 # trie Router + pure Handler type
    asset.rs                  # AssetCache: load once at boot, serve from RAM
    app.rs                    # App (router+assets+metrics), Arc-shared, Send+Sync
    conn.rs                   # Connection state machine + ConnAction
    server.rs                 # Server trait, ServerConfig, bind_listener
    metrics.rs                # atomic counters
server/                       # one binary: `server --model <name> --port <p>`
  src/main.rs                 # CLI, App construction, model dispatch
  src/models/iterative.rs     # the Phase 0 reference model
  assets/                     # benchmark fixtures (index.html, static/style.css)
```

`App` is built once at boot, wrapped in `Arc`, and shared **read-only** by every
worker / thread / forked child; it is `Send + Sync` (enforced by a compile-time
assertion in `app.rs`). A `Handler` is a **pure** function
`fn(&Request, &App) -> Response` — no I/O, no blocking — which replaces the
legacy `fn(TcpStream) -> Result<()>`.

## Bounded inputs

Every ceiling in `limits.rs` is enforced by the parser; exceeding one is a fatal
parse error mapped to an HTTP status (`414`, `431`, `413`, `505`, …). This is
what makes the server slow-loris- and memory-DoS-resistant: an attacker cannot
make the parser buffer unbounded request lines, headers, or bodies.

## The request lifecycle

```
model: accept() ─▶ Connection::new(read_timeout)
        │
        ├─ read bytes ───▶ conn.on_bytes(&buf, &app) ──▶ ConnAction
        │                     │
        │                     ├─ RequestParser.push(bytes)
        │                     │     Incomplete ─▶ WantRead
        │                     │     Complete   ─▶ App::handle(req) ─▶ encode ─▶ WantWrite
        │                     │     Failed     ─▶ error Response   ─▶ encode ─▶ WantWrite (then Close)
        │
        ├─ write bytes ──▶ conn.on_written(n) ──▶ ConnAction
        │                     drained + keep-alive ─▶ WantRead (deadline refreshed)
        │                     drained + close      ─▶ Close
        │
        └─ timer tick ───▶ conn.is_expired(now) ─▶ close if true
```

`App::handle` routes a parsed request: an unsupported method → `405`, no
matching route → `404`, otherwise the matched `Handler` runs. `HEAD` shares
`GET`'s routing table; the body is dropped later, at encode time.

## The `Connection` contract

`core::Connection` is the per-connection state machine that **every** model
drives. It is **sans-IO**: the model performs all reads/writes; the connection
only consumes/produces byte buffers and tells the model what to do next via
`ConnAction`:

| `ConnAction` | Meaning for the model |
|---|---|
| `WantRead`  | Wait for readability, then call `on_bytes(&buf, &app)`. |
| `WantWrite` | Write `pending_write()`, then report progress via `on_written(n)`. |
| `Close`     | Close the fd and drop the `Connection`. |

The contracts the state machine guarantees:

- **Sans-IO.** It performs no syscalls (it reads only the monotonic clock for
  deadlines).
- **In-connection error responses.** On a parse `Failed`, `on_bytes` itself
  builds the error response (status from `ParseError::status()`), encodes it
  with `keep_alive = false`, and the post-write action is `Close`. The model
  never sees the error — it just gets `WantWrite` then `Close`. *This is the
  one-place fix for the legacy "one bad request kills the server" bug:* an error
  is a normal response followed by a close, not a propagated `?`.
- **HEAD handling.** For a `HEAD` request, the response is encoded with
  `include_body = false` — correct headers and `Content-Length`, no body bytes.
- **Keep-alive + deadline refresh.** When `on_written` fully drains the response
  and the request wanted keep-alive, the connection resets its parser, returns
  to reading, and **refreshes the read deadline**. Bytes that arrived past the
  current request are retained by `RequestParser::reset`, so a pipelined request
  is not lost (full pipelining is out of scope for Phase 0, but the retain rule
  holds so it can be added later without redesign).
- **Timeouts.** `is_expired(now)` lets event-loop models drop a stalled
  connection on a timer tick; blocking models additionally rely on socket
  read/write timeouts. A slow client is never allowed to pin a worker forever.
- **Model-agnostic.** The same `Connection` type is used unchanged by blocking
  models and event-loop models.

### Usage skeletons

The two shapes every model fits, verified to type-check against the API in
`conn.rs` (`#[cfg(test)] mod skeletons`):

**Blocking** (`iterative`, `forking`, `thread-per-conn`, `thread-pool`):

```text
let mut conn = Connection::new(read_timeout);
let mut action = ConnAction::WantRead;
loop {
    match action {
        WantRead  => { n = stream.read(&mut buf)?; if n == 0 { break } action = conn.on_bytes(&buf[..n], &app); }
        WantWrite => { let w = stream.write(conn.pending_write())?; action = conn.on_written(w); }
        Close     => break,
    }
}
```

**Event loop** (`poll`, `epoll-*`, `event-loop`, `multireactor`, `io-uring`):

```text
on readable(fd):  loop { n = read(fd, buf) until EAGAIN; action = conn.on_bytes(&buf[..n], &app); } -> set interest from action
on writable(fd):  w = write(fd, conn.pending_write()); action = conn.on_written(w);                 -> set interest from action
on timer tick:    if conn.is_expired(now) { close(fd) }
```

## The Phase 0 model: `iterative`

A single thread that accepts one connection at a time and serves it to
completion using the blocking skeleton above. Its job is to be the **correct
reference** the later models are measured against:

- Every per-connection error is caught and (optionally, behind `--verbose`)
  logged off the hot path; the accept loop continues. One bad client never takes
  the server down.
- Read/write timeouts are set on every stream.
- Static assets are served from the in-memory cache, never re-read from disk.
- No hot-path logging by default.

## Why this shape (the through-line to later phases)

The model owns *only* its concurrency/I-O strategy; everything else is `core`.
That is what makes the eventual benchmark honest — the only thing that varies
between `iterative` and `io-uring` is how readiness/completion is obtained and
how work is dispatched, not the HTTP handling. It is also the direct rehearsal
for the target domain: a microVM sandbox control plane is a multi-reactor
event-loop server dispatching to isolated workers, and the per-connection state
machine here is the same shape as a sandbox lifecycle state machine.
