# Rust-Tcp-Server

Eleven TCP server concurrency models — from an `accept()`-in-a-loop to a
purpose-built `io_uring` completion engine — implemented behind one `Server`
trait and one frozen sans-IO `core::Connection` state machine, then benchmarked
honestly with an open-loop, coordinated-omission-corrected load generator on
AMD EPYC bare metal.

## Headline result

**Eight of eleven models carry a true 10,000 concurrent keep-alive connections at
the full offered 50,000 req/s with zero errors**, in a flat **≈ 10.7 MiB**
resident set — roughly 1.1 KiB of server memory per connection, an fd and a slab
entry rather than a thread or a process (`bench/results/c10k_summary.csv`).
`multireactor` does it at the lowest median of any model, **p50 = 70 µs** at that
rung (`bench/results/c10k_multireactor.csv`). The three that fall short are the
single-thread `iterative` (saturates) and the two bounded-pool models `preforked`
and `thread-pool`, which shed ~99% of load as errors.

The second result is syscalls. On the apples-to-apples single-thread axis,
purpose-built single-ring `io_uring` runs the workload at **2.021 syscalls/req
versus epoll-et's 4.028** — a 1.99× reduction (`bench/results/profiles/summary.csv`)
— because multishot accept and provided buffer rings remove the per-accept and
per-read syscalls and the edge-trigger drain. On this host it sustains C10K
without shedding; the AMD Zen4 pipeline data shows that halved syscall count does
not become more useful work, because the workload is frontend-latency-bound, not
syscall-bound (full analysis in [docs/BENCHMARKS.md](docs/BENCHMARKS.md) §8, §5A).

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

C=100 from the sweep (`bench/results/<model>.csv`, offered 20,000 rps); C=10000 is
the true C10K rung at 10,000 connections / 50,000 rps offered
(`bench/results/c10k_<model>.csv`, `bench/results/c10k_summary.csv`).
Syscalls/req is from `bench/results/profiles/summary.csv`, measured for all eleven
models.

| Model | C=100 thr (req/s) | C=100 p99 (µs) | C=10000 result | C=10000 p50 (µs) | Syscalls/req |
|---|---|---|---|---|---|
| iterative | 20000 | 87 | saturated, no completion | — | 2.044 |
| forking | 20000 | 96 | 50000 req/s, 0 errors | 73 | 2.031 |
| preforked | 20000 | 86 | 17008.8 req/s, 989,735 errors | 60 | 2.158 |
| thread-per-conn | 20000 | 89 | 50000 req/s, 0 errors | 71 | 2.049 |
| thread-pool | 20000 | 86 | 17022.4 req/s, 989,328 errors | 62 | 2.047 |
| poll | 20000 | 116 | 50000 req/s, 0 errors (p50 7231 µs) | 7231 | 4.026 |
| epoll-lt | 20000 | 97 | 50000 req/s, 0 errors | 195 | 6.028 |
| epoll-et | 20000 | 94 | 50000 req/s, 0 errors | 94 | 4.028 |
| event-loop | 20000 | 94 | 50000 req/s, 0 errors | 98 | 4.027 |
| multireactor | 20000 | 91 | 50000 req/s, 0 errors | 70 | 4.178 |
| io-uring | 20000 | 96 | 50000 req/s, 0 errors | 80 | 2.021 |

![Throughput vs concurrency](bench/results/throughput_vs_concurrency.png)

*Throughput vs concurrency across all eleven models. Source: the
`throughput_rps` column of every `bench/results/<model>.csv`.*

## Model index

| Model | Mechanism | Best-case throughput | Where it breaks |
|---|---|---|---|
| iterative | One `accept`→serve→`close` at a time, single thread | 20000 req/s at C=100 (`iterative.csv`) | Total head-of-line blocking; saturates at C10K (`c10k_summary.csv`) |
| forking | One `fork()` per connection | 50000 req/s at C10K, 0 errors (`c10k_forking.csv`) | Commit-limit exhaustion — only on a small host; 384 GiB clears 10k |
| preforked | Fixed pool of worker processes, `SO_REUSEPORT` balanced | 20000 req/s at C=100 (`preforked.csv`) | Bounded blocking pool sheds 99% of load at C10K (`c10k_preforked.csv`) |
| thread-per-conn | One OS thread per connection, uncapped | 50000 req/s at C10K, 0 errors, 261 MiB RSS (`c10k_thread-per-conn.csv`) | Thread-stack commit exhaustion — only on a small host |
| thread-pool | Bounded worker pool over a shared job queue, backpressure | 20000 req/s at C=100 (`thread-pool.csv`) | Sheds 99% of load as errors at C10K; cannot *hold* idle connections (`c10k_thread-pool.csv`) |
| poll | Single-thread `poll(2)` O(n)-scan readiness loop | 50000 req/s at C10K, 0 errors (`c10k_poll.csv`) | Per-wakeup rescan: p50=7231 µs, ~77× epoll-et |
| epoll-lt | Single-thread level-triggered epoll | 50000 req/s at C10K, 0 errors (`c10k_epoll-lt.csv`) | LT re-notification: 6.028 syscalls/req, p50=195 µs at C10K |
| epoll-et | Single-thread edge-triggered epoll, drain to `EAGAIN` | 50000 req/s at C10K, 0 errors, p50=94 µs (`c10k_epoll-et.csv`) | Only the open-loop tail under backlog |
| event-loop | epoll-ET behind a reusable reactor abstraction | 50000 req/s at C10K, 0 errors (`c10k_event-loop.csv`) | Tail under backlog; zero-cost over hand-rolled epoll-ET on this host |
| multireactor | Shared-nothing pinned reactors, `SO_REUSEPORT`, no acceptor | 50000 req/s at C10K, 0 errors, best median p50=70 µs (`c10k_multireactor.csv`) | Load imbalance under skewed lifetimes; no work-stealing |
| io-uring | Single-ring completion engine: multishot accept, provided buffers | 50000 req/s at C10K, 0 errors (`c10k_io-uring.csv`); 2.021 syscalls/req | Single-thread completion path; syscall win doesn't beat epoll-ET's pipeline |

## Why this exists

The shapes measured here are the control-plane primitives of an AI-agent sandbox
host: `multireactor` is the acceptor-free, `SO_REUSEPORT`, one-reactor-per-core
API control plane; the epoll-ET / `io_uring` loop is the host↔guest I/O
multiplexer; the frozen `core::Connection` is the guest lifecycle state machine
that survives an I/O-substrate change unmodified. The benchmark is a rehearsal of
that control plane on a workload where every claim is measurable.

## Build & run

```
cargo build --release
./target/release/server --model <name> --port <p> --assets-dir server/assets
```

`<name>` is one of: `iterative`, `forking`, `preforked`, `thread-per-conn`,
`thread-pool`, `poll`, `epoll-lt`, `epoll-et`, `event-loop`, `multireactor`,
`io-uring`. `multireactor` accepts `--workers N` (default `num_cores()`);
event-loop models accept `--max-connections N`. `io-uring` requires kernel ≥
5.19; it prints the kernel version and exits non-zero on an older host.

## Reproduce the benchmarks

```
cargo build --release
SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/run.sh    # 11-model sweep, C=1/10/100/1000/10000
SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/c10k.sh   # true 10,000-connection C10K
SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/scaling.sh  # multireactor scaling study
PERF_METRIC_GROUP='frontend_bound_group,backend_bound_group,retiring_group,bad_speculation_group' \
  SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/profile.sh  # syscalls/req, ctx/req, AMD Zen4 pipeline
python3 bench/plot.py    # regenerate every plot in bench/results/
```

All numbers above were produced on a single Latitude.sh `m4.metal.large`: AMD
EPYC 9254 (24 physical cores / 48 threads, 4 CCDs with private per-CCD L3), 384
GiB, `Linux 6.8.0-124-generic` (Ubuntu 24.04 LTS), `amd-pstate` performance
governor, loopback only (`bench/results/rig.txt`). The box provisioned as NPS1
(one NUMA node), so server and loadgen are isolated by core-pinning to disjoint
CCDs (server CPUs 0–11, loadgen 12–23) — disjoint cores and disjoint private L3,
memory interleaved. **Anyone can rent this exact SKU by the hour and re-run.** The
full environment, methodology, and threats to validity are in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md) §2–§3.

## Documentation

- [docs/BENCHMARKS.md](docs/BENCHMARKS.md) — the measured teardown: headline
  table, plots, AMD Zen4 pipeline-utilization analysis, per-model mechanism, the
  `io_uring` verdict, C10K resource curves, surprises and corrections, full data
  provenance.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — the four-layer design, the
  sans-IO contract, the evidence that one frozen core served all eleven models,
  and the key design decisions with their rejected alternatives.
