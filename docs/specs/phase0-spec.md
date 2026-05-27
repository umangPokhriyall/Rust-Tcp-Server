# Rust-Tcp-Server — Phase 0 Specification: the `core` Foundation

**Companion to:** `kickoff-brief.md` (read that first for the strategic frame).
**Scope of this document:** the exact public API of the `core` crate, the `iterative` model, and the `server` binary — i.e. everything Phase 0 produces. Plus the Claude Code execution plan (Appendix A & B).
**Audience:** the executing agent (Claude Code). This document is authoritative. If anything here conflicts with intuition or with older code, follow this document.

---

## 1. The one principle everything depends on: `core` is sans-IO

`core` contains **zero socket I/O**. It never calls `read`, `write`, `accept`, or `epoll`. It is a pure library of: an incremental HTTP parser, a response encoder, a router, an asset cache, and a per-connection state machine — all of which operate **only on byte buffers in memory**.

The *models* (`iterative`, `epoll-et`, `io-uring`, …) own every syscall. They obtain readiness however their strategy dictates, perform the reads and writes, and feed bytes to / drain bytes from `core`.

**Why this is non-negotiable:** a blocking model and an `io_uring` completion-based model have nothing in common at the I/O layer. The *only* way one `core` serves all 11 models unchanged is if `core` never touches a socket. The current repo's `route_client(TcpStream)` is blocking-coupled and is precisely why it cannot extend to `epoll`. Do not reintroduce that coupling anywhere in `core`.

Every parser/encoder/connection method below is marked **sans-IO**. That means: no I/O, no blocking, safe to call repeatedly, returns a status the caller acts on.

---

## 2. `core` crate layout

```
core/
  Cargo.toml
  src/
    lib.rs          # re-exports the public surface (§10)
    limits.rs       # bounded-input constants
    http/
      mod.rs
      method.rs     # Method, Version
      headers.rs    # Headers
      request.rs    # Request, RequestParser, ParseStatus, ParseError
      response.rs   # Response, StatusCode, Body
    router.rs       # Router, Handler
    asset.rs        # AssetCache, Asset
    app.rs          # App, AppBuilder
    conn.rs         # Connection, ConnAction
    server.rs       # Server trait, ServerConfig, bind_listener
    metrics.rs      # Metrics
```

`core` Phase 0 dependency allowlist: **`socket2` only.** No `tokio`, no `libc`, no async, nothing else. (`std::net::TcpStream` already provides `set_read_timeout` / `set_write_timeout`, so the `iterative` model needs no `libc`.)

---

## 3. `limits.rs` — bounded inputs (DoS defense)

Every constant below is a hard ceiling the parser enforces; exceeding one is a fatal parse error. This is what makes the server slow-loris- and memory-DoS-resistant.

```rust
pub const MAX_REQUEST_LINE: usize = 8 * 1024;      // request line
pub const MAX_HEADER_BYTES:  usize = 32 * 1024;    // entire header block
pub const MAX_HEADER_COUNT:  usize = 100;
pub const MAX_BODY_BYTES:    usize = 1 * 1024 * 1024;
pub const READ_CHUNK:        usize = 16 * 1024;    // suggested per-read size
```

---

## 4. `http` module

### 4.1 `method.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method { Get, Head, Unsupported }

impl Method {
    /// GET -> Get, HEAD -> Head, anything else -> Unsupported.
    pub fn parse(token: &[u8]) -> Method;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version { Http10, Http11 }

impl Version {
    /// "HTTP/1.0" / "HTTP/1.1" -> Some(..); anything else -> None.
    pub fn parse(token: &[u8]) -> Option<Version>;
}
```

### 4.2 `headers.rs`

```rust
#[derive(Debug, Default)]
pub struct Headers { /* Vec<(String, String)> */ }

impl Headers {
    pub fn new() -> Self;
    /// Case-INSENSITIVE name lookup.
    pub fn get(&self, name: &str) -> Option<&str>;
    pub fn insert(&mut self, name: &str, value: &str);
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)>;
}
```

Implementation note: a linear-scan `Vec`, **not** a `HashMap`. Header counts are tiny (< 100), a `Vec` is cache-friendlier, and avoiding a hash per lookup is the correct mechanical-sympathy call at this scale. Do not use a `HashMap` here.

### 4.3 `request.rs` — the incremental parser (the crux of Phase 0)

```rust
#[derive(Debug)]
pub struct Request {
    pub method:  Method,
    pub path:    String,
    pub version: Version,
    pub headers: Headers,
    pub body:    Vec<u8>,
}

impl Request {
    /// HTTP/1.1: keep-alive unless `Connection: close`.
    /// HTTP/1.0: close unless `Connection: keep-alive`.
    pub fn wants_keep_alive(&self) -> bool;
    /// Content-Length value, or 0 if absent.
    pub fn content_length(&self) -> usize;
}

#[derive(Debug, Clone, Copy)]
pub enum ParseError {
    MalformedRequestLine,
    UnsupportedVersion,
    MalformedHeader,
    RequestLineTooLong,
    HeadersTooLarge,
    TooManyHeaders,
    BodyTooLarge,
}

impl ParseError {
    /// The status to answer with before closing the connection.
    /// e.g. MalformedRequestLine->400, RequestLineTooLong->414,
    /// HeadersTooLarge/TooManyHeaders->431, BodyTooLarge->413,
    /// UnsupportedVersion->505.
    pub fn status(&self) -> StatusCode;
}

#[derive(Debug)]
pub enum ParseStatus {
    /// Need more bytes. Caller reads more and calls `push` again.
    Incomplete,
    /// A full request is ready. `consumed` = bytes used from the input
    /// stream so far; bytes beyond it belong to the NEXT request.
    Complete { request: Request, consumed: usize },
    /// Fatal. Caller answers with `error.status()`, then closes.
    Failed(ParseError),
}

pub struct RequestParser { /* internal accumulation buffer + state machine */ }

impl RequestParser {
    pub fn new() -> Self;

    /// SANS-IO. Append `bytes` to the internal buffer and advance parsing.
    /// MUST be safe to call repeatedly. MUST handle the request arriving
    /// one byte at a time. MUST NOT block or perform I/O.
    pub fn push(&mut self, bytes: &[u8]) -> ParseStatus;

    /// Prepare to parse the next request on a kept-alive connection.
    /// Bytes received after the previous `Complete { consumed }` MUST be
    /// retained (so a pipelined request is not lost).
    pub fn reset(&mut self);
}
```

**Parser contracts (the agent must honor all of these):**
- Sans-IO. Never reads a socket.
- Internal state machine: `RequestLine -> Headers -> Body -> Done`. Internal representation is the agent's choice; only the public surface above is fixed.
- Must parse correctly when fed in arbitrarily small chunks, including one byte at a time.
- Enforces every `limits.rs` ceiling → `Failed` on breach.
- On `Complete`, retains post-`consumed` bytes for the next request.
- Phase 0 must handle **keep-alive** (sequential requests on one connection). Full pipelining (multiple in-flight) is *not* required for Phase 0, but the retain-leftover-bytes rule above must hold so pipelining can be added later without redesign.

### 4.4 `response.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusCode(pub u16);

impl StatusCode {
    pub const OK: StatusCode = StatusCode(200);
    pub const BAD_REQUEST: StatusCode = StatusCode(400);
    pub const NOT_FOUND: StatusCode = StatusCode(404);
    pub const METHOD_NOT_ALLOWED: StatusCode = StatusCode(405);
    pub const URI_TOO_LONG: StatusCode = StatusCode(414);
    pub const PAYLOAD_TOO_LARGE: StatusCode = StatusCode(413);
    pub const HEADER_FIELDS_TOO_LARGE: StatusCode = StatusCode(431);
    pub const INTERNAL_ERROR: StatusCode = StatusCode(500);
    pub const VERSION_NOT_SUPPORTED: StatusCode = StatusCode(505);
    pub fn reason(&self) -> &'static str;
}

#[derive(Debug, Clone)]
pub enum Body {
    Empty,
    /// Asset bodies are Arc-shared — handlers clone the Arc, never the bytes.
    Bytes(std::sync::Arc<[u8]>),
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status:  StatusCode,
    pub headers: Headers,
    pub body:    Body,
}

impl Response {
    pub fn new(status: StatusCode) -> Self;
    pub fn ok(content_type: &str, body: std::sync::Arc<[u8]>) -> Self;
    pub fn text(status: StatusCode, msg: &str) -> Self;
    pub fn with_header(self, name: &str, value: &str) -> Self;

    /// SANS-IO. Serialize status line + headers + body into `out`.
    /// MUST write a correct `Content-Length` and a `Connection` header
    /// consistent with `keep_alive`. When `include_body` is false (HEAD),
    /// write headers + Content-Length but omit the body bytes.
    pub fn encode(&self, keep_alive: bool, include_body: bool, out: &mut Vec<u8>);
}
```

---

## 5. `router.rs`

```rust
/// A handler is a PURE function: (request, app) -> response.
/// No I/O, no blocking. (This replaces the old `fn(TcpStream) -> Result<()>`.)
pub type Handler = fn(req: &Request, app: &App) -> Response;

pub struct Router { /* the existing trie — reuse the `Node` structure */ }

impl Router {
    pub fn new() -> Self;
    pub fn insert(&mut self, method: Method, path: &str, handler: Handler);
    pub fn lookup(&self, method: Method, path: &str) -> Option<Handler>;
}
```

Note: the existing trie (`Node`) data structure is fine — **port it, do not rewrite it**. The only change is the handler type (pure `Handler` instead of `fn(TcpStream)`).

---

## 6. `asset.rs`

```rust
pub struct Asset {
    pub bytes:        std::sync::Arc<[u8]>,
    pub content_type: &'static str,
}

/// Static files loaded into memory ONCE at boot, served from RAM thereafter.
/// No per-request disk I/O — this is a benchmark-correctness requirement.
pub struct AssetCache { /* HashMap<String, Asset> */ }

impl AssetCache {
    /// Load every file under `dir` into memory. Called once, at startup.
    pub fn load_dir(dir: &std::path::Path) -> std::io::Result<Self>;
    pub fn get(&self, name: &str) -> Option<&Asset>;
}
```

---

## 7. `app.rs`

```rust
/// Immutable shared state. Built once at boot, wrapped in Arc, shared
/// read-only by every worker / thread / forked child. MUST be Send + Sync.
pub struct App {
    /* router, assets, metrics — all private */
}

impl App {
    pub fn builder() -> AppBuilder;
    pub fn router(&self)  -> &Router;
    pub fn assets(&self)  -> &AssetCache;
    pub fn metrics(&self) -> &Metrics;

    /// Route a parsed request to a Response.
    /// No matching route -> 404. Unsupported method -> 405.
    pub fn handle(&self, req: &Request) -> Response;
}

pub struct AppBuilder { /* ... */ }

impl AppBuilder {
    pub fn route(self, method: Method, path: &str, handler: Handler) -> Self;
    pub fn assets_dir(self, dir: &std::path::Path) -> Self;
    pub fn build(self) -> std::io::Result<App>;
}
```

---

## 8. `conn.rs` — the per-connection state machine

This is the component every one of the 11 models drives. It owns the protocol state for one connection and is **sans-IO**: the model performs all reads/writes and feeds/drains bytes here.

```rust
/// What the connection wants the model to do next.
#[derive(Debug, PartialEq, Eq)]
pub enum ConnAction {
    WantRead,   // wait for readability, then call `on_bytes`
    WantWrite,  // wait for writability, then write `pending_write` + call `on_written`
    Close,      // close the fd and drop this Connection
}

pub struct Connection { /* parser, state, keep_alive flag, deadline — all private */ }

impl Connection {
    pub fn new(read_timeout: std::time::Duration) -> Self;

    /// Feed bytes the model just read from the socket. Parses; on a complete
    /// request, routes via `app` and encodes the response into the internal
    /// write buffer. On a parse error, internally builds the error response
    /// (status from `ParseError::status()`), so the model never has to.
    /// Returns the next action.
    pub fn on_bytes(&mut self, bytes: &[u8], app: &App) -> ConnAction;

    /// Bytes the model should write now (empty slice if nothing pending).
    pub fn pending_write(&self) -> &[u8];

    /// Model reports it wrote `n` bytes of `pending_write`. Advances the
    /// write offset; when fully drained, transitions to Reading (keep-alive,
    /// and the read deadline is refreshed) or Closing. Returns next action.
    pub fn on_written(&mut self, n: usize) -> ConnAction;

    /// True if the connection exceeded its deadline.
    pub fn is_expired(&self, now: std::time::Instant) -> bool;
}
```

**Connection contracts:**
- Sans-IO. Performs no syscalls.
- On a parse `Failed`, `on_bytes` itself produces the error response, encodes it with `keep_alive = false`, and the post-write action is `Close`. The model never sees the error — it just gets `WantWrite` then `Close`. **This is the one-place fix for the current repo's "one bad request kills the server" bug.**
- For a `HEAD` request, `on_bytes` encodes with `include_body = false`.
- On keep-alive (`on_written` fully drains and the request wanted keep-alive), transition back to `Reading` and **refresh the deadline**.
- The same `Connection` type is used by the blocking models and the event-loop models without modification.

### 8.1 Required usage skeletons (the agent must verify both compile against this API)

Blocking model (`iterative`, `forking`, `thread-per-conn`, `thread-pool`):
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

Event-loop model (`poll`, `epoll-*`, `event-loop`, `multireactor`, `io-uring`):
```text
on readable(fd):  loop { n = read(fd, buf) until EAGAIN; action = conn.on_bytes(&buf[..n], &app); } -> set epoll interest from `action`
on writable(fd):  w = write(fd, conn.pending_write()); action = conn.on_written(w);                 -> set epoll interest from `action`
on timer tick:    if conn.is_expired(now) { close(fd) }
```

---

## 9. `server.rs` and `metrics.rs`

```rust
pub struct ServerConfig {
    pub addr:           std::net::SocketAddr,
    pub workers:        usize,                  // preforked / multireactor / thread-pool
    pub read_timeout:   std::time::Duration,
    pub write_timeout:  std::time::Duration,
    pub max_connections: usize,                 // backpressure ceiling
    pub assets_dir:     std::path::PathBuf,
}
impl Default for ServerConfig { /* sane defaults */ }

/// Every concurrency model implements this ONE trait.
pub trait Server {
    fn name(&self) -> &'static str;
    /// Runs until the process is signalled to stop.
    fn serve(&self, cfg: &ServerConfig, app: std::sync::Arc<App>) -> std::io::Result<()>;
}

/// Bind a TCP listener. `reuse_port = true` sets SO_REUSEPORT + SO_REUSEADDR
/// (via `socket2`) so preforked children / multireactor threads can each own
/// a listener on the same address. Phase 0 only ever calls it with `false`,
/// but the `true` path must be implemented.
pub fn bind_listener(addr: std::net::SocketAddr, reuse_port: bool)
    -> std::io::Result<std::net::TcpListener>;
```

```rust
// metrics.rs — minimal for Phase 0. Latency histograms are NOT in Phase 0
// (the load generator measures latency client-side). Do not add them here.
#[derive(Default)]
pub struct Metrics {
    pub connections: std::sync::atomic::AtomicU64,
    pub requests:    std::sync::atomic::AtomicU64,
    pub errors:      std::sync::atomic::AtomicU64,
}
impl Metrics {
    pub fn new() -> Self;
    pub fn inc_connections(&self);
    pub fn inc_requests(&self);
    pub fn inc_errors(&self);
}
```

---

## 10. `lib.rs` — public surface

`lib.rs` re-exports exactly: `Method`, `Version`, `Headers`, `Request`, `RequestParser`, `ParseStatus`, `ParseError`, `Response`, `StatusCode`, `Body`, `Router`, `Handler`, `AssetCache`, `Asset`, `App`, `AppBuilder`, `Connection`, `ConnAction`, `Server`, `ServerConfig`, `bind_listener`, `Metrics`, and the `limits` module. Nothing else is `pub`.

---

## 11. The `server` binary and the `iterative` model

`server/src/main.rs`:
- Hand-rolled arg parsing (no `clap`) for: `--model <name>`, `--port <n>`, `--assets-dir <path>`.
- Builds `App` via `AppBuilder`: registers `GET /` and `GET /static/style.css` to handlers that pull the corresponding `Asset` from the cache and return `Response::ok(...)`.
- Looks up the model by name and calls `serve`. For Phase 0, **only `iterative` is wired**; any other name prints `"model '<name>' not implemented yet"` and exits 1. (This is deliberate — it stops the agent from drifting into Phase 1.)

`server/src/models/iterative.rs` — implements `Server`:
- `serve`: `bind_listener(cfg.addr, false)`, then loop `accept()`.
- For each accepted stream: set `set_read_timeout` / `set_write_timeout` from `cfg`; build a `Connection`; run the blocking skeleton from §8.1.
- **Every per-connection error is caught, logged off the hot path, and the loop continues. Never `?`-propagate a per-client error out of the accept loop.** (This is the fix for the current `iterative` bug.)
- No `println!` on the hot path. Per-connection/request logging is gated behind a `--verbose` flag or a `log`-level check, off by default.

---

## 12. Phase 0 Definition of Done

Phase 0 is complete only when **all** hold:
1. Workspace has exactly two members: `core` and `server`. The six legacy crates are removed from `main` (preserved on the `legacy-snapshot` branch).
2. `core` exposes exactly the API in §3–§10. No socket I/O anywhere in `core`.
3. `cargo build` and `cargo clippy` are clean (no warnings) for the whole workspace.
4. `cargo test` passes: unit tests for `RequestParser` (well-formed; byte-at-a-time; malformed; each limit breach) and for `Response::encode`; one integration test that spawns the `iterative` server and verifies a 200, a 404, a 400 on a malformed request, and a keep-alive reuse.
5. `server --model iterative --port 8080 --assets-dir server/assets` runs; `curl` against it returns the index page; a malformed request returns 400 and the server **stays up**.
6. `docs/ARCHITECTURE.md` describes the sans-IO design and the `Connection` contract.
7. Dependency allowlist respected: `core` uses only `socket2`; `server` uses only `core` + std.

Out of scope for Phase 0 (do NOT implement): any model other than `iterative`; the benchmark harness / load generator; latency histograms; TLS; HTTP/2; async runtimes.

---

# Appendix A — `CLAUDE.md` (copy verbatim to the repo root)

```markdown
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
```

---

# Appendix B — Claude Code execution plan (6 sessions)

Each session is one focused, independently-committable deliverable. Run **one or two per 5-hour window**; verify and commit between each. Phase 0 spans roughly 3–4 windows.

| # | Session | Deliverable | Done when |
|---|---|---|---|
| A | Skeleton | Workspace + `core` modules with all types/signatures, `todo!()` bodies; `CLAUDE.md`; `docs/ARCHITECTURE.md` stub; legacy crates removed | `cargo build` clean |
| B | HTTP parser | `http/` fully implemented: parser, request, response, method, headers + unit tests | `cargo test` green for `http` |
| C | Routing/state | `router.rs`, `asset.rs`, `app.rs`, `metrics.rs` implemented | `cargo test` green |
| D | Connection/server | `conn.rs` + `server.rs` implemented; both §8.1 skeletons type-check | `cargo build` clean |
| E | Binary + model | `server` binary: CLI + `iterative` model wired | `curl` returns the index page |
| F | Tests + docs | Integration tests; `ARCHITECTURE.md` filled in; full verification | Phase 0 Definition of Done met |

If a session's context grows large mid-way (e.g. Session B), split it at a natural boundary (parser side / response side) and commit the first half before continuing.

### Exact prompts (paste one per session, verify + commit before the next)

**Session A**
> Read `CLAUDE.md`, `docs/specs/phase0-spec.md`, and `docs/specs/kickoff-brief.md` in full. Then execute **Session A only** from Appendix B of the phase-0 spec: create the two-member workspace (`core`, `server`); create every `core` module with all public types and signatures from §3–§10 using `todo!()` or minimal bodies so the crate compiles; create `CLAUDE.md` at the repo root and a `docs/ARCHITECTURE.md` stub; and remove the six legacy crates from `main` after first creating a `legacy-snapshot` branch that preserves them. Implement no logic and no server model. The workspace must `cargo build` cleanly. Commit each file as it compiles. When done, run `cargo build`, show the output, list what you created, and STOP — do not start Session B.

**Session B**
> Read `CLAUDE.md` and `docs/specs/phase0-spec.md` §4. Execute **Session B only**: fully implement the `core::http` module — `Method`, `Version`, `Headers`, `Request`, `RequestParser`, `ParseStatus`, `ParseError`, `Response`, `StatusCode`, `Body` — per §4, honoring every parser contract (sans-IO, byte-at-a-time, all limits enforced). Add unit tests: well-formed request, request fed one byte at a time, each malformed/oversized case, and `Response::encode`. Do not touch any other module. `cargo test` must pass for `http`. Commit per file, then run `cargo test`, list changes, and STOP.

**Session C**
> Read `CLAUDE.md` and `docs/specs/phase0-spec.md` §5–§7, §9. Execute **Session C only**: implement `router.rs` (port the existing trie, change the handler type to the pure `Handler`), `asset.rs`, `app.rs` + `AppBuilder`, and `metrics.rs`. Do not implement `conn.rs`, `server.rs`, or any model. `cargo build` and `cargo test` must be clean. Commit per file, then STOP.

**Session D**
> Read `CLAUDE.md` and `docs/specs/phase0-spec.md` §8–§9. Execute **Session D only**: implement `conn.rs` (`Connection`, `ConnAction`) honoring every contract in §8 — sans-IO, in-connection error responses, HEAD handling, keep-alive deadline refresh — and `server.rs` (`Server` trait, `ServerConfig`, `bind_listener` via `socket2`, both `reuse_port` paths). Verify both usage skeletons in §8.1 type-check against your API (a compile check is enough). Implement no model. `cargo build`/`clippy` clean. Commit, then STOP.

**Session E**
> Read `CLAUDE.md` and `docs/specs/phase0-spec.md` §11. Execute **Session E only**: implement the `server` binary — hand-rolled CLI (`--model`, `--port`, `--assets-dir`), `App` construction with the two routes, model dispatch — and the `iterative` model per §11 (catch and log every per-connection error, never propagate; no hot-path logging). Only `iterative` is wired; other names exit with a not-implemented message. Verify by running the server and `curl`-ing it. Commit, then STOP.

**Session F**
> Read `CLAUDE.md` and `docs/specs/phase0-spec.md` §12. Execute **Session F only**: add the integration test (spawn `iterative`; assert 200, 404, 400-on-malformed, keep-alive reuse, and that the server survives the malformed request); fill in `docs/ARCHITECTURE.md` (the sans-IO design + the `Connection` contract). Then verify the full Phase 0 Definition of Done §12 item by item and report the result of each. Commit, then STOP. Phase 0 is complete.
