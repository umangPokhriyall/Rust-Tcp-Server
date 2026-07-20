# Rust-Tcp-Server

Rust-Tcp-Server implements eleven classic and modern TCP server concurrency
models—from a blocking `accept()` loop through `fork()`, thread pools,
`poll`, `epoll`, multi-reactor designs, to a purpose-built `io_uring`
completion engine.

Every implementation implements the same Server trait while sharing the same
frozen sans-IO core::Connection state machine.

## Why this repository?

Networking tutorials usually present one server architecture in isolation:
an epoll server, an io_uring server, or a thread pool.

This project instead implements eleven concurrency models behind one frozen
HTTP protocol core, one benchmark harness, and one open-loop load generator.
Every implementation shares the same parser, router, connection state machine,
assets, and benchmark methodology, so differences in throughput, latency,
resource usage, syscall count, and microarchitectural behavior arise from the
concurrency model itself.

The result is an apples-to-apples study of classic UNIX server architectures
and modern Linux event mechanisms, supported by coordinated-omission-corrected
latency measurements and AMD Zen 4 pipeline-utilization analysis.

## Headline results

**Eight of the eleven implementations sustain a true C10K workload**
(10,000 concurrent keep-alive connections at an offered 50,000 requests/s)
**with zero request errors**, all within a nearly flat **≈10.7 MiB RSS**—
roughly **1.1 KiB of server memory per connection**, essentially an fd and one
slab entry rather than a thread or process. Among them, `multireactor`
achieves the lowest median latency at **70 µs**.

The second result is architectural rather than algorithmic.
`io_uring` executes the same workload with **2.021 syscalls/request**, almost
exactly half of `epoll-et`'s **4.028 syscalls/request**, thanks to multishot
accept and provided buffer rings. The AMD Zen 4 pipeline analysis shows that
this syscall reduction does **not** translate into lower latency on this
workload because execution is frontend-latency-bound rather than
syscall-bound.

## Repository map

| Path                    | Role                                                                                                                                                                   |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `core/`                 | Sans-IO protocol library: HTTP parser, response encoder, trie router, in-memory asset cache, and the `Connection` state machine. Never touches a socket. Frozen.       |
| `server/src/sys/`       | Raw OS I/O: thin `libc` wrappers for sockets, `poll`, `epoll`, CPU affinity, signals, the connection table. Every syscall in the project lives here.                   |
| `server/src/reactor.rs` | The event-loop assembly: an epoll-ET readiness loop over a connection table, reused unchanged by the single-thread event-loop model and by each `multireactor` worker. |
| `server/src/models/`    | The eleven concurrency strategies, one module each, every one implementing `core::Server`.                                                                             |
| `loadgen/`              | Open-loop, coordinated-omission-corrected HTTP load generator emitting HDR-histogram dumps.                                                                            |
| `bench/`                | Reproduction harness (`run.sh`, `c10k.sh`, `scaling.sh`, `profile.sh`, `plot.py`) and all committed results under `bench/results/`.                                    |

## Results

C=100 from the sweep (`bench/results/<model>.csv`, offered 20,000 rps); C=10000 is
the true C10K rung at 10,000 connections / 50,000 rps offered
(`bench/results/c10k_<model>.csv`, `bench/results/c10k_summary.csv`).
Syscalls/req is from `bench/results/profiles/summary.csv`, measured for all eleven
models.

| Model           | C=100 thr (req/s) | C=100 p99 (µs) | C=10000 result                      | C=10000 p50 (µs) | Syscalls/req |
| --------------- | ----------------- | -------------- | ----------------------------------- | ---------------- | ------------ |
| iterative       | 20000             | 87             | saturated, no completion            | —                | 2.044        |
| forking         | 20000             | 96             | 50000 req/s, 0 errors               | 73               | 2.031        |
| preforked       | 20000             | 86             | 17008.8 req/s, 989,735 errors       | 60               | 2.158        |
| thread-per-conn | 20000             | 89             | 50000 req/s, 0 errors               | 71               | 2.049        |
| thread-pool     | 20000             | 86             | 17022.4 req/s, 989,328 errors       | 62               | 2.047        |
| poll            | 20000             | 116            | 50000 req/s, 0 errors (p50 7231 µs) | 7231             | 4.026        |
| epoll-lt        | 20000             | 97             | 50000 req/s, 0 errors               | 195              | 6.028        |
| epoll-et        | 20000             | 94             | 50000 req/s, 0 errors               | 94               | 4.028        |
| event-loop      | 20000             | 94             | 50000 req/s, 0 errors               | 98               | 4.027        |
| multireactor    | 20000             | 91             | 50000 req/s, 0 errors               | 70               | 4.178        |
| io-uring        | 20000             | 96             | 50000 req/s, 0 errors               | 80               | 2.021        |

![Throughput vs concurrency](bench/results/throughput_vs_concurrency.png)

_Throughput vs concurrency across all eleven models. Source: the
`throughput_rps` column of every `bench/results/<model>.csv`._

## Model index

| Model           | Mechanism                                                         | Best-case throughput                                                           | Limitation                                                                                   |
| --------------- | ----------------------------------------------------------------- | ------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------- |
| iterative       | One `accept`→serve→`close` at a time, single thread               | 20000 req/s at C=100 (`iterative.csv`)                                         | Total head-of-line blocking; saturates at C10K (`c10k_summary.csv`)                          |
| forking         | One `fork()` per connection                                       | 50000 req/s at C10K, 0 errors (`c10k_forking.csv`)                             | Commit-limit exhaustion — only on a small host; 384 GiB clears 10k                           |
| preforked       | Fixed pool of worker processes, `SO_REUSEPORT` balanced           | 20000 req/s at C=100 (`preforked.csv`)                                         | Bounded blocking pool sheds 99% of load at C10K (`c10k_preforked.csv`)                       |
| thread-per-conn | One OS thread per connection, uncapped                            | 50000 req/s at C10K, 0 errors, 261 MiB RSS (`c10k_thread-per-conn.csv`)        | Thread-stack commit exhaustion — only on a small host                                        |
| thread-pool     | Bounded worker pool over a shared job queue, backpressure         | 20000 req/s at C=100 (`thread-pool.csv`)                                       | Sheds 99% of load as errors at C10K; cannot _hold_ idle connections (`c10k_thread-pool.csv`) |
| poll            | Single-thread `poll(2)` O(n)-scan readiness loop                  | 50000 req/s at C10K, 0 errors (`c10k_poll.csv`)                                | Per-wakeup rescan: p50=7231 µs, ~77× epoll-et                                                |
| epoll-lt        | Single-thread level-triggered epoll                               | 50000 req/s at C10K, 0 errors (`c10k_epoll-lt.csv`)                            | LT re-notification: 6.028 syscalls/req, p50=195 µs at C10K                                   |
| epoll-et        | Single-thread edge-triggered epoll, drain to `EAGAIN`             | 50000 req/s at C10K, 0 errors, p50=94 µs (`c10k_epoll-et.csv`)                 | Only the open-loop tail under backlog                                                        |
| event-loop      | epoll-ET behind a reusable reactor abstraction                    | 50000 req/s at C10K, 0 errors (`c10k_event-loop.csv`)                          | Tail under backlog; zero-cost over hand-rolled epoll-ET on this host                         |
| multireactor    | Shared-nothing pinned reactors, `SO_REUSEPORT`, no acceptor       | 50000 req/s at C10K, 0 errors, best median p50=70 µs (`c10k_multireactor.csv`) | Load imbalance under skewed lifetimes; no work-stealing                                      |
| io-uring        | Single-ring completion engine: multishot accept, provided buffers | 50000 req/s at C10K, 0 errors (`c10k_io-uring.csv`); 2.021 syscalls/req        | Single-thread completion path; syscall win doesn't beat epoll-ET's pipeline                  |

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

All numbers above were produced on a single Latitude.sh `m4.metal.large`: AMD
EPYC 9254 (24 physical cores / 48 threads, 4 CCDs with private per-CCD L3), 384
GiB, `Linux 6.8.0-124-generic` (Ubuntu 24.04 LTS), `amd-pstate` performance
governor, loopback only (`bench/results/rig.txt`). The box provisioned as NPS1
(one NUMA node), so server and loadgen are isolated by core-pinning to disjoint
CCDs (server CPUs 0–11, loadgen 12–23) — disjoint cores and disjoint private L3,
memory interleaved. The full environment, methodology, and threats to validity are in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md) §2–§3.

```
cargo build --release
SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/run.sh    # 11-model sweep, C=1/10/100/1000/10000
SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/c10k.sh   # true 10,000-connection C10K
SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/scaling.sh  # multireactor scaling study
PERF_METRIC_GROUP='frontend_bound_group,backend_bound_group,retiring_group,bad_speculation_group' \
  SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/profile.sh  # syscalls/req, ctx/req, AMD Zen4 pipeline
python3 bench/plot.py    # regenerate every plot in bench/results/
```

=

## Documentation

- [docs/BENCHMARKS.md](docs/BENCHMARKS.md) — the comprehensive benchmark study: headline
  table, plots, AMD Zen4 pipeline-utilization analysis, per-model mechanism, the
  `io_uring` verdict, C10K resource curves, surprises and corrections, full data
  provenance.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — the four-layer design, the
  sans-IO contract, the evidence that one frozen core served all eleven models,
  and the key design decisions with their rejected alternatives.
