# Rust-Tcp-Server — Phase 1 Specification: Concurrency Models + Benchmark Harness

**Companion to:** `kickoff-brief.md` (strategy, full model list, the §4 common bar) and `phase0-spec.md` (the `core` foundation). Read both first.
**Scope of this document:** everything Phase 1 produces — the `sys` OS-I/O layer, the `reactor` layer, the eight remaining concurrency models (four fixed, four new), the `loadgen` load generator, and the `bench` harness. Plus the Claude Code execution plan (Appendix A & B).
**Audience:** the executing agent (Claude Code). This document is authoritative.

---

## 1. Phase 1 in one paragraph

Phase 0 built the sans-IO `core` and one model (`iterative`). Phase 1 builds the other ten models' worth of work: it fixes the four broken process/thread models, builds the four event-loop models (`poll`, `epoll-lt`, `epoll-et`, `event-loop`), and builds the measurement apparatus (`loadgen` + `bench/`) so every model is benchmarked the moment it compiles. Phase 1 stops short of `multireactor` and `io-uring` and the final writeup — those are Phase 2.

### 1.1 The layering (do not violate)

```
core      — protocol, SANS-IO. Frozen in Phase 1: add nothing to it.
  ↓
sys       — raw OS I/O: libc syscall wrappers, epoll, poll, nonblocking sockets, ConnTable.
  ↓         (new in Phase 1; lives in server/src/sys/)
reactor   — event-loop assembly: the Reactor struct (epoll-ET + timeouts + backpressure).
  ↓         (new in Phase 1; lives in server/src/reactor.rs)
models    — one concurrency strategy each, all implementing core::Server.
```

**`core` is frozen for Phase 1.** Phase 0's `Connection`, `ConnAction`, `RequestParser`, `Response::encode`, `Server`, `ServerConfig`, and `bind_listener` are already sufficient for every Phase 1 model. If a model appears to need a change to `core`, that is a signal the model is wrong — STOP and ask. Do not edit `core`.

---

## 2. Workspace layout after Phase 1

```
rust-tcp-server/
  Cargo.toml                 # workspace: core, server, loadgen
  core/                      # UNCHANGED in Phase 1
  server/
    src/
      main.rs                # extend CLI dispatch to all 11 models
      sys/                   # NEW — OS I/O layer
        mod.rs
        syscall.rs           # syscall result helper / errno handling
        socket.rs            # set_nonblocking, accept_nonblocking
        epoll.rs             # Epoll, Interest, Trigger, Event
        poll.rs              # poll(2) wrapper, PollFd
        conn_table.rs        # ConnTable: fd -> (TcpStream, Connection)
        signal.rs            # SIGCHLD / SIGINT-SIGTERM handlers (for process models)
      reactor.rs             # NEW — the Reactor struct
      models/
        iterative.rs         # from Phase 0
        forking.rs           # FIX
        preforked.rs         # FIX
        thread_per_conn.rs   # FIX
        thread_pool.rs       # FIX
        poll.rs              # NEW
        epoll.rs             # NEW — parametrized; exposes EpollLt + EpollEt
        event_loop.rs        # NEW — thin user of reactor.rs
      tests/
        conformance.rs       # parametrized over every implemented model
  loadgen/                   # NEW workspace member — the load generator (binary)
    src/main.rs
  bench/
    run.sh                   # orchestrates the model x concurrency sweep
    plot.py                  # matplotlib: distribution + throughput plots
    results/                 # COMMITTED CSVs, histogram dumps, rendered plots
```

Dependency allowlist for Phase 1 (add nothing else):
- `core`: unchanged (`socket2` only).
- `server`: `core`, `socket2`, **`libc`** (new — fork, waitpid, epoll, poll, accept4, signals).
- `loadgen`: **`hdrhistogram`** + std. Nothing else.
- `bench/plot.py`: matplotlib — a dev tool, not a cargo dependency.

---

## 3. The `sys` module — OS I/O primitives

Thin, honest wrappers over `libc`. `sys` does **not** hide the semantic differences between mechanisms (that is the whole point of having `poll`, `epoll-lt`, `epoll-et` as separate models). It removes copy-pasted FFI and fd bookkeeping — nothing more.

### 3.1 `syscall.rs`
A helper that turns a `libc` return value into `io::Result`, mapping `-1` to `io::Error::last_os_error()`. All `sys` code goes through it. (A small `syscall!` macro is acceptable.)

### 3.2 `socket.rs`
```rust
pub fn set_nonblocking(fd: RawFd) -> io::Result<()>;     // O_NONBLOCK via fcntl
/// accept4 with SOCK_NONBLOCK. Returns Ok(None) on EAGAIN/EWOULDBLOCK.
pub fn accept_nonblocking(listener_fd: RawFd) -> io::Result<Option<(RawFd, SocketAddr)>>;
```

### 3.3 `epoll.rs`
```rust
#[derive(Clone, Copy)]
pub enum Interest { Read, Write, ReadWrite }

#[derive(Clone, Copy)]
pub enum Trigger { Level, Edge }   // Edge => EPOLLET

pub struct Event { pub fd: RawFd, pub readable: bool, pub writable: bool,
                   pub hup: bool, pub error: bool }

pub struct Epoll { /* owns the epoll fd; closes on Drop */ }

impl Epoll {
    pub fn new() -> io::Result<Self>;                                       // epoll_create1
    pub fn add(&self, fd: RawFd, interest: Interest, trigger: Trigger) -> io::Result<()>;
    pub fn modify(&self, fd: RawFd, interest: Interest, trigger: Trigger) -> io::Result<()>;
    pub fn delete(&self, fd: RawFd) -> io::Result<()>;
    /// Blocks up to `timeout` (None = forever). Fills `events`, returns the count.
    pub fn wait(&self, events: &mut Vec<Event>, timeout: Option<Duration>) -> io::Result<usize>;
}
```
The `fd` registered is used directly as the epoll `u64` user-data, so `wait` can report it back. `hup`/`error` map `EPOLLHUP`/`EPOLLERR`.

### 3.4 `poll.rs`
```rust
pub struct PollFd { /* wraps libc::pollfd */ }
impl PollFd {
    pub fn new(fd: RawFd, interest: Interest) -> Self;
    pub fn readable(&self) -> bool;   // POLLIN in revents
    pub fn writable(&self) -> bool;   // POLLOUT in revents
    pub fn hup(&self) -> bool;        // POLLHUP
    pub fn error(&self) -> bool;      // POLLERR | POLLNVAL
}
/// Blocks up to `timeout`. Returns the number of fds with non-zero revents.
pub fn poll(fds: &mut [PollFd], timeout: Option<Duration>) -> io::Result<usize>;
```

### 3.5 `conn_table.rs`
```rust
/// Owns one event-loop's set of live connections.
/// The TcpStream owns the fd and closes it on remove/Drop.
pub struct ConnTable { /* HashMap<RawFd, Slot>, Slot { stream: TcpStream, conn: Connection } */ }

impl ConnTable {
    pub fn new() -> Self;
    pub fn insert(&mut self, stream: TcpStream, conn: Connection) -> RawFd;
    pub fn get_mut(&mut self, fd: RawFd) -> Option<&mut Slot>;
    pub fn remove(&mut self, fd: RawFd);                 // drop => fd closed
    pub fn len(&self) -> usize;
    /// For timeout scans: yields (fd, &Connection).
    pub fn iter(&self) -> impl Iterator<Item = (RawFd, &Connection)>;
}
```

### 3.6 `signal.rs`
```rust
/// Install a SIGCHLD handler that reaps with waitpid(WNOHANG) in a loop and
/// decrements a live-child AtomicUsize. Handler body must be async-signal-safe
/// (waitpid + atomic ops only).
pub fn install_sigchld_reaper(live_children: &'static AtomicUsize);

/// Install a SIGINT+SIGTERM handler that flips a shutdown AtomicBool.
pub fn install_shutdown_flag(flag: &'static AtomicBool);
```

---

## 4. The `reactor.rs` module — event-loop assembly

A single reusable `Reactor`: epoll edge-triggered, with timeout enforcement, connection-count backpressure, and read-buffer reuse. The `event-loop` model is a thin user of it; `multireactor` (Phase 2) instantiates it N times.

```rust
pub struct Reactor {
    epoll: Epoll,
    conns: ConnTable,
    listener: TcpListener,
    cfg: ServerConfig,
    read_buf: Vec<u8>,         // reused across reads — not reallocated per event
}

impl Reactor {
    /// Builds the reactor around an already-bound listener; registers it edge-triggered.
    pub fn new(listener: TcpListener, cfg: ServerConfig) -> io::Result<Self>;

    /// Runs until `shutdown` is set. Each iteration:
    ///   1. epoll.wait with a timeout = min(time-to-next-deadline, cap)
    ///   2. for the listener event: accept_nonblocking in a loop until EAGAIN
    ///      (drain — edge-triggered); for each new fd: set_nonblocking, build a
    ///      Connection, insert into ConnTable, register Read+Edge.
    ///   3. for each connection event: drain read() to EAGAIN feeding on_bytes /
    ///      drain write() of pending_write feeding on_written; apply ConnAction
    ///      to the epoll registration (Read/Write) or remove on Close.
    ///   4. scan ConnTable for is_expired(now); close expired connections.
    ///   5. backpressure: if conns.len() >= cfg.max_connections, deregister the
    ///      listener from epoll; re-register when capacity frees.
    pub fn run(&mut self, shutdown: &AtomicBool) -> io::Result<()>;
}
```

Backpressure policy (must be implemented and documented): at the connection cap, the listener is removed from epoll interest, so the kernel accept backlog absorbs new connections and ultimately refuses them — a deliberate, observable shed. Re-arm the listener when `conns.len()` drops below the cap.

---

## 5. The eight models

Every model implements `core::Server` and must pass the **§4 common bar** from the kickoff brief: correct (conformance suite), leak-free (flat RSS/fd count over a soak, no zombies), zero hot-path logging, read+write timeouts, HTTP/1.1 keep-alive, selectable via `--model`. Each model spec below gives the fix or design plus the **defining characteristic** to record for the eventual `BENCHMARKS.md`.

### 5.1 FIX — `forking.rs` (fork-per-connection)
**Bugs (Phase 0 audit §1.3):** no `waitpid` → zombies accumulate until `fork()` fails with `EAGAIN`; no cap → fork-bomb under load.
**Fix:** install the `sys::signal` SIGCHLD reaper + a live-child `AtomicUsize`. Before each `accept`+`fork`, if `live_children >= cfg.max_connections`, apply backpressure (close the just-accepted connection — reject fast). On fork: child `drop`s the listener and runs the blocking skeleton (phase0-spec §8.1) for exactly one connection then `exit(0)`; parent increments the counter and continues. The SIGCHLD handler decrements on reap.
**Defining characteristic:** bulletproof process isolation per connection; `fork()` cost (page-table COW setup) dominates; collapses under connection churn.

### 5.2 FIX — `preforked.rs` (N worker processes)
**Bugs:** children loop on `incoming()` (infinite) so the post-loop `break` is unreachable and children never exit → parent `waitpid` blocks forever; one shared listener fd → `accept` thundering herd.
**Fix:** parent forks `cfg.workers` children; **each child calls `bind_listener(addr, reuse_port = true)`** so every child owns its own `SO_REUSEPORT` listener — the kernel load-balances accepts, no shared fd, no thundering herd. Each child runs the `iterative` accept loop on its own listener, checking the `sys::signal` shutdown flag each iteration. Parent installs the shutdown handler, forwards the signal to the process group on SIGINT/SIGTERM, then `waitpid`s every child cleanly.
**Defining characteristic:** near-linear multicore scaling, zero shared state, zero lock contention; cost is N independent accept queues — a connection hashed to a busy worker cannot be stolen by an idle one (load imbalance under uneven connection lifetimes).

### 5.3 FIX — `thread_per_conn.rs` (thread-per-connection)
**Bugs:** confused fork+thread hybrid; `JoinHandle`s pushed inside an infinite loop and never joined → unbounded memory; unbounded `thread::spawn` → thread-bomb, ~8 MiB virtual stack each.
**Fix:** drop the fork entirely — single process. Accept loop spawns one detached `std::thread` per connection (do not retain the `JoinHandle`). Cap concurrency with a counting semaphore built from `Arc<(Mutex<usize>, Condvar)>` (std has no `Semaphore`): acquire a permit before spawn, release at thread end; at the cap the accept loop blocks on the `Condvar` rather than spawning.
**Defining characteristic:** simplest correct concurrency; the kernel scheduler does the multiplexing; per-thread stack + context-switch cost makes it the model that visibly degrades at C10K — it exists to motivate the event loop.

### 5.4 FIX — `thread_pool.rs` (bounded worker pool)
**State:** the old `server/lib.rs` `ThreadPool` was dead code; rebuild fresh in the new architecture.
**Design:** create `cfg.workers` worker threads at startup. Use a **bounded** `std::sync::mpsc::sync_channel(capacity)` as the job queue. The acceptor thread accepts connections and `try_send`s each `TcpStream` into the channel; workers `recv` and run the blocking skeleton. **Backpressure (explicit, documented):** on `TrySendError::Full`, close the connection immediately — reject fast under overload rather than blocking the acceptor. Graceful shutdown: drop the sender → workers observe disconnect → exit.
**Defining characteristic:** bounded, predictable resource use; explicit fast-reject backpressure; ceiling is `workers` concurrent slow requests — `workers` slow clients cause head-of-line blocking.

### 5.5 NEW — `poll.rs` (single-thread `poll(2)` event loop)
**Design:** single thread; non-blocking listener + non-blocking client sockets. Maintain a `Vec<PollFd>` and a `ConnTable`. Each iteration: rebuild the `PollFd` vector (listener always `Read`; each connection `Read` or `Write` per its last `ConnAction`); `sys::poll` with a timeout = time-to-next-deadline; **scan every returned fd** (O(n)); accept new connections or drive the `Connection`; close expired connections.
**Defining characteristic:** `poll` rebuilds and rescans the entire fd set every iteration — O(n) per wakeup. This is the readiness-I/O baseline that makes epoll's O(ready) advantage measurable. Inherently level-triggered.

### 5.6 NEW — `epoll.rs` (epoll, parametrized — exposes `EpollLt` and `EpollEt`)
One implementation, `fn run_epoll(trigger: Trigger, drain: bool, ...)`, with two thin `Server` impls — `EpollLt { Level, drain: false }` and `EpollEt { Edge, drain: true }`. Same code path; only the trigger and drain discipline differ, so the benchmark isolates exactly the LT-vs-ET difference.
**Design:** single thread; non-blocking sockets; `sys::Epoll` + `ConnTable`. Register the listener and each connection with `trigger`. On a connection's readable event: if `drain`, loop `read()` until `EAGAIN`, feeding each chunk to `on_bytes`; else a single `read()`. On writable: write `pending_write`, on `EAGAIN` keep the `Write` registration and resume on the next event. On the listener event: if `drain`, `accept_nonblocking` in a loop until `EAGAIN`; else once. Apply `ConnAction` via `epoll.modify`; `Close` → `epoll.delete` + `ConnTable.remove`. Scan for expired connections each iteration.
**Defining characteristic:** epoll's ready-list is O(ready), not O(total) — the win over `poll`. **Edge-triggered correctness rule:** ET fires once per transition, so reads and accepts MUST drain to `EAGAIN` or events are lost and the connection hangs. ET minimizes `epoll_wait` wakeups at the cost of that drain discipline — the "understand the API to the floor" model.

### 5.7 NEW — `event_loop.rs` (the `Reactor`-based model)
A thin model: build a listener, construct a `reactor::Reactor`, call `run(&shutdown)`. All logic lives in `reactor.rs` (§4).
**Defining characteristic:** the production-shaped reactor — epoll-ET plus timeout enforcement, connection-cap backpressure, and buffer reuse — packaged as a reusable `Reactor`. Benchmarked against bare `epoll-et`, it answers: does adding production concerns cost latency? (A near-zero delta is the desired, reportable result.) This `Reactor` is what `multireactor` reuses in Phase 2.

---

## 6. The `loadgen` crate — open-loop load generator

A standalone binary. **Open-loop and coordinated-omission-correct** — this property is the point; a closed-loop loader silently under-reports tail latency.

**Coordinated-omission rule:** requests are *scheduled* at a fixed cadence for a target rate R — at times `t0, t0 + 1/R, t0 + 2/R, …`. Latency for each request is `response_received − scheduled_time`, **never** `response_received − actual_send_time`. If the server stalls and a request cannot even be sent on schedule, the queueing delay still counts as latency — because a real user does not wait politely for the server to free up. This is what a closed-loop loader omits.

**Design:**
- CLI: `--target <host:port>`, `--rate <req/s>`, `--connections <M>`, `--duration <secs>`, `--out <csv path>`.
- `M` persistent keep-alive TCP connections to the server.
- A schedule of send-times for rate R; a dispatcher assigns each scheduled request (carrying its `scheduled_time`) to a free connection; if none is free the request waits, and the wait correctly counts toward its latency.
- Each request is a fixed `GET / HTTP/1.1` with `Connection: keep-alive`; the response reader consumes the status line + headers + `Content-Length` body to confirm completion.
- Record `received − scheduled` into an `hdrhistogram::Histogram<u64>` (microseconds).
- Output: one CSV row — `model, rate, connections, throughput_rps, errors, p50, p90, p99, p999, p9999, max` — appended to `--out`; plus a full histogram dump (`.hgrm` or percentile CSV) for distribution plots.

`loadgen` does not depend on `core` — it is a client and writes/reads raw HTTP.

---

## 7. The `bench/` harness

- `bench/run.sh` — for each implemented model: start `server --model <m>`, wait until the port accepts, run `loadgen` across the concurrency/rate sweep (**1, 10, 100, 1000, 10000**), stop the server, append results to `bench/results/<model>.csv` and save histogram dumps. If `perf` is available and `--perf` is passed, wrap one mid-range run in `perf stat`. The script must be idempotent and must clean up server processes on exit.
- `bench/plot.py` — matplotlib: renders the interior latency-distribution plot (full histogram, log-y), the throughput-vs-concurrency plot, and a p99-vs-concurrency plot, into `bench/results/`.
- `bench/results/` — committed: CSVs, histogram dumps, rendered plots.

---

## 8. Tests

Extend `server/tests/conformance.rs` into a parametrized suite: `fn run_conformance(model: &str)` spawns `server --model <model>` and asserts — 200 on `GET /`, 404 on an unknown path, 400 on a malformed request line, 405 on a non-GET method, a successful keep-alive reuse, and **that the server survives the malformed request**. Every model session adds its model(s) to the parametrized list. Each model session also runs a 60-second soak under `loadgen` and confirms flat RSS, flat fd count, and no zombie processes. The full 10-minute soak per model runs in Session 7.

---

## 9. Phase 1 Definition of Done

Phase 1 is complete only when **all** hold:
1. `core` is byte-for-byte unchanged from end of Phase 0.
2. `sys/` and `reactor.rs` implemented per §3–§4; `cargo clippy` clean (no warnings).
3. All eight models (§5) implemented, each passing the §4 common bar, each in the parametrized conformance suite, each passing a 60-second soak (flat RSS/fd, no zombies).
4. `main.rs` dispatches all 11 model names; unknown names still exit with a clear message.
5. `loadgen` implemented per §6 — open-loop, coordinated-omission-correct, hdrhistogram, CSV output.
6. `bench/run.sh` runs the full sweep; `bench/results/` populated with CSVs, histogram dumps, and `plot.py`-rendered plots for all 9 models (iterative + the 8).
7. A 10-minute soak passes for every model.
8. `cargo build`, `cargo clippy`, `cargo test` clean across the workspace.
9. Dependency allowlist (§2) respected.

Out of scope for Phase 1 (do NOT implement): `multireactor`; `io-uring`; the `BENCHMARKS.md` writeup; the README teardown; TLS; HTTP/2. Those are Phase 2.

---

# Appendix A — `CLAUDE.md` update for Phase 1

Replace the "Authoritative specs" and "Hard rules" / dependency lines in the repo-root `CLAUDE.md` with:

```markdown
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
```

---

# Appendix B — Claude Code execution plan (7 sessions)

Run one or two sessions per 5-hour window; verify and commit between each. `loadgen` is built second (Session 2) so every model session can measure itself the moment it compiles.

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | `sys` layer | `server/src/sys/` complete (§3); `libc` added | `cargo build`/`clippy` clean |
| 2 | `loadgen` | `loadgen` crate complete (§6) | runs against the Phase 0 `iterative` server |
| 3 | Process models | `forking` + `preforked` fixed (§5.1–5.2) | conformance + 60s soak pass |
| 4 | Thread models | `thread-per-conn` + `thread-pool` fixed (§5.3–5.4) | conformance + 60s soak pass |
| 5 | poll + epoll | `poll.rs` + `epoll.rs` (LT+ET) (§5.5–5.6) | conformance + 60s soak pass |
| 6 | reactor + event-loop | `reactor.rs` + `event_loop.rs` (§4, §5.7) | conformance + 60s soak pass |
| 7 | Harness + sweep | `bench/run.sh` + `plot.py`; full sweep; 10-min soaks; DoD §9 | Phase 1 DoD met |

If a session's context grows large (Session 5 especially), split at the natural boundary (`poll.rs`, then `epoll.rs`) and commit the first half first.

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read `CLAUDE.md`, `docs/specs/phase1-spec.md` §1–§3, and `docs/specs/phase0-spec.md` for context. Update `CLAUDE.md` per Appendix A of the phase-1 spec. Then execute **Session 1 only**: implement the `server/src/sys/` module — `syscall.rs`, `socket.rs`, `epoll.rs`, `poll.rs`, `conn_table.rs`, `signal.rs` — exactly per §3, and add the `libc` dependency to `server`. Do not touch `core`. Do not implement any model or the reactor. Add unit tests where feasible (e.g. `Epoll` add/wait against a pipe). `cargo build` and `cargo clippy` must be clean. Commit per file, run the checks, list changes, and STOP.

**Session 2**
> Read `CLAUDE.md` and `docs/specs/phase1-spec.md` §6. Execute **Session 2 only**: implement the `loadgen` workspace member per §6 — open-loop, coordinated-omission-correct (latency = received − scheduled), keep-alive connections, `hdrhistogram`, CSV + histogram-dump output. Add `loadgen` to the workspace `Cargo.toml`. Verify by running it against `server --model iterative`. Do not touch `core`, `sys`, or any model. `cargo build`/`clippy`/`test` clean. Commit, list changes, and STOP.

**Session 3**
> Read `CLAUDE.md` and `docs/specs/phase1-spec.md` §5.1–§5.2, §8. Execute **Session 3 only**: fix the `forking` and `preforked` models per §5.1–§5.2 (SIGCHLD reaper + child cap; `SO_REUSEPORT` per child + clean signal-driven shutdown). Add both to the parametrized conformance suite (§8) and run a 60-second `loadgen` soak against each, confirming flat RSS/fd and no zombies. Do not touch `core` or other models. Commit, run checks, list changes, and STOP.

**Session 4**
> Read `CLAUDE.md` and `docs/specs/phase1-spec.md` §5.3–§5.4, §8. Execute **Session 4 only**: fix the `thread-per-conn` and `thread-pool` models per §5.3–§5.4 (detached threads + Condvar semaphore cap; bounded `sync_channel` + fast-reject backpressure). Add both to the conformance suite and run a 60-second soak against each. Do not touch `core` or other models. Commit, run checks, list changes, and STOP.

**Session 5**
> Read `CLAUDE.md` and `docs/specs/phase1-spec.md` §5.5–§5.6, §8. Execute **Session 5 only**: implement `models/poll.rs` and `models/epoll.rs` per §5.5–§5.6 — `epoll.rs` is one parametrized implementation exposing `EpollLt` (Level, no drain) and `EpollEt` (Edge, drain to EAGAIN, partial-write resumption). Add all three model names to the conformance suite and run a 60-second soak against each. If context grows large, commit `poll.rs` before starting `epoll.rs`. Do not touch `core`. Commit, run checks, list changes, and STOP.

**Session 6**
> Read `CLAUDE.md` and `docs/specs/phase1-spec.md` §4, §5.7, §8. Execute **Session 6 only**: implement `server/src/reactor.rs` (the `Reactor` — epoll-ET + timeout enforcement + connection-cap backpressure + buffer reuse) per §4, and the thin `models/event_loop.rs` per §5.7. Add `event-loop` to the conformance suite and run a 60-second soak. Do not implement `multireactor`. Do not touch `core`. Commit, run checks, list changes, and STOP.

**Session 7**
> Read `CLAUDE.md` and `docs/specs/phase1-spec.md` §7, §9. Execute **Session 7 only**: implement `bench/run.sh` and `bench/plot.py` per §7; run the full model × concurrency sweep (1/10/100/1000/10000) across all 9 implemented models; populate `bench/results/` with CSVs, histogram dumps, and rendered plots; run a 10-minute soak per model. Then verify the Phase 1 Definition of Done §9 item by item and report the result of each. Commit, and STOP. Phase 1 is complete.
