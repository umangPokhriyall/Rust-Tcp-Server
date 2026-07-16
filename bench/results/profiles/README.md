# Profiling — syscalls/req, ctx-switches/req, AMD Zen4 pipeline utilization

This directory holds the profiling passes produced by `bench/profile.sh` on the
AMD EPYC 9254 run of record (`bench/results/rig.txt`, git `bf67267`). Three
independent passes are captured for all eleven models; the pipeline pass gets a
second capture under 10,000-connection load for the signal models (`epoll-et`,
`multireactor`, `io-uring`).

## Files

- `strace_<model>.txt` — `strace -c -f` syscall summary (C=10, rate 500).
- `ctx_<model>.txt` — pre/post `/proc/<pid>/task/*/status` context-switch deltas,
  summed across all threads (C=100, rate 2000).
- `perf_<model>.txt` — steady-state AMD Zen4 pipeline-utilization capture (C=100,
  rate 20,000, 20 s window).
- `perf_<model>_c10k.txt` — pipeline capture under 10,000-connection load (signal
  models only).
- `summary.csv` — derived `syscalls_per_req` and `ctx_switches_per_req` for all
  eleven models.

## syscalls/req and ctx-switches/req

From `summary.csv`:

| Model | syscalls/req | ctx-switches/req |
|---|---|---|
| iterative | 2.044 | 1.000 |
| forking | 2.031 | 0.001 |
| preforked | 2.158 | 0.012 |
| thread-per-conn | 2.049 | 0.001 |
| thread-pool | 2.047 | 1.006 |
| poll | 4.026 | 1.001 |
| epoll-lt | 6.028 | 1.002 |
| epoll-et | 4.028 | 1.002 |
| event-loop | 4.027 | 1.002 |
| multireactor | 4.178 | 1.002 |
| io-uring | 2.021 | 1.002 |

The headline pair, on the fair single-thread axis: **`epoll-et` = 4.028
syscalls/req, single-ring `io_uring` = 2.021** — a 1.99× reduction. From the
strace breakdowns:

- `epoll-et` (`strace_epoll-et.txt`): 10,007 `epoll_wait` + 20,011 `recvfrom`
  (the second per request is the edge-trigger drain to `EAGAIN`) + 10,000
  `sendto` = 40,276 over 10,000 requests.
- `io-uring` (`strace_io-uring.txt`): 20,012 `io_uring_enter` (read side + write
  side) + a handful of setup calls = 20,208; multishot accept and provided buffer
  rings remove the per-accept and per-read syscalls.
- `epoll-lt` (`strace_epoll-lt.txt`): 6.028/req — the highest — from a second
  `epoll_wait` per request as level-triggered readiness re-fires for still-ready
  fds (20,015 `epoll_wait` vs `epoll-et`'s 10,007).

`multireactor`'s ctx-switches/req is **1.002**, indistinguishable from
single-thread `epoll-et` and `io-uring`. On the archived laptop it was 1.153,
inflated by 8 reactors sharing 8 cores with the loadgen; on the EPYC box the
loadgen runs on disjoint cores (12–23) and the confound is gone.

## AMD Zen4 pipeline-utilization analysis (not Intel TMA relabeled)

This is **AMD Zen4 pipeline-utilization analysis — the architectural counterpart
to Intel's Top-down Microarchitecture Analysis (TMA), not Intel TMA relabeled.**
Intel's `TopdownL1/L2` metric groups do not exist on Zen4; running them verbatim
errors or silently misleads. The EPYC PMU exposes its own pipeline groups
(`PERF_METRIC_GROUP =
frontend_bound_group,backend_bound_group,retiring_group,bad_speculation_group`,
`bench/results/rig.txt`), which decompose issue slots into the same four top-level
categories via different silicon events. Each `perf_<model>.txt` reports the AMD
sub-metrics; the four top-level buckets are:

- **Retiring** = `retiring_fastpath` + `retiring_microcode` (useful work).
- **Bad speculation** = `bad_speculation_mispredicts` +
  `bad_speculation_pipeline_restarts`.
- **Frontend bound** = `frontend_bound_latency` + `frontend_bound_bandwidth`
  (fetch/decode starvation).
- **Backend bound** = `backend_bound_cpu` + `backend_bound_memory`
  (execution/retire stalls; memory-latency stalls).

The classic scalar counters map to their Zen4 events, not Intel's:

| Classic metric | Zen4 event(s) in these captures |
|---|---|
| IPC | retired macro-ops per unhalted cycle: `ex_ret_ops / ls_not_halted_cyc` |
| branch-miss | `ex_ret_brn_misp` (retired mispredicted branches), shown as `bad_speculation_mispredicts` |
| cache-miss | no discrete LLC-miss event captured; memory-latency stalls appear as `backend_bound_memory` (from `ex_no_retire.load_not_complete` / `de_no_dispatch_per_slot.backend_stalls`) |

### Steady-state buckets (C=100, `perf_<model>.txt`)

| Model | Retiring % | Bad-spec % | Frontend % (latency) | Backend % (memory) | ops/cyc |
|---|---|---|---|---|---|
| iterative | 13.9 | 0.3 | 67.9 (52.8) | 17.8 (14.8) | 0.83 |
| thread-per-conn | 12.5 | 0.4 | 67.1 (54.5) | 18.5 (16.0) | 0.77 |
| thread-pool | 11.4 | 0.7 | 69.3 (56.7) | 14.3 (12.4) | 0.75 |
| poll | 17.1 | 0.4 | 59.0 (49.4) | 23.5 (20.3) | 1.03 |
| epoll-lt | 14.7 | 0.6 | 64.5 (51.5) | 20.2 (17.7) | 0.88 |
| epoll-et | 14.0 | 0.4 | 65.0 (52.3) | 20.5 (18.0) | 0.84 |
| event-loop | 14.0 | 0.4 | 65.2 (52.4) | 20.3 (17.9) | 0.84 |
| multireactor | 13.3 | 0.5 | 67.2 (54.7) | 18.5 (16.1) | 0.81 |

Two models have no complete C=100 row: `perf_forking.txt` reports `<not counted>`
for every event (the per-connection children carry the work; the perf-attached
parent PID is idle), and `perf_io-uring.txt` lost the frontend/backend groups to
event multiplexing — `io-uring`'s pipeline is read from its C10K capture.

### Under 10,000-connection load (`perf_<model>_c10k.txt`, signal models)

| Model | Retiring % | Bad-spec % | Frontend % (latency) | Backend % (memory) | ops/cyc |
|---|---|---|---|---|---|
| epoll-et | 20.1 | 0.3 | 48.4 (39.5) | 31.2 (27.7) | 1.20 |
| multireactor | 15.1 | 0.4 | 61.0 (49.0) | 23.4 (20.3) | 0.91 |
| io-uring | 12.7 | 0.3 | 62.3 (50.6) | 24.7 (21.0) | 0.76 |

### What the pipeline data shows

- **Every model is frontend-latency-bound on Zen4.** Frontend-bound is the
  largest bucket for all measured models (59–69% of issue slots at C=100),
  latency-dominated ~4:1 over bandwidth; retiring never exceeds 17.1%. These are
  small-message request/response loops whose instruction/branch-target footprint
  outruns the front-end; the wide back-end is starved, not saturated. Bad
  speculation is negligible (≤0.7%).
- **At C10K the working set spills L3.** `epoll-et`'s `backend_bound_memory`
  rises from 18.0% (C=100) to 27.7% (C10K) — the Zen4 cache-miss signal — as the
  10,000-entry connection table exceeds the private 32 MiB per-CCD L3.
- **io_uring's syscall win is not a pipeline win.** Despite half the syscalls, at
  C10K `io-uring` retires 0.76 ops/cyc vs `epoll-et`'s 1.20 and is more
  frontend-latency-bound (50.6% vs 39.5%). The completion path's per-CQE dispatch
  and buffer-ring bookkeeping is branchier in one thread; on a frontend-bound
  workload the syscall count is not the limiting resource. This binds the §8
  io_uring verdict to the pipeline data.
- **The reactor abstraction is pipeline-identical to hand-rolled epoll-ET.**
  `event-loop` and `epoll-et` match to the decimal (14.0% retiring, 65% frontend,
  0.84 ops/cyc), confirming zero abstraction cost.

See `docs/BENCHMARKS.md` §5A and §8 for the full analysis.
