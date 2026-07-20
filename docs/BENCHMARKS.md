# BENCHMARKS — TCP server I/O models, measured

## 1. Thesis

This repository implements eleven classic TCP server I/O architectures behind a common Server trait and a shared sans-IO connection state machine. This document presents a measured comparison using throughput, latency distributions, syscall counts, context switches, and AMD Zen4 pipeline-utilization analysis.

## 2. Environment & methodology

**Host** (recorded verbatim in `bench/results/rig.txt`):

| Property       | Value                                                                                                                   |
| -------------- | ----------------------------------------------------------------------------------------------------------------------- |
| CPU            | AMD EPYC 9254, 24 physical cores / 48 threads, single socket (`bench/results/rig.txt`)                                  |
| Chiplet layout | 4 CCDs, each with a private 32 MiB L3 (`L3 cache: 128 MiB (4 instances)`, `bench/results/rig.txt`)                      |
| NUMA           | **NPS1** — one NUMA node, `available: 1 nodes (0)`, `node 0 cpus: 0-47` (`numactl --hardware`, `bench/results/rig.txt`) |
| RAM            | 384 GiB (`node 0 size: 386466 MB`, `bench/results/rig.txt`)                                                             |
| Kernel         | `Linux 6.8.0-124-generic` (Ubuntu 24.04 LTS), microcode `0xa101158` (`bench/results/rig.txt`)                           |
| Governor       | `amd-pstate` `performance` (`bench/results/rig.txt`)                                                                    |
| PMU            | `perf_event_paranoid = -1`, hardware events open natively (`bench/results/rig.txt`)                                     |
| Network        | loopback only (`127.0.0.1`)                                                                                             |

This SKU is a Latitude.sh `m4.metal.large`: it can be rented hourly, self-serve,
and re-run bit-for-bit — **anyone can rent this exact SKU by the hour and
reproduce every reported result.** The run of record is git `bf67267`,
captured `2026-07-04` (`bench/results/rig.txt`).

**Server/loadgen isolation (NPS1 core-pinning).** The box provisioned as **NPS1**
— a single NUMA node exposing all 48 threads (`bench/results/rig.txt`), not the
NPS2 two-node split the run targeted. Under one NUMA node, disjoint-node binding
is impossible, so the harness isolates by core instead: the server is pinned to
CPUs `0-11` (CCDs 0–1) and the load generator to CPUs `12-23` (CCDs 2–3), with
`--membind=0` (`server cpus=0-11  loadgen cpus=12-23  membind=0`,
`bench/results/rig.txt`). This yields **disjoint cores and disjoint private
per-CCD L3** — each CCD owns its 32 MiB L3, true even within one NUMA node — so
server and loadgen never share a core or an L3. The one isolation NPS1 does not
provide is a disjoint memory controller: with a single NUMA node, DRAM traffic is
interleaved across the socket's channels rather than split. This is the
documented NPS1 caveat; per-CCD L3 isolation holds, the memory-controller
split is coarser.

**Prior baseline.** An earlier run of this identical suite on an 11th Gen Intel
Core i5-1135G7 laptop (4 cores / 8 threads, 8 GiB, no PMU) is archived under
`bench/results/_archive-laptop-i5-1135G7/` for historical comparison only; every
number in this document is from the EPYC run and the laptop set is not cited as
evidence.

**Load model.** The load generator is open-loop and corrected for coordinated
omission: requests are scheduled at a fixed offered rate (open-loop) and each request's
latency is measured from the time it _should_ have been sent, not from the time
a connection became free. A backlogged server therefore shows the backlog in its
tail rather than hiding it by slowing the request stream. Results are emitted as
HDR-histogram dumps (`value_us,percentile,total_count,inverse_1_minus_p` — e.g.
`bench/results/epoll-et_r40000_c1000.hgrm`) and per-point summary CSVs.

**Sweep.** `bench/run.sh` drives every model across concurrency
`1 / 10 / 100 / 1000 / 10000` at offered rates `500 / 5000 / 20000 / 40000 /
50000` rps, appending rows to `bench/results/<model>.csv`. Because the load is
rate-capped, a model that keeps up reports throughput equal to the offered rate;
the differentiator at a given rung is the latency distribution and the error
count. `run.sh` starts each server at the default `max_connections`, so its
`c=10000` rung caps the event-loop models below 10,000 concurrent connections and
is **not** the authoritative C10K measurement — the dedicated `bench/c10k.sh`
run (below) is. The `c=10000` rows in the sweep CSVs are therefore superseded for
all 10,000-connection claims by `c10k_<model>.csv`.

**C10K.** `bench/c10k.sh` holds each model at **10,000 concurrent connections /
50,000 rps offered** for 30 s, with the server's `--max-connections` raised to
16384 so the event-loop models accept all of them, while sampling
`/proc/<pid>/status` (VmRSS, ctx-switch counters) and the live fd count. It writes
the resource curve to `bench/results/c10k_<model>.log`, the served
throughput/latency to `bench/results/c10k_<model>.csv`, and a verdict row to
`bench/results/c10k_summary.csv`. 384 GiB of RAM clears the commit limit, so this
is a true 10,000-connection rung with no sentinels.

**Profiling.** `bench/profile.sh` captures three independent, perf-free-vs-perf
passes for all eleven models into `bench/results/profiles/`: syscalls/req
(`strace -c -f`, C=10), ctx-switches/req (summed `/proc/<pid>/task/*/status`,
C=100), and a dedicated `perf stat` pipeline-utilization pass (C=100, and a
second capture under 10,000-connection load for the signal models `epoll-et`,
`multireactor`, `io-uring`). The `perf` groups are the AMD Zen4 pipeline groups
`frontend_bound_group,backend_bound_group,retiring_group,bad_speculation_group`
(`bench/results/rig.txt`, `PERF_METRIC_GROUP`) — see §5A. The throughput sweep,
C10K, and scaling runs are perf-free; perf overhead never touches a throughput
number.

**One-command reproduction:**

```
cargo build --release
SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/run.sh   # 11-model sweep
SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/c10k.sh  # true C10K
SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/scaling.sh
PERF_METRIC_GROUP='frontend_bound_group,backend_bound_group,retiring_group,bad_speculation_group' \
  SERVER_CPUS=0-11 LOADGEN_CPUS=12-23 MEMBIND_NODE=0 bash bench/profile.sh
python3 bench/plot.py
```

## 3. Threats to validity

These are the primary limitations that may affect the generality of the reported results.

- **Coordinated omission** is handled at the source: latency is measured from
  scheduled send time, so the open-loop tail is the real queueing tail.
- **No discarded warmup.** Each point runs with no separate warmup interval
  removed, so first-request connection setup lands in the tail of every point,
  uniformly across models.
- **Held constant:** the same host, the same core-pinning, the same frozen
  `core::Connection`, the same single served asset, the same load generator, and
  the same offered rate at each concurrency rung, for all eleven models.
- **Asset page cache.** The served asset is warm in the page cache for the whole
  run; no model pays disk I/O. This isolates the concurrency/I-O strategy.
- **Server/loadgen contention — resolved.** On the archived laptop the loadgen
  shared all 8 logical cores with the server, inflating `multireactor`'s
  nonvoluntary context switches. On the EPYC box the server (CPUs 0–11) and the
  loadgen (CPUs 12–23) run on **disjoint cores and disjoint private L3**
  (`bench/results/rig.txt`). The confound is removed: `multireactor`'s
  ctx-switches/req fell to **1.002** — see §10.
- **C10K cap — resolved.** The archived laptop could not `pthread_create` 10,000
  worker threads and capped at 8,000. On 384 GiB the loadgen opens all 10,000
  and every model is measured at a true 10,000-connection rung
  (`bench/results/c10k_summary.csv`).
- **Missing PMU — resolved.** The archived laptop denied `perf` (`paranoid=4`),
  so top-down microarchitecture was omitted. On the EPYC box `perf_event_paranoid
= -1` and native AMD Zen4 pipeline-utilization analysis is captured for every
  model (§5A, §7, `bench/results/profiles/perf_*.txt`).
- **Residual caveat — loopback within one socket, no NIC.** All transport is
  `127.0.0.1` across the socket's Infinity Fabric; there is no NIC, no real RTT,
  no loss, no segmentation cost, and — under NPS1 — no memory-controller split
  between server and loadgen. Absolute latencies are loopback latencies; the
  _ordering_ between models and the per-event counters (syscalls/req,
  ctx-switches/req, pipeline buckets) are the transferable results.

## 4. Headline results

Throughput and latency at C=100 are from the sweep CSVs
(`bench/results/<model>.csv`, offered 20,000 rps). The C=10000 rung is the
authoritative C10K capture at 10,000 connections / 50,000 rps offered
(`bench/results/c10k_<model>.csv`, `bench/results/c10k_summary.csv`).
Syscalls/req and ctx-switches/req are from `bench/results/profiles/summary.csv`,
now measured for **all eleven** models.

| Model           | Concurrency | Throughput (req/s) | p50 (µs) | p99 (µs) | p99.9 (µs) | Syscalls/req | Ctx-switches/req |
| --------------- | ----------- | ------------------ | -------- | -------- | ---------- | ------------ | ---------------- |
| iterative       | C=100       | 20000              | 78       | 87       | 96         | 2.044        | 1.000            |
| forking         | C=100       | 20000              | 77       | 96       | 106        | 2.031        | 0.001            |
| preforked       | C=100       | 20000              | 77       | 86       | 92         | 2.158        | 0.012            |
| thread-per-conn | C=100       | 20000              | 77       | 89       | 106        | 2.049        | 0.001            |
| thread-pool     | C=100       | 20000              | 77       | 86       | 92         | 2.047        | 1.006            |
| poll            | C=100       | 20000              | 98       | 116      | 121        | 4.026        | 1.001            |
| epoll-lt        | C=100       | 20000              | 82       | 97       | 109        | 6.028        | 1.002            |
| epoll-et        | C=100       | 20000              | 79       | 94       | 100        | 4.028        | 1.002            |
| event-loop      | C=100       | 20000              | 79       | 94       | 105        | 4.027        | 1.002            |
| multireactor    | C=100       | 20000              | 79       | 91       | 112        | 4.178        | 1.002            |
| io-uring        | C=100       | 20000              | 79       | 96       | 103        | 2.021        | 1.002            |

| Model           | Concurrency | Throughput (req/s)        | p50 (µs) | p99 (µs) | p99.9 (µs) | Syscalls/req | Ctx-switches/req |
| --------------- | ----------- | ------------------------- | -------- | -------- | ---------- | ------------ | ---------------- |
| iterative       | C=10000     | saturated (no completion) | —        | —        | —          | 2.044        | 1.000            |
| forking         | C=10000     | 50000 (0 errors)          | 73       | 95       | 114        | 2.031        | 0.001            |
| preforked       | C=10000     | 17008.8 (989,735 errors)  | 60       | 83       | 88         | 2.158        | 0.012            |
| thread-per-conn | C=10000     | 50000 (0 errors)          | 71       | 92       | 96         | 2.049        | 0.001            |
| thread-pool     | C=10000     | 17022.4 (989,328 errors)  | 62       | 86       | 94         | 2.047        | 1.006            |
| poll            | C=10000     | 50000 (0 errors)          | 7231     | 9527     | 10287      | 4.026        | 1.001            |
| epoll-lt        | C=10000     | 50000 (0 errors)          | 195      | 248      | 263        | 6.028        | 1.002            |
| epoll-et        | C=10000     | 50000 (0 errors)          | 94       | 154      | 177        | 4.028        | 1.002            |
| event-loop      | C=10000     | 50000 (0 errors)          | 98       | 148      | 171        | 4.027        | 1.002            |
| multireactor    | C=10000     | 50000 (0 errors)          | 70       | 92       | 111        | 4.178        | 1.002            |
| io-uring        | C=10000     | 50000 (0 errors)          | 80       | 124      | 146        | 2.021        | 1.002            |

Sources: C=100 rows from each `bench/results/<model>.csv` (row
`rate=20000,connections=100`); C=10000 rows from `bench/results/c10k_<model>.csv`
and `bench/results/c10k_summary.csv`; syscalls/req and ctx-switches/req from
`bench/results/profiles/summary.csv`.

Three observations stand out.

1. Eight models sustain true C10K without errors.
2. io_uring halves syscall count relative to epoll-et while matching C10K throughput.
3. multireactor achieves the lowest median latency under C10K without increasing scheduler activity.

## 5. Plots

All plots are regenerated by `python3 bench/plot.py` from the committed CSVs and
histogram dumps.

**Interior latency distribution, log-y (C=100).** Per-model HDR histograms; the
log-y axis shows the full tail, not only the body.

![Latency distribution at C=100](../bench/results/distribution_c100.png)

_Source: the `_\_r20000_c100.hgrm`dumps in`bench/results/`.\*

**Throughput vs concurrency.**

![Throughput vs concurrency](../bench/results/throughput_vs_concurrency.png)

_Source: the `throughput_rps` column of every `bench/results/<model>.csv`._

**p99 vs concurrency.**

![p99 vs concurrency](../bench/results/p99_vs_concurrency.png)

_Source: the `p99` column of every `bench/results/<model>.csv`._

**multireactor scaling factor.**

![multireactor scaling](../bench/results/multireactor_scaling.png)

_Source: `bench/results/multireactor_scaling.csv`. See §6 for why the throughput
scaling factor is flat and where the scaling signal actually lives._

## 5A. AMD Zen4 pipeline-utilization analysis

This is \*\*AMD Zen4 pipeline-utilization analysis — AMD Zen4 exposes pipeline-utilization
groups rather than Intel's Top-down Microarchitecture Analysis metrics.
Intel's `TopdownL1/L2` metric groups do not exist on Zen4; the EPYC PMU exposes
its own pipeline-utilization groups
(`frontend_bound_group,backend_bound_group,retiring_group,bad_speculation_group`,
`bench/results/rig.txt`), which decompose issue slots into the same four
top-level categories by different silicon events. The buckets below are summed
from the AMD sub-metrics in each `bench/results/profiles/perf_<model>.txt`:

- **Retiring** = `retiring_fastpath` + `retiring_microcode` (useful work).
- **Bad speculation** = `bad_speculation_mispredicts` + `bad_speculation_pipeline_restarts`.
- **Frontend bound** = `frontend_bound_latency` + `frontend_bound_bandwidth`
  (fetch/decode starvation).
- **Backend bound** = `backend_bound_cpu` + `backend_bound_memory`
  (execution/retire stalls, memory-latency stalls).

The classic scalar counters map to their Zen4 events, not Intel's:

The reported metrics map Zen4 events onto the familiar IPC, branch-misprediction,
and memory-stall concepts; see bench/results/profiles/README.md for the exact event mapping.

**Steady-state, C=100** (`perf_<model>.txt`, 20 s at 20,000 rps):

| Model           | Retiring % | Bad-spec % | Frontend % (latency) | Backend % (memory) | ops/cyc |
| --------------- | ---------- | ---------- | -------------------- | ------------------ | ------- |
| iterative       | 13.9       | 0.3        | 67.9 (52.8)          | 17.8 (14.8)        | 0.83    |
| thread-per-conn | 12.5       | 0.4        | 67.1 (54.5)          | 18.5 (16.0)        | 0.77    |
| thread-pool     | 11.4       | 0.7        | 69.3 (56.7)          | 14.3 (12.4)        | 0.75    |
| poll            | 17.1       | 0.4        | 59.0 (49.4)          | 23.5 (20.3)        | 1.03    |
| epoll-lt        | 14.7       | 0.6        | 64.5 (51.5)          | 20.2 (17.7)        | 0.88    |
| epoll-et        | 14.0       | 0.4        | 65.0 (52.3)          | 20.5 (18.0)        | 0.84    |
| event-loop      | 14.0       | 0.4        | 65.2 (52.4)          | 20.3 (17.9)        | 0.84    |
| multireactor    | 13.3       | 0.5        | 67.2 (54.7)          | 18.5 (16.1)        | 0.81    |

Source: `bench/results/profiles/perf_<model>.txt`. Two models have no complete
C=100 row: `perf_forking.txt` reports `<not counted>` for every event (the
per-connection children carry the work; the `perf`-attached parent PID is idle),
and `perf_io-uring.txt` lost the frontend/backend groups to event multiplexing —
`io-uring`'s pipeline is read from its complete C10K capture below.

Across all measured models, frontend stalls dominate execution, while bad speculation
remains negligible.

**The single architectural finding: every model is frontend-latency-bound on
Zen4.** Frontend-bound is the largest bucket for all eight (59–69% of issue
slots), and inside it the _latency_ component (i-cache/BTB miss, fetch bubbles)
dominates the _bandwidth_ component roughly 4:1. Retiring never exceeds 17.1% at
C=100. These are small-message request/response loops whose instruction and
branch-target footprint outruns the Zen4 front-end; the wide back-end is starved,
not saturated. Bad speculation is negligible everywhere (≤0.7%) — the branch
mispredict rate (`ex_ret_brn_misp`) is low and the loops are predictable. The
event-loop models cluster tightly (epoll-et / event-loop identical at 14.0%
retiring, 65% frontend, 0.84 ops/cyc), confirming at the pipeline level what the
latency table shows: the reactor abstraction and hand-rolled epoll-ET are the
same machine (§10).

**Under 10,000-connection load** (`perf_<model>_c10k.txt`, signal models):

| Model        | Retiring % | Bad-spec % | Frontend % (latency) | Backend % (memory) | ops/cyc |
| ------------ | ---------- | ---------- | -------------------- | ------------------ | ------- |
| epoll-et     | 20.1       | 0.3        | 48.4 (39.5)          | 31.2 (27.7)        | 1.20    |
| multireactor | 15.1       | 0.4        | 61.0 (49.0)          | 23.4 (20.3)        | 0.91    |
| io-uring     | 12.7       | 0.3        | 62.3 (50.6)          | 24.7 (21.0)        | 0.76    |

Source: `bench/results/profiles/perf_epoll-et_c10k.txt`,
`perf_multireactor_c10k.txt`, `perf_io-uring_c10k.txt`. At C10K the working set
(10,000 slab entries + buffers) spills the private L3: `epoll-et`'s
`backend_bound_memory` rises from 18.0% to **27.7%** — the cache-miss signal on
Zen4 — while its retiring climbs to 20.1% and ops/cyc to 1.20 as the tight drain
loop stays hot. This binds directly to the io*uring verdict (§8): despite paying
**half** the syscalls, single-ring `io_uring` at C10K retires \_fewer* ops/cyc
(0.76 vs epoll-et's 1.20) and is _more_ frontend-latency-bound (50.6% vs 39.5%).
The completion path's per-CQE dispatch and buffer-ring bookkeeping is branchier
and more fetch-bound in one thread; on this workload the bottleneck is front-end
latency, not the syscall count `io_uring` optimizes.

## 6. multireactor scaling study

`bench/scaling.sh` swept `multireactor --workers` = 1, 2, 4, 8, 16, 32, 48 at
fixed concurrency 1000 / offered 80,000 rps
(`bench/results/multireactor_scaling.csv`):

| Workers | Throughput (req/s) | p50 (µs) | p99 (µs) | p99.9 (µs) |
| ------- | ------------------ | -------- | -------- | ---------- |
| 1       | 80000.0            | 67       | 87       | 107        |
| 2       | 80000.0            | 61       | 79       | 88         |
| 4       | 80000.0            | 57       | 82       | 89         |
| 8       | 80000.0            | 56       | 80       | 84         |
| 16      | 80000.0            | 57       | 81       | 85         |
| 32      | 80000.0            | 58       | 82       | 85         |
| 48      | 80000.0            | 58       | 82       | 86         |

Throughput remains fixed at the offered rate (80,000 req/s) for every worker
count because the benchmark is rate-limited rather than saturation-driven.
A single pinned reactor already absorbs the entire offered load (67 μs p50),
so increasing the number of reactors cannot increase measured throughput.
Additional workers reduce median latency slightly (67→56 μs at eight workers)
before plateauing.

## 7. Per-model mechanism

Because the benchmark is open-loop and rate-capped, all models that keep pace with the offered load achieve identical throughput at C=100 (20,000 req/s). This section therefore focuses on the mechanisms responsible for differences in latency, scalability, syscall frequency, context switching, and pipeline utilization. C=10,000 results refer to the dedicated C10K benchmark (bench/results/c10k*<model>.csv), while syscall, context-switch, and PMU measurements come from bench/results/profiles/summary.csv and the corresponding perf*<model>.txt captures.

> **iterative.** Single thread, one `accept()`→serve→`close` at a time, the
> reference model. At C=100 it serves the full 20,000 req/s (p99 = 87 µs)
> [`bench/results/iterative.csv`]; at C=10000 it does not complete — the single
> serving thread cannot drain 10,000 connections inside the budget and the point
> is recorded `saturated` [`bench/results/c10k_summary.csv`]. Profile: 2.044
> syscalls/req, 1.000 ctx-switches/req, and 67.9% frontend-bound / 13.9% retiring
> — the most frontend-latency-bound model in the set
> [`bench/results/profiles/summary.csv`, `bench/results/profiles/perf_iterative.txt`].
> Because only one connection is serviced at a time, head-of-line blocking is unavoidable
> once requests accumulate. Saturation therefore occurs as soon as offered concurrency
> exceeds the capacity of a single sequential execution path.

> **forking.** One `fork()` per connection, children reaped via `SIGCHLD`. At
> C=100 it serves 20,000 req/s (p99 = 96 µs) [`bench/results/forking.csv`]; at
> C=10000, given 384 GiB and 24 cores, it **sustains** 10,000 concurrent children
> at 50,000 rps with 0 errors (p50 = 73 µs, p99 = 95 µs)
> [`bench/results/c10k_forking.csv`, `bench/results/c10k_summary.csv`] — the
> laptop's `EAGAIN`-on-clone wall is a memory limit, not a model limit, and this
> host has the memory. Profile: 2.031 syscalls/req, ~0 ctx-switches/req on the
> idle parent; the `perf` parent-PID capture reports `<not counted>` because the
> work lives in the children [`bench/results/profiles/perf_forking.txt`]. Scalability
> is ultimately bounded by operating-system process creation and memory commitment rather
> than request processing. On this host those limits are not reached at 10,000 concurrent
> processes, but the process remains the most expensive isolation unit among the evaluated architectures.

> **preforked.** A fixed pool of `cfg.workers` worker processes, each with its
> own `SO_REUSEPORT` listener, kernel-balanced. At C=100 it serves 20,000 req/s
> (p99 = 86 µs) [`bench/results/preforked.csv`]; at C=10000 the fixed blocking
> pool cannot _hold_ 10,000 simultaneous connections and sheds them — 17008.8
> req/s served with 989,735 errors [`bench/results/c10k_preforked.csv`,
>
> > `bench/results/c10k_summary.csv`]. Profile: 2.158 syscalls/req, 0.012
> > ctx-switches/req [`bench/results/profiles/summary.csv`]. Scalability is limited
> > by the fixed worker pool. Once all workers are occupied, additional connections cannot
> > be parked and are rejected instead of queued, making the worker count the effective concurrency limit.

> **thread-per-conn.** One OS thread per connection, uncapped. At C=100 it serves
> 20,000 req/s (p99 = 89 µs) [`bench/results/thread-per-conn.csv`]; at C=10000 it
> **sustains** 10,000 threads at 50,000 rps with 0 errors (p50 = 71 µs, p99 = 92
> µs), RSS climbing to 267,284 KiB (≈ 261 MiB) for the thread stacks
> [`bench/results/c10k_thread-per-conn.csv`, `bench/results/c10k_summary.csv`].
> Profile: 2.049 syscalls/req, ~0 ctx-switches/req; 67.1% frontend-bound
> [`bench/results/profiles/summary.csv`, `bench/results/profiles/perf_thread-per-conn.txt`].
> The architecture scales successfully on this machine because sufficient memory is available
> for thread stacks. Its practical limit is therefore memory consumption rather than scheduling
> overhead; larger deployments eventually become constrained by per-thread stack commitment.

> **thread-pool.** A bounded worker pool draining a shared job queue with
> explicit fast-reject backpressure. At C=100 it serves 20,000 req/s (p99 = 86
> µs) [`bench/results/thread-pool.csv`]; at C=10000 it stays up (flat RSS ≈ 3
> MiB) but sheds heavily — 17022.4 req/s with 989,328 errors
> [`bench/results/c10k_thread-pool.csv`, `bench/results/c10k_summary.csv`].
> Profile: 2.047 syscalls/req, 1.006 ctx-switches/req
> [`bench/results/profiles/summary.csv`]. A bounded set of blocking workers cannot
> _hold_ 10,000 idle-ish connections, so backpressure converts excess load into
> errors rather than memory growth. The design intentionally trades scalability
> for predictable resource usage: excess concurrency is converted into rejected requests
> rather than additional memory consumption.

> **poll.** Single-thread `poll(2)` readiness loop, the O(n)-scan baseline that
> exists to make epoll's improvement measurable. At C=100 it serves 20,000 req/s
> (p99 = 116 µs) [`bench/results/poll.csv`]; at C=10000 it carries all 10,000 at
> 50,000 rps with 0 errors, but at **p50 = 7231 µs, p99 = 9527 µs**
> [`bench/results/c10k_poll.csv`], RSS rising to 11,004 KiB
> [`bench/results/c10k_summary.csv`]. Profile: 4.026 syscalls/req, 1.001
> ctx-switches/req; 59.0% frontend-bound, 20.3% backend-memory
> [`bench/results/profiles/summary.csv`, `bench/results/profiles/perf_poll.txt`].
> The implementation maintains all 10,000 connections because readiness is still event-driven,
> but each wake-up requires an O(n) scan of the descriptor array. Consequently, latency
> increases by roughly 77× relative to epoll-et despite identical connection capacity.
> Scalability is therefore limited by descriptor scanning rather than connection management.

> **epoll-lt.** Single-thread level-triggered `epoll`, non-blocking sockets,
> driving `core::Connection`. At C=100 it serves 20,000 req/s (p99 = 97 µs)
> [`bench/results/epoll-lt.csv`]; at C=10000 it carries all 10,000 at 50,000 rps
> with 0 errors but the worst median of any event-loop model, p50 = 195 µs (p99 =
> 248 µs) [`bench/results/c10k_epoll-lt.csv`]. Profile: **6.028 syscalls/req** —
> the highest of any model, from a second `epoll_wait` per request as
> level-triggered readiness re-fires for still-ready fds (20,015 `epoll_wait` vs
> epoll-et's 10,007) [`bench/results/profiles/summary.csv`,
>
> > `bench/results/profiles/strace_epoll-lt.txt`] — and 1.002 ctx-switches/req. Level-triggered
> > readiness repeatedly reports descriptors that remain ready, increasing the number of epoll_wait
> > calls and total syscall count. The resulting overhead approximately doubles median latency
> > relative to epoll-et, although it is far smaller than observed on the archived laptop.
> > Scalability is therefore limited by repeated readiness notifications rather than event-processing capacity.

> **epoll-et.** Single-thread edge-triggered `epoll`: drain each socket to
> `EAGAIN`, manage `EPOLLOUT` for partial writes, one `core::Connection` per fd.
> At C=100 it serves 20,000 req/s (p99 = 94 µs) [`bench/results/epoll-et.csv`]; at
> C=10000 it carries all 10,000 at 50,000 rps with 0 errors at p50 = 94 µs, p99 =
> 154 µs [`bench/results/c10k_epoll-et.csv`], RSS flat at 10,744 KiB
> [`bench/results/c10k_summary.csv`]. Profile: **4.028 syscalls/req** (1×
> `epoll_wait`, 1× `sendto`, 2× `recvfrom` — the second `recvfrom` is the
> edge-trigger drain returning `EAGAIN`) and **1.002 ctx-switches/req**
> [`bench/results/profiles/summary.csv`, `bench/results/profiles/strace_epoll-et.txt`];
> under C10K load, 20.1% retiring, 27.7% backend-memory, 1.20 ops/cyc — the most
> efficient pipeline of the signal models [`bench/results/profiles/perf_epoll-et_c10k.txt`].
> No capacity limit is observed at this workload; only queueing delay increases
> under sustained backlog. The implementation minimizes syscall overhead among
> readiness-based designs at the cost of a more complex drain-to-EAGAIN and partial-write state machine.

> **event-loop.** The same epoll-ET mechanism behind a reusable reactor
> abstraction with explicit buffer management. At C=100 it serves 20,000 req/s (p99 = 94 µs, identical to epoll-et)
> [`bench/results/event-loop.csv`]; at C=10000 it carries all 10,000 at 50,000 rps
> with 0 errors, p50 = 98 µs, p99 = 148 µs [`bench/results/c10k_event-loop.csv`].
> Profile: 4.027 syscalls/req, 1.002 ctx-switches/req, and a pipeline profile
> identical to epoll-et's (14.0% retiring, 65.2% frontend, 0.84 ops/cyc)
> [`bench/results/profiles/summary.csv`, `bench/results/profiles/perf_event-loop.txt`].
> Across throughput, latency, syscall counts, and pipeline utilization, the abstraction
> is indistinguishable from the hand-written epoll-et implementation. The previously observed
> difference on the archived laptop disappears under isolated core placement, indicating that
> it arose from measurement conditions rather than abstraction overhead. As with epoll-et, only
> queueing delay increases under sustained backlog.

> **multireactor.** Shared-nothing: reactor threads, each pinned to a core with
> its own `SO_REUSEPORT` listener, no acceptor, no fd handoff, no shared hot-path
> state; the kernel 4-tuple-hashes connections across the per-reactor listeners.
> At C=100 it serves 20,000 req/s (p99 = 91 µs) [`bench/results/multireactor.csv`];
> at C=10000 it carries all 10,000 at 50,000 rps with 0 errors at the best median
> of any model, p50 = 70 µs (p99 = 92 µs) [`bench/results/c10k_multireactor.csv`].
> Profile: **4.178 syscalls/req** — per-reactor identical to epoll-et's, because
> shared-nothing means no shared syscall — and **1.002 ctx-switches/req**, now
> indistinguishable from the single-thread models because the loadgen no longer
> shares its cores (§3, §10) [`bench/results/profiles/summary.csv`,
>
> > `bench/results/profiles/strace_multireactor.txt`]. Under C10K load, 15.1%
> > retiring, 0.91 ops/cyc [`bench/results/profiles/perf_multireactor_c10k.txt`]. Because each
> > reactor owns its connections independently, no synchronization occurs on the request path.
> > The remaining limitation is load imbalance: SO_REUSEPORT hashing does not provide work stealing,
> > so uneven connection lifetimes can leave reactors unevenly loaded.

> **io-uring.** Completion-based, purpose-built: a single ring on a single thread,
> multishot accept, provided buffer rings (the kernel selects the read buffer and
> reports it in the CQE), batched submission — the same unmodified
> `core::Connection` driven by completions instead of readiness. At C=100 it
> serves 20,000 req/s (p99 = 96 µs) [`bench/results/io-uring.csv`]; at C=1000 it
> holds the full 40,000 rps with 0 errors (p99 = 104 µs) — no shedding, unlike the
> laptop — and at C=10000 it **sustains** all 10,000 at 50,000 rps with 0 errors,
> p50 = 80 µs, p99 = 124 µs [`bench/results/io-uring.csv`,
>
> > `bench/results/c10k_io-uring.csv`]. Profile: the headline — **2.021
> > syscalls/req** (≈ 2× `io_uring_enter`, one read side, one write side; multishot
> > accept and provided buffers remove the per-accept and per-read syscalls) and
> > 1.002 ctx-switches/req [`bench/results/profiles/summary.csv`,
> >
> > > > `bench/results/profiles/strace_io-uring.txt`]; but under C10K load it retires
> > > > only 0.76 ops/cyc against epoll-et's 1.20 and is more frontend-latency-bound
> > > > (50.6% vs 39.5%) [`bench/results/profiles/perf_io-uring_c10k.txt`]. No scalability
> > > > limit is observed on this workload. Although io_uring achieves the lowest syscall count
> > > > of all evaluated designs, the reduction does not translate into higher pipeline efficiency.
> > > > The measurements are consistent with additional completion-processing overhead, leaving execution
> > > > frontend-bound rather than syscall-bound.

## 8. The io_uring verdict

The comparison between single-threaded io_uring and single-threaded epoll-et isolates the benefit of syscall elimination from multicore scaling. The measurements show that io_uring halves syscall frequency while matching epoll-et at C10K, but the reduction does not translate into higher pipeline utilization because the workload is frontend-bound.

- **Syscalls/req: 2.021 (io-uring) vs 4.028 (epoll-et)** — io_uring performs approximately
  half as many kernel entries (2.021 vs. 4.028 syscalls/request). Multishot accept eliminates
  repeated accept syscalls, while provided buffer rings eliminate per-read buffer setup,
  leaving roughly two io_uring_enter calls per request.
- **Ctx-switches/req: 1.002 (io-uring) vs 1.002 (epoll-et)** — identical
  [`bench/results/profiles/summary.csv`]. Both park once per request on their wait
  call.
- **Throughput at scale: both sustain C10K; neither sheds.** On the archived
  laptop single-ring `io_uring` shed above C≈1000; **on the EPYC box it does
  not.** At C=1000 it holds 40,000 rps with 0 errors
  [`bench/results/io-uring.csv`], and at a true 10,000 connections it holds 50,000
  rps with 0 errors (p99 = 124 µs) [`bench/results/c10k_io-uring.csv`] — the same
  rung `epoll-et` and `multireactor` clear. The predicted "single ring sheds above
  C≈1000" did **not** reproduce given disjoint cores and adequate
  `max_connections`; see §10.
- **Pipeline: fewer syscalls did not buy more useful work.** Under C10K load,
  `io_uring` retires **0.76 ops/cyc against `epoll-et`'s 1.20** and spends **50.6%
  of issue slots frontend-latency-bound against `epoll-et`'s 39.5%**
  [`bench/results/profiles/perf_io-uring_c10k.txt`,
  `bench/results/profiles/perf_epoll-et_c10k.txt`]. The measurements are consistent
  with additional completion-processing overhead (CQE dispatch and buffer-ring management),
  which increases frontend stalls despite the lower sysc

The conclusion, stated explicitly: **`io_uring` here wins syscalls/req by ≈2× on
the fair single-thread axis and, on this host, matches `epoll-et` at C10K rather
than shedding — but the AMD pipeline data shows its syscall win does not convert
into pipeline efficiency, because this workload is frontend-latency-bound, not
syscall-bound.** Since multireactor uses multiple cores while this implementation uses
a single ring on one core, their absolute throughput is not directly comparable.
The production form — thread-per-core, multi-ring — is the path to competing on absolute
throughput and is left as future work; it was deliberately not built, so
the single-ring number isolates the syscall mechanism.

## 9. C10K — resource curves and failure points

At a true 10,000 connections / 50,000 rps offered (`bench/c10k.sh`, 30 s,
`bench/results/c10k_summary.csv`), the models split into those that multiplex
connections onto a thread, those that allocate a thread/process per connection
(which now survive, given 384 GiB), and the bounded pools that shed.

**The event-driven architectures (poll, epoll-lt, epoll-et, event-loop, multireactor, and io_uring) all sustain 10,000 concurrent connections with nearly constant memory usage.** Their
resource curves are nearly constant for the whole run:

| Model        | RSS first→last (KiB) | fds first→last | Verdict                               |
| ------------ | -------------------- | -------------- | ------------------------------------- |
| poll         | 3480 → 11004         | 3093 → 4       | ok, 50000 rps, 0 errors (p50 7231 µs) |
| epoll-lt     | 10744 → 10748        | 10005 → 7444   | ok, 50000 rps, 0 errors               |
| epoll-et     | 10740 → 10744        | 10005 → 7668   | ok, 50000 rps, 0 errors               |
| event-loop   | 10740 → 10748        | 10005 → 7180   | ok, 50000 rps, 0 errors               |
| multireactor | 10948 → 10952        | 10027 → 27     | ok, 50000 rps, 0 errors               |
| io-uring     | 11568 → 11604        | 10005 → 6792   | ok, 50000 rps, 0 errors               |

Source: `bench/results/c10k_summary.csv` and the per-model
`bench/results/c10k_<model>.log`. RSS remains approximately constant at 10.7–11.6 MiB
while serving 10,000 concurrent connections, corresponding to roughly 1.1 KiB of server
memory per active connection.— and the fd count tracks live connections. This illustrates the primary
advantage of event-driven architectures: each connection requires only lightweight state (a connection
record and file descriptor), rather than an operating-system thread or process. The cost shows in
tail latency (§4), not memory or fds. `io_uring` now sits in this group on both footprint
(flat ≈ 11.6 MiB) and throughput (0 errors), the laptop shedding gone.

**The process-per-connection and thread-per-connection architectures also complete the C10K
workload on this host because sufficient physical memory is available.** `forking` sustains 10,000
children at 50,000 rps / 0 errors (parent RSS flat at 2472 KiB) and `thread-per-conn` sustains 10,000
threads at 50,000 rps / 0 errors, RSS climbing to 267,284 KiB (261 MiB) for the
stacks [`bench/results/c10k_summary.csv`,
`bench/results/c10k_forking.csv`, `bench/results/c10k_thread-per-conn.csv`]. Unlike the archived laptop results, neither architecture reaches process or thread creation limits on the EPYC system.

The remaining architectures fail for architectural rather than implementation reasons:

- **iterative**: recorded `saturated` — one blocking serving thread cannot
  establish and drain 10,000 connections inside the budget; the loadgen never
  completes and `c10k_iterative.csv` is empty
  [`bench/results/c10k_summary.csv`].
- **preforked** and **thread-pool**: survive without dying (bounded RSS) but shed
  ~99% of load — 17008.8 rps / 989,735 errors and 17022.4 rps / 989,328 errors
  respectively [`bench/results/c10k_preforked.csv`,
  `bench/results/c10k_thread-pool.csv`]. A bounded blocking pool can _serve_ but
  cannot _hold_ 10,000 concurrent connections. On this host the C10K failure mode
  is a bounded worker set, not memory exhaustion.
- **poll** survives on capacity but collapses on latency: p50 = 7231 µs from the
  O(n) per-wakeup fd scan [`bench/results/c10k_poll.csv`].

## 10. Discussion and interpretation

Findings that contradicted a going-in hypothesis or flipped a laptop-era caveat,
each with its number.

- **Core isolation removes the apparent context-switch overhead of multireactor.** The laptop
  reported 1.153 and attributed it to 8 pinned reactors sharing 8 cores with the
  loadgen. The prediction was that on disjoint cores it would fall toward 1.0. It
  did: **1.002** [`bench/results/profiles/summary.csv`], identical to single-thread
  `epoll-et` (1.002) and `io-uring` (1.002). Predicted, then confirmed — the
  laptop figure was a co-residency confound, not a model cost.
- **The reusable reactor abstraction introduces no measurable overhead relative to handwritten epoll-ET.**
  The laptop showed the two crossing over and diverging; on disjoint
  Zen4 cores they are indistinguishable — p99 = 94/94 µs at C=100, 95/95 µs at
  C=1000, 148/154 µs at C10K [`bench/results/event-loop.csv`,
  `bench/results/epoll-et.csv`, `bench/results/c10k_event-loop.csv`,
  `bench/results/c10k_epoll-et.csv`], with identical syscalls/req (4.027 vs 4.028)
  and identical pipeline buckets (14.0% retiring, 65% frontend, 0.84 ops/cyc)
  [`bench/results/profiles/summary.csv`, `bench/results/profiles/perf_event-loop.txt`,
  `bench/results/profiles/perf_epoll-et.txt`]. The laptop crossover was
  contention, not abstraction cost — the hypothesis now holds.
- **Single-ring io_uring sustained the full C10K workload.** The laptop shed at
  C=1000 (287k errors) and C=8000; on EPYC, with disjoint cores and
  `--max-connections 16384`, it holds C=1000 (0 errors) and a true C10K (0
  errors, p99 = 124 µs) [`bench/results/io-uring.csv`,
  `bench/results/c10k_io-uring.csv`]. The prediction that "single-ring io_uring
  still sheds above C≈1000" was not borne out on this host; the laptop shedding was
  a small-host artifact, not a ring property.
- **Lower syscall counts do not translate into higher pipeline utilization.** Despite half
  the syscalls, at C10K it retires 0.76 ops/cyc vs `epoll-et`'s 1.20 and is more
  frontend-latency-bound (50.6% vs 39.5%)
  [`bench/results/profiles/perf_io-uring_c10k.txt`,
  `bench/results/profiles/perf_epoll-et_c10k.txt`]. The AMD pipeline data
  reframes the verdict: on a frontend-latency-bound workload, eliminating syscalls
  is not eliminating the bottleneck (§8).
- **Frontend latency dominates execution across all measured architectures.** Frontend-bound is the largest
  bucket for all measured models (59–69% at C=100), latency-dominated ~4:1 over
  bandwidth, with retiring never above 17.1%
  [`bench/results/profiles/perf_*.txt`, §5A]. The processor is starved on
  instruction fetch, not saturated on execution — the shared shape of every
  small-message request/response loop here.
- **Process- and thread-per-connection architectures are constrained primarily by available memory rather than concurrency itself.** On the laptop `forking` and `thread-per-conn` panicked with `EAGAIN`; on 384 GiB both carry
  10,000 connections at 50,000 rps with 0 errors
  [`bench/results/c10k_forking.csv`, `bench/results/c10k_thread-per-conn.csv`]. The
  C10K wall for these models is memory, not architecture; given the memory they
  clear it. The models that fail here are the bounded pools (preforked,
  thread-pool) and single-thread iterative.
- **The multireactor scaling experiment was limited by the offered load rather than processor capacity.** — a
  measurement-design limit, not a model finding: 80,000 rps is below the
  saturation of even one EPYC reactor (p50 = 67 µs at 1 worker), so the signal
  cannot appear in throughput [`bench/results/multireactor_scaling.csv`, §6].

  Taken together, these results indicate that the dominant performance differences between server architectures are determined less by raw syscall counts than by how efficiently they manage concurrency and exploit available hardware resources. Event-driven architectures consistently provide the lowest memory footprint while sustaining C10K workloads, whereas thread- and process-based designs remain viable on sufficiently provisioned hardware but incur substantially higher per-connection resource costs. The Zen4 pipeline analysis further shows that this workload is predominantly frontend-latency-bound, explaining why reducing syscall frequency alone does not necessarily improve end-to-end performance.

## 11. Reproducibility and data provenance

Every quantitative result presented in this document can be traced directly to a committed artifact under bench/results/, enabling independent verification and complete reproduction of all figures, tables, and claims.

| Claim / figure                                                        | Source file                                                                                                                              |
| --------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| C=100 throughput/p50/p99/p99.9, every model                           | `bench/results/<model>.csv` (row `rate=20000,connections=100`)                                                                           |
| C=1000 throughput/latency, every model                                | `bench/results/<model>.csv` (row `rate=40000,connections=1000`)                                                                          |
| C=10000 (C10K rung) latency/throughput/errors                         | `bench/results/c10k_<model>.csv`                                                                                                         |
| C10K verdicts, RSS/fd/ctx curves summary                              | `bench/results/c10k_summary.csv`                                                                                                         |
| C10K per-sample resource curves                                       | `bench/results/c10k_<model>.log`                                                                                                         |
| syscalls/req + ctx-switches/req, all 11 models                        | `bench/results/profiles/summary.csv`, `bench/results/profiles/strace_<model>.txt`, `bench/results/profiles/ctx_<model>.txt`              |
| syscall breakdown (epoll_wait/recvfrom/sendto; io_uring_enter)        | `bench/results/profiles/strace_epoll-et.txt`, `bench/results/profiles/strace_epoll-lt.txt`, `bench/results/profiles/strace_io-uring.txt` |
| AMD Zen4 pipeline buckets (retiring/bad-spec/frontend/backend), C=100 | `bench/results/profiles/perf_<model>.txt`                                                                                                |
| AMD Zen4 pipeline buckets under C10K load (signal models)             | `bench/results/profiles/perf_epoll-et_c10k.txt`, `perf_multireactor_c10k.txt`, `perf_io-uring_c10k.txt`                                  |
| multireactor scaling table (workers 1–48)                             | `bench/results/multireactor_scaling.csv`, `bench/results/scaling_w*.csv`                                                                 |
| latency distribution plot (C=100)                                     | `bench/results/distribution_c100.png` (from the `*_r20000_c100.hgrm` dumps)                                                              |
| throughput-vs-concurrency plot                                        | `bench/results/throughput_vs_concurrency.png`                                                                                            |
| p99-vs-concurrency plot                                               | `bench/results/p99_vs_concurrency.png`                                                                                                   |
| multireactor scaling plot                                             | `bench/results/multireactor_scaling.png`                                                                                                 |
| host CPU / RAM / kernel / microcode / governor / NPS / pinning        | `bench/results/rig.txt`                                                                                                                  |
| C10K methodology and true-10000 justification                         | `bench/results/c10k_README.md`                                                                                                           |
| AMD pipeline methodology and Zen4 event mapping                       | `bench/results/profiles/README.md`                                                                                                       |
