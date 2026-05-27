# Rust-Tcp-Server — Per-Chat Kickoff Brief & Execution Spec

**Repo:** https://github.com/umangPokhriyall/Rust-Tcp-Server
**Owner:** internet-native systems engineer, no formal industry experience, building proof-of-work.
**This document is the complete spec.** The executing chat has no other context. Read it fully before writing code.

---

## 0. Why this repo exists (the strategic frame)

This repo is **Repo 1 of a 5-repo "Systems Polish Sprint,"** itself the first 3 weeks of a 90-day plan to break into **AI agent-infrastructure / microVM sandboxing** companies (E2B, Modal, Daytona, Northflank, Composio-class). The owner has no pedigree, so the artifact must be an *unfakeable, falsifiable* demonstration of elite systems execution.

This is not a tutorial repo. The deliverable is a **benchmark teardown of the full evolution of TCP server I/O models — from `accept()`-in-a-loop to `io_uring` — built behind one interface, measured rigorously, and explained mechanistically.**

**Why this specific artifact neutralizes the lack of pedigree:**
1. **Benchmarks are objective.** A p99 number from a working server does not care what school the author went to.
2. **The iterative → io_uring progression *is* senior systems knowledge.** Understanding *why* each model supersedes the last (syscall cost, context-switch cost, readiness vs completion I/O) is exactly what a Principal Engineer probes in an interview.
3. **The writeup proves you can explain mechanism**, not just produce code — the single hardest thing to fake.
4. **It is the literal substrate of the target domain.** A microVM sandbox control plane *is* a multi-reactor event-loop server that dispatches to isolated workers. Building this repo rehearses the 90-day flagship.

**Direct microVM mapping (state this in the README):** the multi-reactor model = the sandbox API control plane (acceptor + N reactors); the epoll-ET / io_uring event loop = the host↔guest I/O multiplexer; the per-connection state machine = the sandbox lifecycle state machine; the "correctness under failure + backpressure" discipline = what a sandbox orchestrator needs when a guest hangs or floods.

---

## 1. Current-state audit (what is wrong, precisely)

The workspace currently has 6 members: `forking`, `iterative`, `pre_fork`, `server`, `thread_per_request`, `thread_pools`.

### 1.1 Structural problem (highest priority)
`node.rs`, `response.rs`, `router.rs`, `routes.rs` are **byte-identical copies across `forking`, `iterative`, `pre_fork`, `thread_per_request`.** There is no shared crate. A Principal Engineer reads this as "no abstraction boundary exists." This must be collapsed into one `core` crate.

### 1.2 Bugs shared by all four router-based crates
- **HTTP parsing reads only the request line** (`read_line`). Headers and body are ignored. No `Content-Length`, no keep-alive, no method beyond `GET`. Every connection is one-shot HTTP/1.0-style — every request pays a fresh TCP handshake.
- **`println!` on the hot path** (per connection *and* per request). `stdout` is behind a global lock; this is a lock acquisition + syscall per request. It invalidates every benchmark.
- **`send_file` does `File::open` + `read_to_end` per request.** This benchmarks the OS page cache, not the server.
- **No read/write timeouts.** A single idle connection slow-loris's the server.

### 1.3 Per-crate correctness bugs
- **`iterative`:** `router.route_client(client)?` in the accept loop — one client error propagates via `?` and **the entire server process exits.** One malformed request = denial of service.
- **`forking`:** `fork()` per connection with **no `waitpid`** → zombie processes accumulate until `fork()` fails with `EAGAIN` and the server dies. No cap on concurrent children → fork-bomb under load.
- **`pre_fork`:** all 10 children loop on `listener.incoming()` (an infinite iterator); the `break` after the loop is **unreachable**, so children never exit and the parent's `waitpid` loop blocks forever. Shares one listener fd across forks → `accept()` thundering herd. Inferior to `SO_REUSEPORT`.
- **`thread_per_request`:** misnamed — it is pre-fork (10 procs) × thread-per-connection, a confused hybrid. `handles.push()` runs inside an infinite loop so the join loop is **unreachable** → `JoinHandle`s leak forever (unbounded memory). Unbounded `thread::spawn` per connection → thread-bomb; ~8 MiB default stack each → OOM under load.
- **`server`:** a different lineage (the Rust Book ch.20 server). Hardcoded `127.0.0.1:7878`, fixed `[0;1024]` read buffer (truncates large requests), `.unwrap()` everywhere (one bad request panics). The `ThreadPool` in `lib.rs` is **commented out** in `main.rs` — it is dead code; `server` actually runs single-threaded. The one good instinct: `dhat-heap` profiling is wired up — keep that instinct.
- **`thread_pools`:** empty. `fn main() { println!("Hello, world!"); }`.

**Reality check:** the owner believes there are 6 models. There are effectively **4 buggy models + 1 dead-code threadpool + 1 empty crate.** The "thread pool" model does not currently exist as a runnable thing.

---

## 2. Target architecture

Restructure into a clean workspace. **The four copy-pasted crates collapse into modules behind one trait.**

```
rust-tcp-server/
  Cargo.toml                  # workspace
  core/                       # the ONE source of truth — a lib crate
    src/
      http.rs                 # request parser (req line + headers + body), response writer, keep-alive
      router.rs               # the existing trie (Node) — keep, it is fine
      asset.rs                # in-memory static-asset cache (load once at boot; serve from RAM)
      app.rs                  # App { router, assets, metrics } — shared, immutable, Arc-wrapped
      conn.rs                 # per-connection state machine (ReadingRequest -> Writing -> KeepAlive|Close)
      metrics.rs              # latency histogram (hdrhistogram), counters
      server.rs               # the `Server` trait — the swappable interface
  server/                     # one binary: `server --model <name> --port <p>`
    src/main.rs               # CLI; selects and runs a model
    src/models/               # one module per model, each implements `Server`
  bench/
    loadgen/                  # custom open-loop load generator (own crate or module)
    run.sh                    # one command: sweeps every model x concurrency, emits CSVs + plots
    results/                  # COMMITTED: *.csv + *.svg/png plots
  docs/
    ARCHITECTURE.md
    BENCHMARKS.md              # the teardown writeup — the actual artifact
  README.md                   # benchmark teardown, pinned on profile
```

**The `Server` trait is the product. The models are instances.** Sketch:

```rust
// core/src/server.rs
pub trait Server {
    fn name(&self) -> &'static str;
    /// Takes an already-bound listener and the shared app. Runs until shutdown.
    fn serve(&self, listener: std::net::TcpListener, app: std::sync::Arc<App>) -> std::io::Result<()>;
}
```

`App` is immutable and `Arc`-shared: router (trie) + asset cache + metrics handle. The HTTP request/response handling lives **once** in `core` and is reused by all 11 models. A model only owns its **concurrency/I-O strategy** — nothing else.

---

## 3. The models — fix 5, build 6 (11 total)

All 11 implement `Server`, selected via `--model`. Every model must meet the **common bar** in §4 before it is considered done.

### Fix (port into `core`-backed modules; correct the bugs)
1. **`iterative`** — single-thread accept→handle loop. Reference model. Fix: never `?`-propagate a per-client error — log (off hot path) and continue. Add timeouts. Keep-alive.
2. **`forking`** — fork-per-connection. Fix: reap children (`SIGCHLD` handler or `waitpid` with `WNOHANG`, or double-fork). Cap concurrent children; handle `EAGAIN` gracefully.
3. **`preforked`** — N worker processes. Fix the unreachable-`break` / never-exiting bug. **Upgrade from shared-fd thundering herd to `SO_REUSEPORT`** (each child binds its own listener). Clean shutdown via signal.
4. **`thread-per-conn`** — clean thread-per-connection (drop the fork hybrid entirely). Fix unbounded spawn + leaked handles. Either cap with a semaphore, or keep it uncapped *and document the failure mode honestly* as the model's defining weakness.
5. **`thread-pool`** — resurrect and fix `server/lib.rs`'s `ThreadPool`: bounded job queue, **explicit backpressure when the queue is full** (reject or block — decide and document), graceful shutdown.

### Build (the actual systems content)
6. **`poll`** — single-thread `poll(2)` event loop. The O(n)-scan readiness baseline. Exists to make the next model's improvement *measurable*.
7. **`epoll-lt`** — `epoll` level-triggered, single thread, non-blocking sockets, per-connection state machine from `core/conn.rs`.
8. **`epoll-et`** — `epoll` **edge-triggered**. The high-signal variant: must drain reads until `EAGAIN`, handle partial writes, register/deregister `EPOLLOUT` correctly. Getting ET right is the "I understand this API to the floor" signal.
9. **`event-loop`** — a clean single-threaded **reactor** abstraction over epoll-ET: a proper readiness-driven connection state machine with deliberate buffer management. This is the architectural artifact, and the direct ancestor of the microVM I/O multiplexer.
10. **`multireactor`** — one acceptor + **N reactor threads**, each running the epoll-ET event loop, threads **pinned to cores**, `SO_REUSEPORT` per reactor. The production pattern and the direct microVM-control-plane analogue.
11. **`io-uring`** — use the raw **`io-uring` crate** (tokio-rs/io-uring), NOT `tokio-uring` and NOT tokio. **Must be purpose-built**: multishot accept, provided/registered buffer rings, batched submission. A drop-in epoll replacement yields only ~1.06x and would make the benchmark pointless; purpose-built reaches ~2x. Using the raw crate (direct SQ/CQ, opcodes) is itself the signal — it shows you understand the submission/completion model, not just a runtime.

---

## 4. The common bar — every model must pass this

A model is **not done** until all of the following hold:

- **Behind the `Server` trait**, runnable as `server --model <name> --port <p>`.
- **Correct.** Passes a conformance test suite: well-formed requests, malformed requests, slow-loris (partial then idle), pipelined/keep-alive requests, oversized requests. **One bad client must never kill the server.**
- **Leak-free.** No zombies, no unbounded thread/handle growth. Runs 10 minutes under sustained load with **flat RSS** and flat fd count.
- **Zero hot-path logging.** Logging is gated behind a feature flag or routed through a sampling/async logger. No `println!` per request, ever.
- **Timeouts.** Read and write timeouts on every connection. A stalled client is dropped.
- **HTTP/1.1 keep-alive.** Connections are reused; the parser handles `Content-Length` and `Connection:` correctly.
- **Static assets served from the in-memory cache** (`core/asset.rs`), never re-read from disk per request.
- **Measured immediately.** The moment a model builds, it goes through the harness. No model is "done" without numbers.

---

## 5. The benchmark harness

- **Custom open-loop load generator** (`bench/loadgen`). Open-loop (fixed request *rate*), not only closed-loop (fixed concurrency) — a closed-loop benchmark hides tail latency under **coordinated omission** and lies. Record latency into an `hdrhistogram`. Writing this yourself (~200–300 lines) is part of the signal and forces coordinated-omission awareness.
- **Cross-check with `wrk2`** (which corrects for coordinated omission) so the numbers are externally reproducible.
- **Metrics captured per run:** throughput (req/s), p50/p90/p99/p99.9/p99.99 latency, at concurrency **1 / 10 / 100 / 1,000 / 10,000**. Plus CPU utilization, context-switch count, syscall count, and RSS (`perf stat`, `/usr/bin/time -v`).
- **Interior latency distribution** — plot the full histogram, not just percentiles, so the tail *shape* is visible (the David Gross order-book-tail discipline).
- **Top-down microarchitecture profile** (`perf stat` topdown: retiring / bad-speculation / frontend-bound / backend-bound) on at least `epoll-et` and `io-uring`.
- **Reproducible.** `bench/run.sh` runs the full sweep and writes CSVs + plots into `bench/results/` (committed). Document the test machine (CPU, cores, RAM) and **kernel version** (io_uring multishot accept needs ≥5.19; test on 6.1+).

---

## 6. Hard Definition of Done

The repo is world-class-artifact-grade only when **all** of these are true:

1. All 11 models implemented behind one `Server` trait, swappable via `--model`. **Zero copy-pasted logic** — HTTP/router/asset code lives once in `core`.
2. Every model passes the §4 common bar.
3. One reproducible harness; `bench/results/` committed with CSVs and plots.
4. **Performance bars met (or honestly failed with analysis):**
   - `epoll-et` measurably beats `thread-per-conn` at ≥1,000 concurrency (lower p99, higher throughput).
   - `multireactor` scales close to linearly with cores up to physical core count — show the **scaling-factor plot** (sum of N reactors ÷ single reactor).
   - `io-uring` (purpose-built) beats `epoll-et` on the request-response workload — **or**, if it does not, report that honestly with a top-down profile explaining why. *An honest negative result with rigorous analysis is elite signal; a fake win is not.*
   - The server survives **10,000 concurrent keep-alive connections** without falling over. Name **C10K** explicitly.
5. `docs/BENCHMARKS.md` is the teardown: methodology, machine + kernel, results table, interior-distribution plots, scaling plot, and **one paragraph per model explaining the mechanism** — connection-setup cost, the ~1–2µs OS context-switch cost (quote it), syscall batching, why ET must drain, why io_uring eliminates syscalls. Honest about confounds.
6. `README.md` is a benchmark teardown (not a how-to), pinned on the GitHub profile.

---

## 7. Non-negotiable engineering rules (the discipline that signals "elite")

These are distilled from David Gross's low-latency C++ talks and the Jane Street exchange-architecture notes. They are what separate this from a student project.

1. **Measure, never guess.** Every claim in the README has a number behind it. Performance intuition is wrong by default.
2. **Nothing on the hot path that isn't the work.** No `println!`, no `File::open`, no avoidable allocation per request. "Slow everywhere" means it doesn't fit in cache.
3. **Distributions, not averages.** Plot the full interior latency distribution. A fat tail is a bug to be explained, not a footnote.
4. **Open-loop, coordinated-omission-aware load.** A closed-loop benchmark under-reports tail latency. Know why.
5. **Mechanical sympathy.** Know cache sizes and the cost of a context switch (~1–2µs) and a syscall. Pin reactor threads to cores. Be NUMA-aware if multi-socket.
6. **Correctness under failure first.** One malformed request must never kill the server. A slow client must never hold a worker forever. **Backpressure is explicit** — decide and document what happens when the accept backlog / job queue fills.
7. **One abstraction, many implementations.** The `Server` trait is the product. No copy-paste.
8. **Benchmark what you think you're benchmarking.** Cache assets in RAM or you're timing the page cache. Warm up. Randomize where it matters.
9. **Reproducible or it didn't happen.** One command, committed results, documented machine and kernel.
10. **Simple and fast beats clever and fast.** Do not add complexity the benchmark does not justify.

Use the vocabulary of this domain — *mechanical sympathy, edge-triggered, readiness vs completion I/O, coordinated omission, thundering herd, backpressure, interior latency distribution, top-down microarchitecture analysis* — but **only after the technique is actually applied.** Decorative jargon is detected instantly. Earn the term, then use it.

---

## 8. Build order (maps to the 21-day sprint)

This repo is **Week 1 primary** + **Week 2 secondary** of the sprint. Sequence inside the repo:

**Phase 0 — Foundation (Day 1, ~3–4h).** Restructure the workspace. Delete the four copy-pasted crates. Stand up `core` (http, router, asset, app, metrics, the `Server` trait). Port `iterative` as the reference model behind the trait. Get **one model green end-to-end** before anything else.

**Phase 1 — Week 1.** Build the harness skeleton + `loadgen` *early* so every model is measured the moment it compiles. Fix `forking`, `preforked`, `thread-per-conn`, `thread-pool` to the §4 bar. Then build `poll` → `epoll-lt` → `epoll-et` → `event-loop`, in that order (each is the conceptual prerequisite of the next).

**Phase 2 — Week 2 (secondary slice).** Build `multireactor`, then `io-uring`. Run the full benchmark sweep. Write `docs/BENCHMARKS.md` and the `README.md` teardown. Commit `bench/results/`.

---

## 9. Out of scope — do NOT do these

- No TLS/HTTPS. No HTTP/2. No web framework.
- **No async runtime (tokio) for the epoll / event-loop / multireactor models.** Hand-rolling the reactor is the entire point; tokio defeats it. The `io-uring` model uses the raw `io-uring` crate, not `tokio-uring`, for the same reason.
- No new router — the existing trie is fine.
- **No frontends, no styled HTML.** The static assets are benchmark fixtures only.
- Do not gold-plate the HTTP parser. Handle `GET`, headers, keep-alive, `Content-Length`; reject everything else cleanly. A full RFC-compliant parser is out of scope.
- Do not chase an `io_uring` "win." Report what the numbers say.
- Do not exceed the build order. Resist adding models or features not in §3.

---

## 10. First message for the executing chat

Paste this brief, then start with:

> "Execute Phase 0. Restructure the workspace per §2: create the `core` crate with the `Server` trait, http parser, trie router, in-memory asset cache, and metrics. Collapse the four copy-pasted crates. Port `iterative` as the reference model behind the trait and get it green end-to-end with keep-alive, timeouts, and zero hot-path logging. Show me the workspace tree and the `core` public API before writing any other model."
