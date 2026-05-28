# Rust-Tcp-Server — Phase 2 Specification (Revised): Final Models, Definitive Sweep, and Signal Artifacts

**Companion to:** `kickoff-brief.md`, `phase0-spec.md`, `phase1-spec.md`. Read all three first.
**This is the complete, authoritative Phase 2 spec.** It supersedes the prior version: the writeup sections (§9–§12) are now rigorously specified and a governing Writing Standard (§8) has been added.
**Scope:** the final two models (`multireactor`, `io-uring`), the definitive 11-model sweep + scaling study, the profiling deep-dive, and the artifacts that convert the repo into proof-of-work — `docs/BENCHMARKS.md`, `README.md`, `docs/ARCHITECTURE.md`, `docs/x-thread.md`.
**Audience:** Claude Code. Authoritative. Final phase for this repo.

---

## 1. Phase 2 in one paragraph

Phase 1 delivered nine models, the load generator, and committed results. Phase 2 adds the two state-of-the-art models — `multireactor` (shared-nothing epoll-ET across pinned cores) and `io-uring` (completion-based, purpose-built) — runs the definitive sweep across all eleven, profiles the key models at the microarchitecture level, and produces the writeups that make the engineering legible to a reviewer who has never met the author. After Phase 2 the repo is a finished, publishable artifact.

### 1.1 Frozen / reused
- **`core` is frozen.** The sans-IO `Connection` drives io_uring's completion model unchanged. If io_uring appears to need a `core` change, the model is wrong — STOP and ask.
- **`reactor.rs` is reused unchanged** by `multireactor`.
- **`sys` gains exactly one file:** `affinity.rs`.

---

## 2. Workspace additions & dependencies

```
server/src/sys/affinity.rs          # NEW — CPU pinning
server/src/models/multireactor.rs   # NEW
server/src/models/io_uring.rs       # NEW
docs/BENCHMARKS.md                  # NEW — the teardown
docs/ARCHITECTURE.md                # FINALIZE
docs/x-thread.md                    # NEW — X thread draft + strategy notes
README.md                           # REWRITE as pinned-ready teardown
bench/results/profiles/             # NEW — perf/strace telemetry
```

Dependency additions (only these): `server` adds **`io-uring`** (the raw tokio-rs `io-uring` crate — never `tokio-uring`, never `tokio`). `libc` (present) covers affinity.

---

## 3. `sys/affinity.rs`

```rust
/// Pin the calling thread to one logical core via sched_setaffinity.
/// Warn-and-continue if core_id is out of range. Linux-only.
pub fn pin_to_core(core_id: usize) -> std::io::Result<()>;
/// Logical core count (sysconf _SC_NPROCESSORS_ONLN).
pub fn num_cores() -> usize;
```

---

## 4. `multireactor.rs`

**Shared-nothing, no acceptor.** `cfg.workers` reactor threads (default `num_cores()`). Each: `bind_listener(addr, reuse_port = true)` (own `SO_REUSEPORT` listener), `pin_to_core(i)`, build a `reactor::Reactor`, `run(&shutdown)`. The kernel load-balances connections across the per-reactor listeners — no shared acceptor, no fd handoff, no shared state, no hot-path lock. Shutdown via a shared `AtomicBool` set by the SIGINT/SIGTERM handler; main joins all threads.

**On the brief's "one acceptor + N reactors":** `SO_REUSEPORT` supersedes it — build shared-nothing. Record the rejected acceptor+handoff design and the reason in BENCHMARKS.md (§9) and ARCHITECTURE.md (§11).

**Defining characteristic (telemetry to capture):** near-linear multicore scaling, zero shared state, zero hot-path contention; each reactor is a pinned epoll-ET loop. Caveat: kernel-hash 4-tuple balancing imbalances under skewed connection lifetimes, with no work-stealing (same as `preforked`).

---

## 5. `io_uring.rs` — completion-based, purpose-built

**Must be purpose-built, not drop-in** (drop-in ≈ 1.06x and proves nothing).

**Hard requirements:** raw `io-uring` crate (verify exact opcode/buffer-ring API against the installed crate docs); multishot accept; provided buffer rings (buffer-select reads, CQE reports the filled buffer); batched submission (many SQEs per `io_uring_enter`). Multishot recv (kernel ≥6.0) is an optional stretch.

**Kernel gate:** requires kernel ≥5.19 (target ≥6.1). Read `uname -r` at startup; if too old, print `io_uring unavailable: kernel X.Y < 5.19`, exit non-zero for this model, and the sweep records io_uring N/A (§6). Document the kernel in BENCHMARKS.md.

**Flow (completion, not readiness):** (1) submit multishot accept once; (2) accept CQE → register a `core::Connection`, submit a buffer-select read; (3) read CQE → bytes are in the provided buffer → `conn.on_bytes` → on `WantWrite` submit a write of `pending_write`, return the buffer to the ring; (4) write CQE → `conn.on_written` → submit next read, or close on `Close`, or submit the remainder on partial write. Encode `(conn_id, op_kind)` in each SQE `user_data`; keep a connection slab keyed by `conn_id`.

**The payoff to state:** `core::Connection` is used unmodified — the same sans-IO machine that drove blocking and epoll now drives completions. Phase 0 validated three ways.

**Scope:** single ring, single thread (isolates syscall-elimination vs single-thread `epoll-et` — the fair comparison). Thread-per-core multi-ring is the production form and the path to competing with `multireactor` on absolute throughput — note as future work, do not build.

**Defining characteristic (telemetry):** completion I/O removes the readiness syscall; multishot accept + provided buffers remove per-op submission overhead; syscalls/req drops far below epoll. Cost: complexity, kernel dependence, benefit only when purpose-built.

---

## 6. The definitive sweep + scaling study

Run `bench/run.sh` across **all 11 models** at concurrency **1 / 10 / 100 / 1000 / 10000** on kernel ≥6.1 if available. Commit fresh CSVs + histogram dumps to `bench/results/`, overwriting Phase 1 partials. **C10K:** confirm every event-loop model and `multireactor` survive 10,000 concurrent keep-alive connections; record where the thread/process models degrade and the symptom (expected — a result, not a failure). **Scaling study:** sweep `multireactor --workers` = 1,2,4,8,…,`num_cores()` at fixed high concurrency; emit `bench/results/multireactor_scaling.csv` + a scaling-factor plot (throughput@N ÷ throughput@1). If io_uring is N/A, record N/A and proceed with ten.

---

## 7. The profiling deep-dive

For `epoll-et` (single-thread), `multireactor`, and `io-uring`, capture to `bench/results/profiles/`: top-down microarchitecture (`perf stat` topdown: retiring/bad-spec/frontend/backend bound); context-switches, cpu-migrations, cache-misses, instructions, IPC; **syscalls per request** (the headline io_uring-vs-epoll number — `perf stat -e 'syscalls:sys_enter_*'` over a fixed request count, or `strace -c -f` fallback); full interior latency histograms (log-y). Privilege fallback if `perf` is restricted: `/proc/<pid>/status` ctxt-switch counters + `strace -c`. Document the method used.

---

## 8. Writing Standard — governs every artifact in §9–§12

Claude Code MUST follow these rules in all four documents. They are the difference between a portfolio README and an artifact a Principal Engineer hires on.

1. **Authoritative and declarative.** State findings as facts: "epoll-et sustains X req/s at C=10000 (`bench/results/epoll_et.csv`)." Not "epoll-et seems quite fast."
2. **Every quantitative claim cites its committed source file**, inline, in parentheses. A number without a provenance file does not appear.
3. **Numbers carry units and conditions.** Never a bare number — always value + unit + concurrency level + kernel/machine where it matters. "p99 = 1.8 ms at C=1000" not "p99 is low."
4. **Zero marketing language.** Banned: blazing, lightning, incredible, amazing, revolutionary, simply, just, powerful, seamless, exclamation marks, emoji, "we're excited." The telemetry and the architecture carry the weight.
5. **Tables and plots over prose** wherever data is dense. Prose explains *mechanism*, not data a table already shows.
6. **Intellectual honesty is mandatory and is the signal.** State what underperformed, what contradicted the hypothesis, what remains unexplained, and what you would change. A senior reader trusts a writeup more for its admitted limits.
7. **Commit where the data is clear; hedge only where it is genuinely ambiguous.** No reflexive hedging.
8. **Assume a senior systems reader.** Do not explain what epoll or a context switch is. Do explain every non-obvious *design choice* and every surprising *number*.
9. **Reproducibility is non-negotiable.** Every figure/result states the exact command and environment to reproduce it.
10. **Comparisons are apples-to-apples and name the axis** (single-thread vs single-thread; per-core normalized). Never compare 1-core io_uring to N-core multireactor without saying so.

---

## 9. `docs/BENCHMARKS.md` — the teardown

Built entirely from committed numbers. Structure:

1. **Thesis** (one paragraph): every TCP server I/O model from accept-loop to purpose-built io_uring, one `Server` trait, measured honestly.
2. **Environment & methodology:** CPU model, physical/logical cores, RAM, kernel version, NIC/loopback note; the open-loop coordinated-omission-correct load model in three sentences; the one-command reproduction (`bench/run.sh`).
3. **Threats to validity** (subsection — do not omit): coordinated-omission handling, warmup, what was held constant, residual confounds (asset page cache, loopback vs real network, single-host loadgen contention). This subsection is a credibility multiplier; write it plainly.
4. **Headline results table** — exact columns: `Model | Concurrency | Throughput (req/s) | p50 (µs) | p99 (µs) | p99.9 (µs) | Syscalls/req | Ctx-switches/req`. Rows for representative concurrencies (at least C=100 and C=10000).
5. **Plots** (embedded): interior latency distribution (log-y); throughput-vs-concurrency; p99-vs-concurrency; the multireactor scaling-factor plot. Each plot caption cites its source CSV.
6. **Per-model mechanism paragraphs** — use this exact template for all 11, in order (iterative → forking → preforked → thread-per-conn → thread-pool → poll → epoll-lt → epoll-et → event-loop → multireactor → io-uring):
   > **{model}.** {One-line mechanism.} At C=100 it sustains {X} req/s (p99 {Y} µs); at C=10000, {Z} req/s (p99 {W} µs) [`<file>`]. The profile shows {syscalls/req}, {ctx-switches/req}, {top-down bound} [`<profile file>`]. {Why those numbers — the mechanism, grounded in the profile.} It breaks at {failure mode}. It represents the tradeoff of {tradeoff}.
7. **The io_uring verdict** — single-ring io_uring vs single-thread epoll-et (the fair axis), the syscalls/req delta with both numbers, and the explicit statement that multireactor wins absolute throughput on N cores while io_uring here uses one. If N/A on the kernel, say so and present the design.
8. **C10K** — not just "survived": the resource curve at 10k for the event-loop models (RSS, fd count, ctx-switch rate) and the exact failure point + symptom for the thread/process models.
9. **Surprises and corrections** (subsection): findings that contradicted the hypothesis (e.g. event-loop being zero-cost over bare epoll-et, or io_uring not winning on this workload, or multireactor load imbalance), each with its number.
10. **Data provenance table:** every figure and headline claim → its source file under `bench/results/`.
11. **The microVM bridge** (closing): multireactor = sandbox API control plane (acceptor-free, SO_REUSEPORT, pinned reactors); the epoll-ET/io_uring loop = host↔guest I/O multiplexer; `core::Connection` = sandbox lifecycle state machine; correctness-under-failure + backpressure = orchestrator discipline. Two short paragraphs, no hype.

---

## 10. `README.md` — pinned-ready (the 60-second review)

The headline number, table, and best plot MUST appear above any prose deep-dive (the 30-second grasp). Exact order:

1. **One sentence:** what this is — 11 TCP server concurrency models, accept-loop through purpose-built io_uring, behind one `Server` trait, benchmarked.
2. **Headline result:** the single most striking number, with condition and source file.
3. **Repository map** (table): `core/` (sans-IO protocol), `server/src/sys/` (raw OS I/O), `server/src/reactor.rs` (event-loop assembly), `server/src/models/` (the 11 strategies), `loadgen/` (open-loop generator), `bench/` (harness + results). One line each — a reviewer navigates by this.
4. **Results table** (same columns as BENCHMARKS §9.4, condensed to C=100 and C=10000).
5. **One plot** inline (the most striking).
6. **Model index table:** `Model | Mechanism (one line) | Best-case throughput | Where it breaks`.
7. **Why this exists** (2 sentences): the AI-agent sandbox / infra relevance. No hype.
8. **Build & run:** exact commands (`cargo build --release`, `server --model <name> --port <p> --assets-dir ...`).
9. **Reproduce the benchmarks:** the exact `bench/run.sh` invocation + environment note.
10. **Links:** `docs/BENCHMARKS.md`, `docs/ARCHITECTURE.md`.

No decorative badges (build/CI status only, if any). No emoji. Follow §8.

---

## 11. `docs/ARCHITECTURE.md` — finalize

1. **Layering diagram** (ASCII): the four layers `core → sys → reactor → models` with dependency arrows and an explicit line marking the sans-IO boundary (everything above it touches no socket).
2. **Per-layer contract** — for each of core / sys / reactor / models: what it owns, what it must never do, its public surface. `core`: protocol, sans-IO, never a syscall. `sys`: raw OS I/O, no protocol logic. `reactor`: assembly, no model-selection. `models`: one strategy each, no copy-pasted logic.
3. **The sans-IO rationale:** why protocol is separated from I/O, and the concrete payoff — blocking `read`/`write`, epoll readiness, and io_uring completion all reuse one `Connection` state machine.
4. **Evidence table** proving "one frozen core served all 11 models": `Model | I/O mechanism | Consumes unmodified core::Connection? (Y)`. All 11 rows = Y.
5. **Connection lifecycle:** the `Reading → Writing → KeepAlive/Close` state machine and the `ConnAction` (WantRead/WantWrite/Close) contract.
6. **Key design decisions + rejected alternatives** (each with the reason): no async runtime/tokio; shared-nothing `SO_REUSEPORT` over single-acceptor+handoff; `Vec` header store over `HashMap`; provided buffer rings over per-read allocation; single-ring io_uring scope. State the tradeoff accepted in each.

---

## 12. `docs/x-thread.md` — strategy + draft

### 12.1 Strategy (write this at the top of the file as a short brief for the owner)
- **Why X:** with no professional network, the thread is how a cold artifact reaches the people who hire (systems + AI-infra engineers). It is the distribution channel, not a vanity post.
- **Governing principle:** post findings, not hype. This is a technical result, not a launch. The numbers do the persuading.
- **Targeting / engagement:** be discoverable to the Rust-systems, io_uring, and AI-infra (sandbox/inference/voice) communities. Engage by replying with technical substance to *their* posts over time — never cold-@ famous accounts in the thread.
- **Cadence:** post only when able to be present for 2–3 hours after; threads die without author engagement. Reply to every substantive reply. A day later, quote-tweet the single best plot standalone.
- **Conversion path:** thread → profile → repo → BENCHMARKS.md → conversation. The CTA is the repo link. No "looking for work" in the thread; availability lives in bio/pinned, proof-first.
- **Do not:** hashtag-stuff, cold-tag, or editorialize beyond the data.

### 12.2 Draft (claude-code fills the {placeholders} from committed numbers; owner edits for voice)
A 7–9 post thread, each ≤280 chars, every number with its condition:
- **1 (hook):** Built every TCP server concurrency model in Rust — from `accept()`-in-a-loop to purpose-built io_uring, 11 models behind one trait — and benchmarked all of them honestly. Thread.
- **2 (method):** Open-loop load, corrected for coordinated omission. Most server benchmarks quietly hide tail latency; this one measures from when each request *should* have been sent.
- **3 (throughput/p99):** {headline throughput + p99 at C=10000, top models} [plot ref].
- **4 (syscalls/req):** The io_uring story is syscalls: {epoll syscalls/req} → {io_uring syscalls/req}. Completion I/O + multishot accept + provided buffers, not readiness.
- **5 (C10K):** thread-per-connection collapses at {point/symptom}; the event-loop models hold 10k keep-alive connections flat at {RSS/ctx-switch number}.
- **6 (honest verdict):** Single-ring io_uring vs single-thread epoll-et: {result}. multireactor wins absolute throughput — but on N cores, not by magic. Axis matters.
- **7 (reactor finding):** {the event-loop-vs-epoll-et zero-cost-abstraction result, if it held}.
- **8 (bridge):** This is the shape of an AI-agent sandbox control plane: acceptor-free SO_REUSEPORT reactors dispatching to isolated workers.
- **9 (CTA):** Full teardown, every number sourced, reproducible harness: {repo link}.

---

## 13. Phase 2 Definition of Done

1. `core` byte-for-byte unchanged since Phase 0.
2. `multireactor` and `io-uring` implemented (§4–§5), each passing the §4-common-bar, conformance, and a 60s soak (io_uring N/A documented if kernel too old).
3. multireactor scaling study committed; scaling near-linear to core count or the deviation explained.
4. io_uring purpose-built and beats single-thread epoll-et on the workload, or an honest negative result with a top-down profile explains why.
5. Definitive 11-model sweep committed with all plots; C10K recorded with the resource curve / failure symptoms.
6. Profiling telemetry committed under `bench/results/profiles/` (top-down, syscalls/req, ctx-switches).
7. `docs/BENCHMARKS.md` complete per §9 — every claim sourced, threats-to-validity and surprises sections present, microVM bridge.
8. `README.md` per §10 (60-second grasp above the fold); `docs/ARCHITECTURE.md` per §11 (evidence table + rejected alternatives); `docs/x-thread.md` per §12 (strategy + sourced draft).
9. All four documents conform to the §8 Writing Standard — verify by re-reading each against the 10 rules.
10. `cargo build`/`clippy`/`test` clean; dependency allowlist respected.

Final phase — no Phase 3.

---

# Appendix A — `CLAUDE.md` update for Phase 2

```markdown
## Authoritative specs
- docs/specs/kickoff-brief.md  — strategy, full model list, §4 common bar
- docs/specs/phase0-spec.md    — core foundation (FROZEN)
- docs/specs/phase1-spec.md    — sys, reactor, first 9 models, loadgen, bench
- docs/specs/phase2-spec.md    — CURRENT: multireactor, io-uring, sweep, writeups

## Hard rules
1. `core` is FROZEN. OS I/O lives in server/src/sys/. io_uring uses the raw
   `io-uring` crate only — never tokio-uring/tokio.
2. No hot-path logging. No async runtime. io_uring must be PURPOSE-BUILT.
3. Phase 2 deps: server -> `io-uring`. Nothing else.
4. WRITING STANDARD (BENCHMARKS/README/ARCHITECTURE/x-thread): authoritative,
   declarative, every number cites its committed source file with units and
   conditions. No marketing words (blazing/incredible/seamless/etc.), no emoji,
   no exclamation. Honesty about what underperformed is required. Build writeups
   ONLY from committed numbers — invent nothing.

## Scope discipline
Work ONLY on the given session. End with cargo build+clippy+test, list changes, STOP.
```

---

# Appendix B — Claude Code execution plan (7 sessions)

| # | Session | Deliverable | Done when |
|---|---|---|---|
| 1 | multireactor | `sys/affinity.rs` + `multireactor.rs` (§3–§4) | conformance + 60s soak |
| 2 | io-uring | `io_uring.rs` purpose-built (§5) + `io-uring` dep | conformance + 60s soak (or kernel-N/A documented) |
| 3 | Definitive sweep | full 11-model sweep + scaling study (§6) | `bench/results/` populated, C10K recorded |
| 4 | Profiling | top-down + syscalls/req + ctx-switches (§7) | `bench/results/profiles/` committed |
| 5 | BENCHMARKS.md | the teardown from real numbers (§8–§9) | every claim cites a committed file; §8 rules pass |
| 6 | README + ARCHITECTURE | §10–§11 from real numbers + evidence/rejected-alternatives tables | §8 rules pass; 60-sec grasp above the fold |
| 7 | X strategy + draft + DoD | `docs/x-thread.md` (§12) + verify DoD §13 | strategy + sourced draft; DoD §13 each item reported |

io_uring (Session 2) is the heavy build — split at the accept/read/write boundary if context grows. Writing sessions (5–7) carry large context (they read all of `bench/results/`); keep them separate so each gets a clean window.

### Exact prompts (paste one per session; verify + commit before the next)

**Session 1**
> Read `CLAUDE.md`, `docs/specs/phase2-spec.md` §1–§4, and `phase1-spec.md` §4 (the `Reactor` API). Update `CLAUDE.md` per Appendix A. Then execute **Session 1 only**: implement `sys/affinity.rs` (§3) and `models/multireactor.rs` (§4) — shared-nothing, SO_REUSEPORT per reactor, pinned, reusing the Phase 1 `Reactor` unchanged. Add to the conformance suite; run a 60s soak. Do not touch `core`. Commit, run checks, list changes, STOP.

**Session 2**
> Read `CLAUDE.md` and `phase2-spec.md` §5. Execute **Session 2 only**: implement `models/io_uring.rs` purpose-built per §5 — raw `io-uring` crate, multishot accept, provided buffer rings, batched submission, completion flow feeding the unmodified `core::Connection`, kernel gate. Verify API against the installed crate docs. Add the dep; add to conformance; 60s soak or document kernel-N/A. Do not touch `core`. Split at the accept/read/write boundary if context grows. Commit, run checks, list changes, STOP.

**Session 3**
> Read `CLAUDE.md` and `phase2-spec.md` §6. Execute **Session 3 only**: run the definitive 11-model sweep (1/10/100/1000/10000) and the multireactor scaling study; regenerate all plots; record C10K results and resource curves. Commit fresh CSVs/dumps/plots to `bench/results/`. Modify no model. Report headline numbers and STOP.

**Session 4**
> Read `CLAUDE.md` and `phase2-spec.md` §7. Execute **Session 4 only**: capture top-down microarchitecture, context-switches, and syscalls/req for `epoll-et`, `multireactor`, `io-uring` (perf, or the documented fallbacks). Commit to `bench/results/profiles/`. Report the syscalls/req comparison and STOP.

**Session 5**
> Read `CLAUDE.md`, `phase2-spec.md` §8–§9, and every file in `bench/results/`. Execute **Session 5 only**: write `docs/BENCHMARKS.md` per §9, obeying the §8 Writing Standard — environment/methodology, threats-to-validity, headline table, embedded plots, the per-model template for all 11, the io_uring verdict, the C10K resource curves, surprises-and-corrections, the data-provenance table, the microVM bridge. Every number cites its committed file. Invent nothing. Before finishing, re-read the document against the 10 §8 rules and fix violations. Commit and STOP.

**Session 6**
> Read `CLAUDE.md`, `phase2-spec.md` §8, §10–§11, and `bench/results/`. Execute **Session 6 only**: rewrite `README.md` per §10 (headline number + table + plot above the fold; repository map; model index) and finalize `docs/ARCHITECTURE.md` per §11 (layering diagram, per-layer contracts, the "one frozen core / 11 models" evidence table, key design decisions + rejected alternatives). Obey §8. Re-read both against the §8 rules and fix violations. Commit and STOP.

**Session 7**
> Read `CLAUDE.md`, `phase2-spec.md` §12–§13, and `bench/results/`. Execute **Session 7 only**: write `docs/x-thread.md` — the strategy brief (§12.1) then the 7–9 post draft (§12.2) with every {placeholder} filled from committed numbers, obeying §8. Then verify the Phase 2 Definition of Done §13 item by item and report each. Commit and STOP. The repo is complete.
