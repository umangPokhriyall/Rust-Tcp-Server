# Rust-Tcp-Server

Eleven TCP server concurrency models — from an `accept()`-in-a-loop to a
purpose-built `io_uring` completion engine — implemented behind one `Server`
trait and one frozen sans-IO `core::Connection` state machine, then benchmarked
honestly with an open-loop, coordinated-omission-corrected load generator.

## Headline result

All five event-loop models and `multireactor` carry **8000 concurrent
keep-alive connections at the full offered 50000 req/s with zero errors**, in a
flat **≈ 10 MiB** resident set — roughly 1.3 KiB of server memory per
connection, an fd and a slab entry rather than a thread or a process
(`bench/results/c10k_summary.csv`). `multireactor` does it at the lowest median
of any surviving model, **p50 = 86 µs** at that rung
(`bench/results/c10k_multireactor.csv`). The thread- and process-per-connection
models never reach the rung: `forking` and `thread-per-conn` panic on
`EAGAIN`/`WouldBlock` allocating the per-connection thread
(`bench/results/c10k_server_thread-per-conn.log`).

The second result is syscalls. On the apples-to-apples single-thread axis,
purpose-built single-ring `io_uring` runs the workload at **2.015 syscalls/req
versus epoll-et's 4.024** — a 2.00× reduction
(`bench/results/profiles/summary.csv`) — because multishot accept and provided
buffer rings remove the per-accept and per-read syscalls and the edge-trigger
drain. It does not win absolute throughput: one ring on one thread sheds load
above C≈1000, while `multireactor` uses all N cores (full analysis in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md) §8).

## Repository map

| Path | Role |
|---|---|
| `core/` | Sans-IO protocol library: HTTP parser, response encoder, trie router, in-memory asset cache, and the `Connection` state machine. Never touches a socket. Frozen since Phase 0. |
| `server/src/sys/` | Raw OS I/O: thin `libc` wrappers for sockets, `poll`, `epoll`, CPU affinity, signals, the connection table. Every syscall in the project lives here. |
| `server/src/reactor.rs` | The event-loop assembly: an epoll-ET readiness loop over a connection table, reused unchanged by the single-thread event-loop model and by each `multireactor` worker. |
| `server/src/models/` | The eleven concurrency strategies, one module each, every one implementing `core::Server`. |
| `loadgen/` | Open-loop, coordinated-omission-corrected HTTP load generator emitting HDR-histogram dumps. |
| `bench/` | Reproduction harness (`run.sh`, `c10k.sh`, `scaling.sh`, `profile.sh`, `plot.py`) and all committed results under `bench/results/`. |

## Results

C=100 from the sweep (`bench/results/<model>.csv`, offered 20000 rps); C=8000 is
the C10K rung at 8000 connections / 50000 rps offered
(`bench/results/c10k_<model>.csv`, `bench/results/c10k_summary.csv`).
Syscalls/req is from `bench/results/profiles/summary.csv` and was measured only
for the three signal models; `—` marks unprofiled.

| Model | C=100 thr (req/s) | C=100 p99 (µs) | C=8000 result | C=8000 p50 (µs) | Syscalls/req |
|---|---|---|---|---|---|
| iterative | 20000 | 18607 | saturated, no completion | — | — |
| forking | 20000 | 15055 | server panic on spawn (EAGAIN) | — | — |
| preforked | 20000 | 2061 | saturated, no completion | — | — |
| thread-per-conn | 20000 | 5695 | server panic on spawn (EAGAIN) | — | — |
| thread-pool | 20000 | 3091 | 16569.6 req/s, 1,002,913 errors | 75 | — |
| poll | 20000 | 11495 | 50000 req/s, 0 errors | 9975 | — |
| epoll-lt | 20000 | 22639 | 50000 req/s, 0 errors | 457215 | — |
| epoll-et | 20000 | 23055 | 50000 req/s, 0 errors | 157 | 4.024 |
| event-loop | 20000 | 71679 | 50000 req/s, 0 errors | 63519 | — |
| multireactor | 20000 | 24399 | 50000 req/s, 0 errors | 86 | 4.059 |
| io-uring | 20000 | 39327 | 18402.4 req/s, 947,927 errors | 83 | 2.015 |

![Throughput vs concurrency](bench/results/throughput_vs_concurrency.png)

*Throughput vs concurrency across all eleven models. Source: the
`throughput_rps` column of every `bench/results/<model>.csv`.*

## Model index

| Model | Mechanism | Best-case throughput | Where it breaks |
|---|---|---|---|
| iterative | One `accept`→serve→`close` at a time, single thread | 20000 req/s at C=100 (`iterative.csv`) | Total head-of-line blocking; no second connection |
| forking | One `fork()` per connection | 20000 req/s at C=100 (`forking.csv`) | `EAGAIN` on clone; commit-limit exhaustion (`c10k_server_forking.log`) |
| preforked | Fixed pool of 8 worker processes, `SO_REUSEPORT` balanced | 20000 req/s at C=100, tightest process-group tail p99=2061 µs (`preforked.csv`) | 8 blocking workers saturate; no per-connection multiplexing |
| thread-per-conn | One OS thread per connection, uncapped | 20000 req/s at C=100 (`thread-per-conn.csv`) | Thread-stack commit exhaustion ≈ 4–5k conns; panic at 4608 fds (`c10k_thread-per-conn.log`) |
| thread-pool | Bounded worker pool over a shared job queue, backpressure | 20000 req/s at C=100 (`thread-pool.csv`) | Sheds 67% of load as errors at C=8000; cannot *hold* idle connections (`c10k_thread-pool.csv`) |
| poll | Single-thread `poll(2)` O(n)-scan readiness loop | 50000 req/s at C=8000, 0 errors (`c10k_poll.csv`) | Per-wakeup rescan: p50=9975 µs, 2 orders over epoll-et |
| epoll-lt | Single-thread level-triggered epoll | 50000 req/s at C=8000, 0 errors (`c10k_epoll-lt.csv`) | LT re-notification collapse: p50=457215 µs (457 ms) at C=8000 |
| epoll-et | Single-thread edge-triggered epoll, drain to `EAGAIN` | 50000 req/s at C=8000, 0 errors, p50=157 µs (`c10k_epoll-et.csv`) | Only the open-loop tail under backlog |
| event-loop | epoll-ET behind a reusable reactor abstraction | 50000 req/s at C=8000, 0 errors (`c10k_event-loop.csv`) | Tail under backlog; latency profile diverges from hand-rolled epoll-ET |
| multireactor | Shared-nothing pinned reactors, `SO_REUSEPORT`, no acceptor | 50000 req/s at C=8000, 0 errors, best median p50=86 µs (`c10k_multireactor.csv`) | Load imbalance under skewed lifetimes; no work-stealing |
| io-uring | Single-ring completion engine: multishot accept, provided buffers | 20000 req/s at C=100, lowest p50=77 µs (`io-uring.csv`); 2.015 syscalls/req | Sheds load above C≈1000: one ring on one thread |

## Why this exists

The shapes measured here are the control-plane primitives of an AI-agent
sandbox host: `multireactor` is the acceptor-free, `SO_REUSEPORT`,
one-reactor-per-core API control plane; the epoll-ET / `io_uring` loop is the
host↔guest I/O multiplexer; the frozen `core::Connection` is the guest lifecycle
state machine that survives an I/O-substrate change unmodified. The benchmark is
a rehearsal of that control plane on a workload where every claim is measurable.

## Build & run

```
cargo build --release
./target/release/server --model <name> --port <p> --assets-dir server/assets
```

`<name>` is one of: `iterative`, `forking`, `preforked`, `thread-per-conn`,
`thread-pool`, `poll`, `epoll-lt`, `epoll-et`, `event-loop`, `multireactor`,
`io-uring`. `multireactor` accepts `--workers N` (default `num_cores()`).
`io-uring` requires kernel ≥ 5.19; it prints the kernel version and exits
non-zero on an older host.

## Reproduce the benchmarks

```
cargo build --release
bash bench/run.sh        # the 11-model sweep, concurrency 1/10/100/1000/10000
bash bench/c10k.sh       # the C10K resource curves
bash bench/scaling.sh    # the multireactor scaling study
bash bench/profile.sh    # syscalls/req + ctx-switches/req (perf fallback documented)
python3 bench/plot.py    # regenerate every plot in bench/results/
```

All numbers above were produced on an 11th Gen Intel Core i5-1135G7 (4 physical
/ 8 logical cores), 8 GiB RAM, `Linux 7.0.0-15-generic`, loopback only; the full
environment, methodology, and threats to validity are recorded in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md) §2–§3. The benchmark ran C10K at 8000
connections, the highest concurrency this host's commit limit allows
(`bench/results/c10k_README.md`).

## Documentation

- [docs/BENCHMARKS.md](docs/BENCHMARKS.md) — the measured teardown: headline
  table, plots, per-model mechanism, the `io_uring` verdict, C10K resource
  curves, surprises and corrections, full data provenance.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — the four-layer design, the
  sans-IO contract, the evidence that one frozen core served all eleven models,
  and the key design decisions with their rejected alternatives.
