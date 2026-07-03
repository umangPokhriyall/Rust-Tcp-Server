# PRINCIPAL REVIEW — Pre-Metal Cross-Repo Audit & Strategic Re-Alignment
*Issued by: Chief Systems Architect (Fable 5). 2026-07-02. Subordinate to NORTH-STAR.md. Read alongside chief-architect-directive.md and global-infrastructure-directive.md. This is the gate document before the single-session Latitude.sh validation sweep and portfolio distribution.*

---

## VERDICT UP FRONT

The portfolio is architecturally sound and the frozen invariants hold. **Do not provision the metal box yet.** Three concrete pre-metal defects will corrupt or waste the metal window if hit unprepared, and one of them (R1's hardcoded Intel `perf` metric group) will *error on sight* on EPYC. All three are laptop-fixable in under a session each. The strategic targeting is correct for mid-2026 with one reframe: the flagship headline is no longer cold-start latency (table stakes now) — it is the **measured anatomy** nobody publishes (UFFD tail, rtnl tax, state-persistence boundary, isolation-tier frontier).

Fix the three P0 items locally, re-run green, *then* burn the box.

---

# PHASE 1 — Global Constitutional Audit

**Result: PASS on all frozen cores and all Chief-Architect Directive upgrades. Zero freeze violations.**

## 1.1 Frozen-invariant verification

| Repo | Freeze anchor | State |
|---|---|---|
| Rust-Tcp-Server | `core` sans-IO (RequestParser / Connection / ConnAction) | Intact. No `core/` edits; drove all 11 models. (No git tag, but no diff surface — tag it before publish, see P1.) |
| Web3-Terminal | `book` @ `book-v1-frozen` | Tag present; six book files byte-identical; no `f64` in measured path; `unsafe` only in `sync`. |
| proctor | `proctor_core` @ `v0.1.0-core-frozen` | `git diff v0.1.0-core-frozen -- core/` **empty**. Epoch fencing enforced in `sched`, not `core` — freeze holds exactly as directive §1.1 required. |
| frost-ed25519-kit | all `frost-core` + `legacy` frozen | Held. One *authorized* post-freeze exception on record (P4 fuzz find: non-canonical point acceptance in `group.rs::from_compressed`, fixed + re-frozen). Legitimate and documented. |
| Coingate | `pre-idempotency` tag; CrashPointId set closed at 15 | Held. Black-box harness links no target crate (verified). |

## 1.2 Directive upgrade execution — all CONFIRMED locally

- **proctor §1.1 fencing:** `core::Epoch` type present (`id.rs:42`), `Lease.epoch` (`lease.rs:27`), epoch-fenced store writes, single `reclaim_expired` authority, `slow-zombie` test in `store/contract.rs` + `sim.rs`. `P_MIN = 0.02` hard sampling floor present (CLAUDE.md rule 5). Hypergeometric detection committed (`detection-family.csv`, `detection-divergence.csv`). Cross-referenced to Coingate's XAUTOCLAIM class — portfolio coherence intact.
- **frost §2.1 ROS:** `legacy/results/ros_forgery.txt` — **ℓ=256 sessions, forgery in 50.3 ms**, out-of-set proof committed. `RosOutcome::NoSolution` against FROST (structural, `ros_resistance.rs`). This is the most senior single artifact in the portfolio.
- **frost §2.2 identifiable abort:** `Error::Culprit(Identifier)` (`error.rs:31`), verification shares in `verify.rs`/`keygen.rs`.
- **frost §2.3 hedged nonce:** `d_i = H3(random32 ‖ encode(share))` (`sign.rs:79–85`).
- **frost §2.4 intermediate-first KAT:** `rfc9591_kat.rs` asserts binding-factor input → binding factors → group commitment → partials → final, in that order (stages 1/1b/2/3). Exactly the bisection ordering §2.4 mandated.
- **frost §2.5 identifier discipline:** zero + duplicate rejection at deserialization (`group.rs:178`, `:228`).
- **Coingate §3.1 exhaustive enumeration:** `harness/enumerate.rs` — every crash point (SelfTest excluded) × every schedule, deterministic; seeded interleaving sweep on top. Headline is the exhaustive form.
- **Coingate §3.2 in-progress key protocol / §3.4 RC / §3.5 reconciliation:** 409 + Retry-After lifecycle present (`idem.rs`, `idempotency_pg.rs`); whole proof at `READ COMMITTED`; reconciler is Invariant #5 (`oracles.rs:216`). Committed: **62/62 passed, 0 conservation violations, 1 send/withdrawal at RC**, plus a before/after showing a double-credit at RC in legacy.

**Sans-IO boundaries:** clean in all five. proctor `core` sans-IO+frozen; frost `frost-core` sans-IO (`#![forbid(unsafe_code)]`, no `solana-*` in crypto path, six-crate shipped graph); Coingate `idempotency/` compiles without a database; Web3 `book`/`sync`/`feed` float-free and heap-free in the hot path; R1 `core` drove three I/O backends unchanged.

---

# PHASE 2 — Pre-Metal Telemetry Lockdown

**Result: FAIL — the vendor-aware profiling plumbing is NOT committed locally in any of the three metal repos. This is the single highest-risk area and it is unprepared.**

The global-infrastructure-directive §5 names the AMD-counter correction "the one genuinely risky edit" and mandates it be pre-wired and human-gated, *not* improvised on the box. Current local state:

### P0-a — R1 `bench/profile.sh` still hardcodes Intel TMA (a live landmine)
`Rust-Tcp-Server/bench/profile.sh:267`:
```
perf stat -M TopdownL1,TopdownL2 -p "$SERVER_PID" ...
```
`TopdownL1`/`TopdownL2` **do not exist on AMD Zen 4** — this errors or silently misleads on EPYC 9254. There is **no vendor-detect, no `PERF_METRIC_GROUP` plumbing** in the script. The phase3-spec.md documents the correction perfectly (§A.6 `perf list metricgroups` discovery, vendor branch, AMD retiring/bad-spec/frontend/backend mapping) — but the spec is not the script. The doc is right; the executable is wrong.

### P0-b — R2 harness prep session never ran locally
`Web3-Terminal/bench/src/benches/profile.rs` has **no vendor logic**; there is **no `metal_run.sh`**, no `PRODUCER_CORE`/`WRITER_CORE`/`physcpubind` honoring, no `perf c2c` wrapper. The `bare-metal-rerun-spec.md` checklist explicitly says these are *laptop pre-commit* items ("commit the harness-only changes on a branch and push to main … testable locally on the non-perf paths"). That session is unstarted. The frozen `book`/`feed`/`sync` need no edits — this is pure harness plumbing that should exist before the box.

### P1-c — proctor is closest but the profiler wrapper is missing
NUMA/CCD-aware pinning, configurable N-grid, and platform-keyed results **landed** (last commit `45fb423`). `profile-placement` subcommand exists (`main.rs:48`). But there is **no `metal_run.sh`** and no committed `PERF_METRIC_GROUP` consumer wiring the vendor-detect around `perf`. Least risky of the three, but still not launch-ready.

### What is legitimately deferred to the box (do NOT pre-guess)
Only the **exact AMD metric-group string / raw PMC event names** — those depend on the provisioned kernel and must be read from `perf list metricgroups` by the human on the box (the §A.6 comprehension gate). The *branching logic and env plumbing* around that string is what must be pre-committed. Right now the string is deferred **and so is the plumbing** — that is the error.

### Pinning / NUMA isolation — CONFIRMED correct where present
R1 `scaling.sh`/`c10k.sh`/`profile.sh` honor `numactl --cpunodebind/--membind` for disjoint server/loadgen nodes; proctor `orchestrate.rs` derives NUMA-aware per-role core plans with disjoint physical cores per worker, spilling across sockets not hyperthread siblings. The NPS2 → 2-node CCD-isolation mapping from global-infra §2.2 is respected in design. Verify `numactl --hardware` shows 2 nodes *after* setting NPS2 in the Latitude console (provisioning step, on-box).

---

# PHASE 3 — Distribution Artifact Verification (Rule 8)

**Result: PASS on voice/fluff; ONE traceability defect (P0-d) + minor numeric drift.**

### Marketing-language scan: clean
Zero hits for blazing/incredible/seamless/game-changing/emoji/exclamation across all READMEs, BENCHMARKS, and x-threads in all five repos. The voice is uniformly declarative-sourced. The three x-threads reviewed (R1, R2, frost) each lead with an arresting *engineering finding*, not portfolio commentary:
- **R1:** "11 models behind one trait, benchmarked honestly, open-loop CO-corrected" → the honest io_uring-sheds-load verdict and the reactor-is-not-zero-cost crossover.
- **R2:** "the obviously optimal data structure lost by ~254× on real market data" → memory-hierarchy-meets-book-width, the ranking fully inverts.
- **frost:** "I shipped a threshold signer, then forged it on purpose — a valid signature on a never-signed message in ~49 ms."

These are the correct high-signal openers. Keep them.

### P0-d — R1 citation paths are broken (Rule 8 traceability failure)
`Rust-Tcp-Server/docs/BENCHMARKS.md` (11 refs) and `docs/x-thread.md` cite `bench/results/c10k_summary.csv`, `bench/results/profiles/summary.csv`, etc. — but every results file was moved to **`bench/results/_archive-laptop-i5-1135G7/`**. A Principal reviewer clicking any source link gets a 404. The numbers are real and *do* exist at the archived path; the citations were orphaned when the laptop set was archived ahead of the metal run. Rule 8 says "every quantitative claim cleanly traces to a verifiable baseline data file" — right now it traces to a dead path. This must be reconciled: either the metal run restores canonical top-level paths (per phase3-spec §A intent) or the docs are repointed to the archived path. Until one happens, R1 is **not** distribution-ready even though it reads as complete.

### Minor — frost "~49 ms" vs committed 50.3 ms
The frost x-thread and README say "~49 ms"; the committed `ros_forgery.txt` currently reads **50.316832 ms**. The file self-documents per-run variance and states "the forgery's SUCCESS, not its speed, is the result," so "~49 ms" is defensible as an approximate. Recommend normalizing the prose to "~50 ms" or "tens of ms" so a pedantic reviewer diffing the file finds no gap. Cosmetic, not load-bearing.

### R2 / Coingate / proctor traceability: intact
R2 thread numbers (btree 22.30 / sorted 13.51 / flat 8.85 ns/event) trace exactly to `throughput.csv`. Coingate headline (62/62, 0 violations, RC) traces to `chaos/results/summary.md` + `before-after.md`. proctor results are platform-keyed under `results/laptop-i5-1135g7/` by design and cite their CSVs. Note proctor has **no README/x-thread yet** — that is correctly deferred to Phase 8 (post-metal), not a defect.

---

# PHASE 4 — Mid-2026 Market Context & Strategic Re-Alignment

## 4.1 The landscape as it actually sits, mid-2026

The secure-code-execution layer is now a contested market, not a frontier. Grounded reads:
- **E2B** — Firecracker microVMs, ~80 ms same-region cold start / ~200 ms p50 cross-region; scaled 40k→15M sandbox runs/month in 12 months. The Firecracker reference point.
- **Daytona** — pivoted dev-environments → agent infra; **sub-90 ms** cold start via Docker containers (Kata optional). Fastest create, *weakest isolation class* — the persistent-state / workspace story is theirs.
- **Modal** — gVisor user-space kernel, the only one holding **GPUs** (T4→H200), serverless scale-to-zero, Python IaC. Owns the GPU-serverless quadrant.
- **AWS Lambda MicroVMs** — now GA ("isolated sandboxes with full lifecycle control"), Firecracker + SnapStart, the enterprise baseline. Snapshot restore ~1–5 ms vs ~200 ms boot, but SnapStart's real bottleneck is **on-demand paging page faults on tiered memory** — the exact tail the flagship is built to expose.
- **Firecracker UFFD** — `/dev/userfaultfd` is the default handler on kernel ≥6.1; the lazy-restore mechanism is now mainstream, which means its *cost distribution* is the open question, not its existence.

**Consequence (confirms directive §9.2):** "sub-100 ms cold start" is table stakes. Everyone ships it. It is not a differentiator and must not be the pitch.

## 4.2 The refined Flagship Teardown methodology — exploit what nobody publishes

Keep the §6 isolation-tier-frontier framing (Firecracker/E2B vs gVisor/Modal vs container/Daytona vs raw CH/FC floor). Add four elite signals for the mid-2026 reality:

1. **UFFD lazy-restore tail (the signature artifact).** Restore-time looks instant; cost shifts into the first-N-operation page faults post-resume. Measure restore-time **and** the latency distribution of the first N guest ops, **eager vs `/dev/userfaultfd` lazy**, with the fault trace. "Restore is 8 ms but p99 of first-exec is +40 ms under lazy paging, here's why" — this is coordinated-omission applied to memory. Directly targets the published-nowhere SnapStart bottleneck.
2. **State-persistence boundary (the Daytona axis).** Daytona's whole thesis is persistent workspaces. Add a measured axis: **snapshot/restore of a warm, stateful sandbox** — resume a VM with a populated page cache / running runtime vs cold, and quantify the persistence dividend and its memory cost. This is where microVM restore *beats* container cold-create and where the honest "containers win pure cold-start, microVMs win warm-restore-with-isolation" frontier lives.
3. **Isolation-overhead matrix (the tier discriminator).** Inside the guest, identical static binary: syscall latency (getpid loop), `clock_gettime`, fresh-page fault cost, small-file open/read/write, CPU loop, memory-bandwidth pass. This is what separates gVisor's syscall-interception tax from microVM near-native syscalls from container native. The frontier plot (isolation class × syscall overhead × cold start) is the screenshot artifact.
4. **rtnl network tax under burst.** Per-VM TAP + iproute2/iptables serializes on the rtnl lock under concurrent creates — a hidden cold-start floor. Publish the serialization curve; design the fast tier with **no per-VM TAP** (guest egress via host vsock proxy), TAP as opt-in slow tier.

## 4.3 Mapping Rust / TypeScript / Bun payloads into the sandbox (full-stack mastery signal)

The core runtime payloads (Rust, TS, Bun) are the *guest workload*, and demonstrating them inside the microVM proves the sandbox executes real agent code, not a toy `echo`:
- **Guest agent shim (Rust):** the in-guest vsock agent that receives exec requests and streams stdout back through the SPMC ring is Rust — it is Repo 1 + Repo 2 discipline living on the guest side of the KVM boundary. This is the natural home for the Rust identity.
- **TS / Bun as the executed payload:** ship a golden rootfs with a Bun runtime pre-initialized in the snapshot. The headline `T_first_exec` then measures **"API call → first byte of a real Bun/TS agent script's output."** Bun's fast cold-start *inside* the microVM stacks two cold-starts (VM restore + runtime init) — decompose both stages. This is the honest full-stack number: not "Firecracker boots in Xms" but "a TS agent produces its first byte in Yms end-to-end, of which restore is A, runtime init is B, first-exec page-fault tail is C."
- **The mapping to demonstrate:** provider guest environments (E2B/Modal/Daytona) all run exactly these runtimes. Matching their guest surface (Node/TS/Bun/Python) on your floor is what makes the frontier comparison *apples-to-apples* and forecloses the "your floor runs a static C binary, theirs runs a real runtime" objection. Commit one static-binary cell (pure isolation floor) **and** one Bun-runtime cell (real-workload floor); publish both, labeled.

## 4.4 The India→US WAN problem — measure, report, isolate (the central honesty problem)

The box is a US metro; engineering origin is India. This is a ~200–250 ms RTT confound that will swamp every number if not surgically isolated. The directive §6.5 handles the *provider* asymmetry; here is the precise protocol for the *operator-origin* asymmetry:

**Rule: nothing latency-sensitive is ever measured from India. The box measures itself and the providers; you only SSH in to launch.**

1. **Separate the operator plane from the measurement plane.** Your laptop in India is a *control terminal* — it SSHes in, launches `metal_run.sh`, and pulls results (`git push` from the box / `scp`). It is **never** in any measured path. Every latency figure — the raw floor, the provider sweep, the create-storm — originates from a client process *on the box itself* or from a cloud client *adjacent to the provider region*, never from your keyboard.
2. **The raw floor is loopback, on-box.** Cloud Hypervisor / Firecracker floor: client and VMM on the same Latitude box, `127.0.0.1` / vsock. Zero WAN. This is the true execution floor and it has no India component by construction.
3. **Provider sweeps run from a US-adjacent client, not from India.** Run the E2B/Modal/Daytona open-loop arrivals from the Latitude box (US metro) or a cloud VM in the provider's region. Document which region for each provider. If you *must* characterize your own India→US path (e.g., to state the developer-experience latency), report it as a **separate, explicitly-labeled `operator_wan` measurement**, never folded into a provider or floor number.
4. **Report raw AND floor-subtracted, never floor-subtracted alone.** For each provider: measure the null-op API floor (auth'd no-op / trivial GET) from the same client, report both the raw distribution and the (raw − null-op-floor) distribution. The floor-subtracted number isolates *their* VMM+scheduler from *the shared network path*. Present both side by side (directive §6.5).
5. **Threats-to-validity must name the origin.** State plainly: "measurement client ran in `<US region>`; the India operator origin is a control channel only and appears in no reported latency except the labeled `operator_wan` row." That sentence converts a credibility hole into a rigor signal.

**One-line WAN isolation formula for the writeup:**
`T_true_execution = T_first_exec_measured − T_null_op_floor(same client, same region)` — and every reported floor number is on-box loopback with `T_null_op ≈ 0`.

## 4.5 Target companies, OSS projects, and proof-first outreach

**Companies (the first readers are the target):**
| Target | Why the portfolio qualifies | Lead artifact |
|---|---|---|
| **E2B** | Firecracker microVMs — the flagship *is* their stack. | The isolation-tier frontier + UFFD tail; the vsock throughput teardown. |
| **Modal** | gVisor + serverless scale; they respect measurement. | The isolation-overhead matrix (gVisor syscall tax quantified respectfully). |
| **Daytona** | Persistent-workspace thesis. | The state-persistence-boundary axis + warm-restore dividend. |
| **Fly.io** | Firecracker fleet at metro scale; the reactor/control-plane story is theirs. | R1 SO_REUSEPORT multireactor + the create-storm curve. |
| **AWS Lambda / Firecracker team** | SnapStart page-fault bottleneck is your signature artifact. | The UFFD lazy-restore tail with fault traces. |
| **Cloudflare (Workers/containers), Vercel (Sandbox)** | Edge isolation + cold-start obsession. | The CO-correct cold-start methodology. |

**OSS (both Rust, both flagship-adjacent — per directive §7):**
1. **Cloud Hypervisor `virtio-blk` io_uring path + rust-vmm shared crates (`virtio-queue`, `vm-memory`).** Your Repo 1 Phase 2 io_uring discipline (drop-in ~1.06× vs purpose-built ~2×) maps directly. Entry: open-loop TMA-profiled block-path benchmark on the Phase 3 metal box (PMU already paid for) → measured issue with profile attached → patch.
2. **Firecracker vsock device path (UDS-backed connection muxer).** Literally the host↔guest path the flagship lives on — every hour is dual-purpose flagship research. Firecracker runs perf A/B in CI, so measured submissions get serious maintainer attention. Rigorous vsock throughput/latency benchmark (open-loop, distributions, TMA) → measured issue → targeted patch (buffer sizing, copy elimination).

**Anti-targets (drop explicitly):** gVisor (Go), vLLM (Python/CUDA) — dilute the Rust/systems identity.

**Proof-first outreach strategy (NORTH-STAR §7):**
- **The artifact is the opener, never a request.** Outreach leads with the link to a specific finding, not "I'm looking for a role."
- **Sequence:** publish the x-thread → the single best plot as a day+1 quote-tweet → engage technically with *their* engineers' posts over weeks (substance on their threads, never cold-@ to borrow audience) → then, if at all, a one-line DM/email to a named Principal that is *only* "I measured X on your stack, thread here, thought you'd want the fault trace." The measured issue on their own OSS repo **is** the outreach — a well-profiled Firecracker vsock issue reaches the exact engineers you want, under your own name, with zero pitch.
- **The two OSS submissions are the distribution channel, not polish.** They stay on the critical path even under schedule squeeze.

---

# PHASE 5 — Next-Gen Macro Roadmap & Flagship Blueprint

## 5.1 Prioritized pre-metal modification checklist (do these locally, in order, before provisioning)

**P0 — blockers; the metal window is wasted or corrupted without them:**
1. **R1 `bench/profile.sh`:** replace the hardcoded `perf stat -M TopdownL1,TopdownL2` with the vendor-detect branch (`grep -qi amd /proc/cpuinfo`) + `PERF_METRIC_GROUP` env plumbing. Leave the AMD group *string* as a required env var filled on-box from `perf list metricgroups`. Keep strace/`/proc` passes unchanged (vendor-neutral). Build/clippy/test green on the non-perf paths locally.
2. **R2 harness prep session:** land the `bare-metal-rerun-spec.md` laptop pre-commit items — vendor-detect profiler + `PERF_METRIC_GROUP`, `PRODUCER_CORE`/`WRITER_CORE`/reader-core `numactl --physcpubind` honoring, `perf c2c` wrapper, `metal_run.sh`. No `book`/`feed`/`sync` edits. Testable locally on non-perf paths.
3. **R1 doc citation reconciliation:** repoint BENCHMARKS.md + x-thread.md source links from `bench/results/*.csv` to the actual archived path (`bench/results/_archive-laptop-i5-1135G7/…`), OR pre-commit the metal-run doc-rewrite plan so the metal session restores canonical paths. Either way, no distribution until every source link resolves.

**P1 — do before the box but not window-blocking:**
4. **proctor `metal_run.sh` + `PERF_METRIC_GROUP` wrapper:** wire the vendor-detect around `perf` invoking `profile-placement` + the crypto path. Pinning/grid/platform-keying already landed.
5. **R1 freeze tag:** apply a `v*-core-frozen` git tag to make the R1 core freeze auditable like the other four repos (currently freeze is real but untagged).
6. **frost prose normalization:** "~49 ms" → "~50 ms" / "tens of ms" across README + x-thread to match committed `ros_forgery.txt`.

**On-box, human-gated (NOT pre-doable):**
7. Set NPS2 in the Latitude console; confirm `numactl --hardware` shows 2 nodes.
8. Run `perf list metricgroups`; the **human** identifies the AMD Zen 4 pipeline-utilization group(s) / raw PMC events and can state what AMD backend-bound means vs Intel *before* publishing any pipeline claim (the non-delegable §A.6 comprehension gate).
9. Re-confirm the R2 "SortedVec is memory-bound, not speculation-bound" finding against the **AMD backend-bound counter specifically** — the qualitative result should hold; the proving counter is now an AMD event.

## 5.2 Flagship Sandbox assembly architecture (how the 5 modules merge)

```
 clients ──► [R1] SO_REUSEPORT multireactor front end (pinned reactors, one per CCD-core)
              │   POST /sandboxes  ── [R5] Idempotency-Key intake (in_progress→completed SM, 409+Retry-After)
              │   key table + transactional outbox → dispatch queue   [R5]
              ▼
            [R3] SCHEDULER: least-loaded placement over warm pool,
              lease + EPOCH fencing token, single reclaim authority,
              Little's-law-sized backpressure (shed at saturation)
              ▼
            VMM SUPERVISOR (one per microVM, owned by a reactor shard):
              spawn jailer/firecracker
                ├─ Firecracker API UDS  ──┐
                ├─ host-side vsock fds   ──┤  ALL registered into ONE
                ├─ pidfd (VMM exit)      ──┤  epoll-ET / io_uring loop   [R1 external-fd seam]
                └─ timerfd (boot ddl)    ──┘  [R1 net-new timer wheel]
              guest stdout/stderr ──► [R2] SPMC ring (lossy live path, seq-gap detect)
                          ├─► live WebSocket subscribers
                          ├─► durable log sink  [R2 §4.2.1 distinct LOSSLESS path — do NOT bend the ring]
                          └─► metrics consumer
              [R2] VmStateSnapshot seqlock cell ◄── API GET reads without touching the reactor
 ═══════════════ KVM boundary (AMD-V/SVM, /dev/kvm, no nested virt) ═══════════════
              guest: minimal kernel + init shim + [Rust vsock agent] + [Bun/TS runtime in golden snapshot]
              result commit carries lease epoch → [R3] store rejects zombie-VM writes  (fencing = R5's XAUTOCLAIM class)
              secrets transiting into guest ──► [R4] split-trust broker (zeroized, hedged, identifiable-abort)
```

**Role mapping, stated once:** R1 = the event substrate (every host-side fd of the KVM boundary lands in its loop) + control-plane front end + the cold-start benchmark methodology. R2 = the output plane (SPMC ring) + state-read plane (seqlock) + the profiling discipline. R3 = placement/lease/reclaim brain + epoch-fencing safety. R4 = secret broker for credentials transiting guests. R5 = the intake contract (one VM per job under retries/redelivery — at-most-once-effect lifted from money to VMs).

**Flagship Phase 0 = the §4 seams + a single-VM vertical slice** (one create → one Firecracker → one Bun/TS exec → output through the ring) with **stage-timestamped cold start from day one**: intake → dedup → placement → (clone rootfs ‖ network) → VMM spawn → restore/boot → agent ready → first exec byte. The decomposition is the product; the headline is its sum. Scheduler, idempotent intake, and secret broker bolt on in later phases — each already built and measured in its home repo. Draft the flagship kickoff brief only after Repos 3–5 close (they have; proctor Phase 7 metal + close-out and Coingate are the gating items — see §5.4).

## 5.3 Known flagship seams to budget (from directive §4, confirmed against code)

- **R1 external-fd ingestion:** reactor acquires fds only via `accept()` today. Flagship needs `register_fd(fd, driver)` for Firecracker API UDS, host vsock, pidfd, timerfd. Audit whether `reactor` internals assume accept-origin fds beyond registration — budget the answer.
- **R1 connection-driver genericity:** event loop dispatches into HTTP `Connection`/`ConnAction`. If hard-wired to HTTP types rather than a trait boundary, Phase 0 includes a reactor-genericization refactor — known, not discovered.
- **R1 timer facility:** per-socket timeouts exist; a control plane supervising thousands of VMs needs `timerfd` + timer wheel/heap in the same epoll set. Net-new component.
- **R2 durable-sink losslessness:** the ring is correctly producer-never-blocks/lossy for live subscribers; the durable stdout log MUST be a distinct lossless path (dedicated consumer with measured headroom + loud failure, or producer-side spill). Do not bend the ring.
- **R2 bytes-oriented reserve API + generic seqlock payload:** confirm the ring reserve API is bytes-oriented (guest stdout is variable-length chunks; current `push([u64; W])` is fixed-width — flagship Phase 0 item) and the seqlock cell is generic over any fixed-size POD (`VmStateSnapshot` will be defined to fit; current `Seqlock<TopOfBook>` is typed — genericize or re-instantiate).

## 5.4 Assembly-order note
Per STATE.md/directive doctrine: the flagship kickoff brief is drafted only after Repos 3–5 close. frost (R4) and Coingate (R5) are effectively closed; proctor (R3) has its Phase 7 metal run + Phase 8 close-out pending, and R1 has its Phase 3 metal run pending (the hard gate: no flagship performance claim publishes before the R1 Phase 3 TMA run completes, because the cold-start methodology inherits its credibility from that run). **Co-schedule** the OSS profiling (CH virtio-blk / FC vsock TMA) into the same box windows — the PMU is already provisioned.

## 5.5 META-PROMPT TEMPLATE — Flagship Kickoff Brief (fill after the metal sweep)

Copy this into the flagship kickoff-brief drafting session. Bracketed `[[SLOT: …]]` markers are the exact injection points for raw Latitude.sh numbers; do not fabricate — leave a slot empty and marked `PENDING-METAL` until the committed CSV exists.

```markdown
# FLAGSHIP KICKOFF BRIEF — microVM Agent Sandbox
*Subordinate to NORTH-STAR.md. Drafted after Repos 1–5 close and the Latitude metal sweep. Every number traces to a committed file on the box run; invent nothing.*

## 0. Rig of record (from the metal sweep — METHODOLOGY.md)
- SKU / region: [[SLOT: m4.metal.large | rs4.metal.large, <US metro>]]
- CPU / topology: EPYC [[SLOT: 9254 (24c/4 CCD) | 9354P (32c/8 CCD)]], NPS[[SLOT: 2|4]] → [[SLOT: N]] NUMA nodes, per-CCD private L3 [[SLOT: 32 MiB]]
- Kernel / microcode: [[SLOT: uname -r]] / [[SLOT: microcode rev]]  ·  governor: amd-pstate performance
- /dev/kvm present (AMD-V/SVM), no nested virt  ·  numactl --hardware: [[SLOT: paste]]

## 1. The claim (headline = anatomy, NOT a single latency)
"The measured anatomy of microVM sandbox cold start, with an open reference implementation hitting the raw Cloud Hypervisor/Firecracker floor on rentable EPYC metal."
- Internal bar (not the pitch): p99 T_first_exec < 100 ms.
- Stage-decomposed cold start (the product): intake→dedup→placement→(clone‖net)→spawn→restore/boot→agent-ready→first-byte.

## 2. Baselines inherited from the home repos (metal-run values)
- Control-plane front end (R1 Ph3): top throughput [[SLOT: RPS @ C]], syscalls/req io_uring vs epoll-et [[SLOT: x vs y]], multireactor tail [[SLOT: p50/p99]], AMD Zen4 pipeline buckets [[SLOT: retiring/bad-spec/frontend/backend %]]. Source: [[SLOT: R1 bench/results/metal-*/…]]
- Output plane (R2 Ph4/6/7/9): apply-loop ns/event per impl [[SLOT]], seqlock/SPMC perf c2c HITM [[SLOT]], AMD backend-bound for SortedVec [[SLOT]]. Source: [[SLOT: R2 bench/results/perf/…]]
- Scheduler (R3 Ph7): dispatch p99 decomposition (decision µs vs Redis RTT) [[SLOT]], detection vs hypergeometric×(1−FAR) [[SLOT]], N=24 disjoint-core scaling [[SLOT]]. Source: [[SLOT: proctor results/metal-*/…]]

## 3. Cold-start anatomy — the metal measurements (the differentiators)
- Boot vs restore: full boot [[SLOT: ms]] | snapshot restore [[SLOT: ms]]. Source: [[SLOT]]
- UFFD lazy-restore tail: restore [[SLOT: ms]]; first-N-op p99 eager [[SLOT]] vs /dev/userfaultfd lazy [[SLOT]]; fault trace [[SLOT: file]]. Source: [[SLOT]]
- State-persistence dividend: warm-restore (populated page cache) [[SLOT]] vs cold [[SLOT]]; memory cost [[SLOT]]. Source: [[SLOT]]
- rtnl network tax: TAP-create serialization curve under B concurrent creates (B=1/10/50/100) [[SLOT]]; vsock-proxy fast-tier vs TAP slow-tier [[SLOT]]. Source: [[SLOT]]
- Rootfs provisioning: O(1) overlay/reflink clone time [[SLOT]]. Source: [[SLOT]]

## 4. Guest workload floor (full-stack, apples-to-apples with providers)
- Static-binary isolation floor (pure): T_first_exec [[SLOT: ms]]. Source: [[SLOT]]
- Bun/TS real-runtime floor: T_first_exec [[SLOT: ms]] = restore [[SLOT]] + runtime-init [[SLOT]] + first-exec page-fault tail [[SLOT]]. Source: [[SLOT]]
- Isolation-overhead matrix (in-guest, identical binary): getpid [[SLOT]], clock_gettime [[SLOT]], page-fault [[SLOT]], file open/read/write [[SLOT]], CPU loop [[SLOT]], mem-bw [[SLOT]]. Source: [[SLOT]]

## 5. Teardown frontier (providers vs floor — raw AND floor-subtracted)
- Measurement client region: [[SLOT: US region]]. Operator origin (India) = control channel only; operator_wan row = [[SLOT: labeled separately]].
- Per provider {E2B, Modal, Daytona, AWS Lambda MicroVM}: pool-regime T_first_exec [[SLOT]], exhaustion-regime (B swept) [[SLOT]], null-op floor [[SLOT]], floor-subtracted [[SLOT]], isolation class + in-guest overhead [[SLOT]]. Sources: [[SLOT]]
- The frontier plot: isolation class × cold start × syscall overhead. File: [[SLOT]]

## 6. Threats to validity (mandatory)
- WAN/origin: [[fixed sentence — client region, India as control-only, operator_wan labeled]].
- Provider warm-pool state unobservable; you measure their product (gateway+billing+scheduler) vs your VMM floor — platform overhead, not engineering deficiency. Respectful framing (these are hiring targets).
- Single-socket intra-NUMA caveat (Infinity Fabric, not inter-socket) — more representative of a single-host fleet, state it.
- AMD Zen4 pipeline analysis is the architectural counterpart to Intel TMA, NOT Intel TMA relabeled.

## 7. Phase plan
- Phase 0: §4 seams (reactor external-fd + genericity, timer wheel, ring lossless-sink, bytes reserve API, generic seqlock) + single-VM vertical slice with stage timestamps.
- Later phases bolt on R3 scheduler, R5 idempotent intake, R4 secret broker — each already measured in its home repo.
- Comprehension gates per NORTH-STAR §4 — non-delegable.
```

---

## CLOSING — the one-paragraph state for the human

Frozen cores and every directive upgrade are intact; the portfolio is architecturally coherent and reads as senior work. The gate to the metal box is three laptop-fixable P0s: R1's `profile.sh` still runs Intel `TopdownL1` (will error on EPYC), R2's vendor-aware harness prep never landed, and R1's doc source-links point at an archived path that 404s. Fix those, re-run green, provision one `m4.metal.large` with NPS2, and the metal window pays for R1 Ph3 + R2 profiling + R3 Ph7 + the OSS TMA in a handful of co-scheduled hours. The strategic reframe for mid-2026 is settled: cold-start latency is table stakes; the flagship sells the *anatomy* — UFFD tail, state-persistence boundary, isolation-tier frontier, rtnl tax — with a Rust guest agent and a real Bun/TS workload floor, measured on-box with India held strictly to the control channel. Distribution and the two Rust OSS submissions (Cloud Hypervisor virtio-blk, Firecracker vsock) are the critical path, not polish.
```
