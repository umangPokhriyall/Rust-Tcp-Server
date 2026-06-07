# Rust-Tcp-Server — Phase 3 Specification (Bare Metal): The Definitive Run with Native TMA

**Companion to:** `kickoff-brief.md`, `phase0/1/2-spec.md`, `NORTH-STAR.md`, `STATE.md`.
**Scope:** re-run the existing benchmark suite on a single **2-socket x86_64 bare-metal** EC2 instance, regenerating the identical file set with confound-free numbers, a true C10K, and — new — native Top-Down Microarchitecture Analysis (TMA) for **all eleven models**. The metal PMU closes the one gap the laptop documented as omitted. No new models, no new harness logic beyond NUMA pinning and extending the profiler.
**Audience:** the human operator (runbook). Claude Code re-enters only at §4 (one small code change) and §11 (doc update).

---

## 0. Why bare metal, why one host, and the human/Claude-Code split

The laptop run carried three confounds (single-host loadgen contention, an 8000-connection cap, no `perf`/PMU). The strategy: a single 2-socket metal box resolves all three at once — **socket-level NUMA isolation** stands in for two hosts, **256 GiB** clears the commit limit for a true C10K, and **direct PMU access** (only available on metal) delivers real TMA. The residual cost — loopback instead of a real NIC — is purpose-appropriate (a sandbox host↔guest path is local) and orthogonal to every load-bearing result here, so it is a documented minor caveat, not an asterisk on the claims.

**The split (NORTH-STAR §4):** the benchmark *run* is measurement → the human executes documented commands. Claude Code touches this phase twice: a small *code* change to add NUMA pinning and extend the profiler (§4, on the laptop, before metal), and the final *documentation* update from the new numbers (§11).

---

## 1. What this run fixes — and what it will not

**Resolves:**
- **Native TMA top-down for all 11 models** (retiring / bad-speculation / frontend-bound / backend-bound). The §7 gap is *closed*, not documented-around.
- **Server/loadgen contention** → eliminated by pinning the server to NUMA node 0 and loadgen to node 1 (disjoint cores, L3, and memory controllers).
- **True C10K at 10 000 connections** (256 GiB + `overcommit=1` + small loadgen stacks); a real C=10000 sweep rung, no sentinels.
- **Stable clocks** under a `performance` governor (settable on metal), no thermal throttling.
- **A wide multireactor scaling curve** (workers 1→64 physical cores).

**Will NOT change (state up front):**
- **Single-ring io_uring still sheds above C≈1000** — a property of one ring on one thread, not the host. The §8 verdict stands, cleaner.
- **syscalls/req stays ~4.0 (epoll-et) / ~2.0 (io_uring)** — a per-event property; reproducing it on different silicon is a reproducibility win.
- **multireactor ctx-switches/req should now fall toward ~1.0** (loadgen off the server's cores) — call this out as predicted-then-confirmed; it validates the laptop caveat.

**Residual caveat (document, do not hide):** loopback, single host. Mitigated by two-socket NUMA isolation; the one shared resource that remains is the inter-socket interconnect carrying the loopback payload between node 1 (loadgen) and node 0 (server). Minor, and aligned with the sandbox-host purpose. Absolute latencies are loopback latencies (no NIC/RTT).

---

## 2. Instance selection

**Primary: `c6i.metal`** — 128 vCPU / **64 physical cores across 2 Ice Lake sockets** / 256 GiB / ~$5.44/hr → ~18 h on $100. Two sockets are the requirement, not a luxury: the entire single-host strategy depends on isolating loadgen from the server, which needs two NUMA nodes.

**Premium: `c7i.metal-48xl`** — 192 vCPU / 96 physical cores / 2 Sapphire Rapids sockets / 384 GiB / ~$8.57/hr → ~11.7 h on $100. Choose this for the newest µarch (richer TMA model, AMX), more scaling cores, and more headroom. Either works.

**Do NOT use `c7i.metal-24xl`** despite it being a candidate: it is **single-socket** (48 cores, one NUMA node), so server and loadgen share L3 and memory controllers no matter how they are pinned — which contaminates exactly the backend-bound/cache TMA numbers this run exists to capture. A single socket defeats the strategy.

Verify live pricing before launch (figures approximate). Region: pick one with the instance in stock (US East/West, EU); on-demand, never spot (a reclaim mid-run corrupts the dataset).

**Rejected alternatives (record in BENCHMARKS/ARCHITECTURE):**
- *Two virtualized hosts:* real network + separation, but no PMU → no TMA. Trades away the gap-closer.
- *Two bare-metal hosts:* the only literally-zero-asterisk option (real network + PMU + separation), ~11.7 h on 2× c7i.metal-24xl within $100 — but 2× cost and SSH orchestration for a marginal gain over NUMA isolation, given loopback suits the purpose. Recorded as the alternative; not chosen.
- *Single-socket metal:* shared L3 pollutes server TMA. Rejected (this is why not c7i.metal-24xl).

---

## 3. Topology (single host, NUMA-isolated)

One bare-metal box. Server process pinned to **NUMA node 0** (`numactl --cpunodebind=0 --membind=0`); loadgen pinned to **node 1** (`--cpunodebind=1 --membind=1`); traffic over loopback (`127.0.0.1`). Disjoint cores, disjoint last-level cache, disjoint memory controllers — the cleanest server/loadgen isolation achievable without a second machine. No SSH, no two-host harness change, no placement group. Confirm the topology after boot with `lscpu | grep -i numa` and `numactl --hardware` (expect 2 nodes).

---

## 4. Step 0 (one-time, on the laptop, before metal): NUMA pinning + full-suite TMA

Two small *code* changes — appropriate for Claude Code, done and committed on the laptop first (the non-perf paths are testable locally; the perf path is validated on metal). This replaces the two-host/SSH change from earlier drafts (not needed — everything is one box now).

> Read `CLAUDE.md` and `docs/specs/phase3-spec.md` §3–§4. Make two minimal harness changes, altering no benchmark parameters or model code:
> (1) **NUMA pinning.** Have `bench/run.sh`, `bench/c10k.sh`, `bench/scaling.sh`, and `bench/profile.sh` honor optional env `SERVER_NUMA_NODE` and `LOADGEN_NUMA_NODE`: when set, prefix the server process with `numactl --cpunodebind=$SERVER_NUMA_NODE --membind=$SERVER_NUMA_NODE` and the loadgen process with the loadgen node. Unset = current behavior, byte-identical.
> (2) **Full-suite TMA.** Extend `bench/profile.sh` to (a) cover **all 11 models**, not just three; (b) add a `perf stat -M TopdownL1,TopdownL2` capture (a dedicated steady-state run per model — `perf -p <server_pid> -- sleep <dur>` under a fixed `loadgen` load), writing `bench/results/profiles/perf_<model>.txt`, **in addition to** the existing strace (syscalls/req) and `/proc` (ctx-switches/req) passes; (c) add one TMA capture per signal model under C10K-level load (`perf_<model>_c10k.txt`). Keep the strace and ctx passes exactly as they are. The throughput sweep, c10k, and scaling runs must remain **perf-free** — TMA never wraps them (perf overhead would corrupt those numbers).
> Default behavior with no env vars set must be unchanged. Test the non-perf paths locally; the perf path will be validated on the metal host. Commit. STOP.

---

## 5. Provision (the single host)

Latest **Ubuntu 26.04 LTS** AMI via SSM (never stale):

```bash
REGION=us-east-1
AMI=$(aws ssm get-parameters --region $REGION \
  --names /aws/service/canonical/ubuntu/server/26.04/stable/current/amd64/hvm/ebs-gp3/ami-id \
  --query 'Parameters[0].Value' --output text)
```

`provision.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
sudo apt-get update -y
sudo apt-get install -y build-essential git python3 python3-matplotlib \
     linux-tools-generic linux-tools-$(uname -r) strace numactl cpufrequtils
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

sudo tee /etc/sysctl.d/99-bench.conf >/dev/null <<'EOF'
vm.overcommit_memory=1
fs.file-max=4194304
net.core.somaxconn=65535
net.ipv4.tcp_max_syn_backlog=65535
net.core.netdev_max_backlog=65535
net.ipv4.ip_local_port_range=1024 65535
net.ipv4.tcp_tw_reuse=1
kernel.perf_event_paranoid=-1
EOF
sudo sysctl --system
sudo tee /etc/security/limits.d/99-bench.conf >/dev/null <<'EOF'
* soft nofile 1048576
* hard nofile 1048576
EOF

# stable clocks for reproducible TMA (metal allows governor control)
sudo cpupower frequency-set -g performance || true

git clone https://github.com/umangPokhriyall/Rust-Tcp-Server.git
cd Rust-Tcp-Server && source "$HOME/.cargo/env" && cargo build --release
```

After re-login, **verify the metal capabilities** that justify the whole strategy:
```bash
numactl --hardware                          # expect: available: 2 nodes
grep -c arch_perfmon /proc/cpuinfo          # expect: > 0
perf stat -M TopdownL1 -- sleep 1           # expect: a real 4-bucket breakdown, no error
ulimit -n                                   # expect: >= 1048576  (loadgen shell also: ulimit -s 512)
```
If `perf stat -M TopdownL1` errors here, stop and recheck the instance is truly metal and `perf_event_paranoid=-1` applied — the TMA capture depends on it.

---

## 6. The run (one host; pure commands, no Claude Code)

```bash
cd ~/Rust-Tcp-Server
export SERVER_NUMA_NODE=0
export LOADGEN_NUMA_NODE=1
# loopback target; server binds 127.0.0.1 (default)

bash bench/run.sh                                   # 11-model sweep, C=1/10/100/1000/10000 (10000 now real), perf-free
C10K_CONNS=10000 C10K_RATE=50000 bash bench/c10k.sh    # true C10K + resource curves
bash bench/scaling.sh                               # multireactor workers 1/2/4/8/16/32/64
bash bench/profile.sh                               # strace + ctx + TMA top-down, ALL 11 models (dedicated runs)
python3 bench/plot.py                               # regenerate every plot
```

Pass the server `--max-connections 16384` on the C10K path exactly as Phase 2 does. Run a low-concurrency smoke first (`C10K_CONNS=1000 bash bench/c10k.sh epoll-et multireactor`) to confirm NUMA pinning and the perf path before the full unattended suite. (Optional stretch: 256 GiB can carry far past 10 000 — a 50 000-connection rung makes a striking headline — but 10 000 is the named C10K bar; keep it as the canonical result and add higher rungs only as clearly-labeled extras.)

---

## 7. Cost control

Credits are finite (~$5.44/hr burns $100 in ~18 h) — terminate promptly; do not leave it running overnight.

```bash
aws ec2 run-instances --region $REGION --image-id $AMI \
  --instance-type c6i.metal --count 1 \
  --key-name <KEY> --security-group-ids <SG> \
  --instance-initiated-shutdown-behavior terminate \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=30,VolumeType=gp3}' \
  --tag-specifications 'ResourceType=instance,Tags=[{Key=proj,Value=rtcp-bench-metal}]'
```
End of run (pull results off the box first, then it self-destructs):
```bash
scp -r ubuntu@<PUBLIC_IP>:~/Rust-Tcp-Server/bench/results ./ec2-results   # or git push from the box
ssh ubuntu@<PUBLIC_IP> 'sudo shutdown -h now'                            # terminates (launch flag)
aws ec2 terminate-instances --region $REGION --instance-ids <ID>          # belt-and-suspenders
```
Note: bare-metal instances take several minutes to provision and to terminate — budget for it, and set a small billing alarm as a hard backstop.

---

## 8. Data layout — archive the laptop run, regenerate canonical

Identical to before: the metal set lands at the canonical `bench/results/` (same filenames + schema, so docs resolve), with the new `profiles/perf_*.txt` TMA files added. The laptop run is preserved, demoted.

On the laptop, before metal (commit this):
```bash
mkdir -p bench/results/_archive-laptop-i5-1135G7
git mv bench/results/*.csv bench/results/*.png bench/results/*.log \
       bench/results/c10k_README.md bench/results/_archive-laptop-i5-1135G7/ 2>/dev/null || true
git mv bench/results/profiles bench/results/_archive-laptop-i5-1135G7/profiles
printf '%s\n' '# Constrained historical run (superseded)' \
  'i5-1135G7, 8 GiB, single-host loopback. C10K capped at 8000; perf unavailable (no TMA).' \
  'Superseded by the definitive c6i.metal 2-socket run in ../ (see docs/BENCHMARKS.md §2).' \
  > bench/results/_archive-laptop-i5-1135G7/README.md
git commit -am "archive constrained laptop run; bench/results/ now holds the definitive metal run"
```
Then bring `ec2-results/` into `bench/results/` and commit.

---

## 9. The one residual caveat (for the record)

The single remaining non-ideality is loopback over the inter-socket interconnect rather than a real NIC. It is purpose-appropriate (sandbox host↔guest is local) and orthogonal to the CPU/syscall/memory phenomena the benchmark demonstrates. The only configuration that removes even this is two bare-metal hosts over a real network (within $100, ~11.7 h on 2× c7i.metal-24xl) — recorded here as the zero-asterisk alternative, not chosen because the marginal realism gain does not justify 2× cost and SSH orchestration for this artifact's purpose.

---

## 10. Phase 3 Definition of Done

1. Identical `bench/results/` file set regenerated on metal (same names + schemas), plus `profiles/perf_<model>.txt` TMA for **all 11 models** and `perf_<model>_c10k.txt` for the signal models; laptop set archived with its note.
2. **TMA top-down captured natively for every model** — the §7 omission is gone; the four buckets are real data.
3. C10K ran at a true **10 000 connections**; the C=10000 sweep rung holds real data, no sentinels.
4. Server pinned to NUMA node 0, loadgen to node 1; `numactl --hardware` topology recorded in the methodology; clocks under `performance` governor.
5. Every new number traces to a committed file; nothing invented.
6. Instance terminated; credit spend recorded.

---

## 11. Final prompt for Claude Code — update all documentation from the metal numbers

Run after the new `bench/results/` is committed:

> Read `CLAUDE.md`, `docs/specs/phase2-spec.md` §8 (the Writing Standard), `docs/specs/phase3-spec.md`, and **every file under `bench/results/`** (the new bare-metal dataset, including `profiles/perf_*.txt`; the laptop run is archived under `bench/results/_archive-laptop-i5-1135G7/` and is historical only). Update all six documentation artifacts strictly from the new committed numbers — `README.md`, `docs/BENCHMARKS.md`, `docs/ARCHITECTURE.md`, `docs/x-thread.md`, `bench/results/c10k_README.md`, and `bench/results/profiles/README.md` — obeying the §8 Writing Standard in each.
>
> Specifically: (1) replace every figure and source-file citation with the metal value; (2) rewrite environment/methodology — now a single **c6i.metal** (or the instance actually used): **64 physical cores across 2 NUMA sockets, 256 GiB, Ubuntu 26.04, kernel <actual>**, with the server pinned to NUMA node 0 and loadgen to node 1, loopback transport, `performance` governor; record `numactl --hardware`; (3) update the C10K narrative to the true **10 000-connection** result and resource curves; (4) **add the TMA top-down results** (retiring / bad-speculation / frontend-bound / backend-bound) per model from `profiles/perf_*.txt` — and in `bench/results/profiles/README.md` **remove the "perf unavailable / paranoid=4 / top-down omitted" section entirely** and replace it with the real TMA tables and the metal-PMU method; bind the io_uring verdict to the new microarchitecture data where relevant (e.g., whether io_uring is more retiring-bound than epoll-et); (5) in **threats-to-validity** and **surprises-and-corrections**, flip the now-resolved confounds — single-host contention (NUMA-isolated now), the 8000 cap (true 10 000 now), and the missing TMA (captured now) — and state the single residual caveat: loopback over the inter-socket interconnect, no real NIC; note whether multireactor's ctx-switches/req fell toward ~1.0 as predicted; (6) keep one sentence in BENCHMARKS §2 referencing the archived laptop baseline; (7) re-confirm the io_uring verdict (single-ring still sheds above C≈1000; syscalls/req reproduced ~2.0 vs ~4.0) against the new data and adjust only if numbers moved.
>
> Invent nothing — every number cites its committed metal file. When done, re-read all six documents against the ten §8 rules, fix any violation, list exactly which numbers changed and which new TMA results were added, and STOP.
