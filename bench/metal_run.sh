#!/usr/bin/env bash
# bench/metal_run.sh — Phase 3 §A.8 unattended metal-box driver.
#
# Runs the full Phase 3 sweep on a single Latitude EPYC box end-to-end so the
# billed metal time is spent running the suite, not waiting on a human at a
# prompt (Mode A, §0). It regenerates the identical bench/results/ file set at
# the canonical top-level paths (superseding the archived laptop set under
# bench/results/_archive-laptop-i5-1135G7/), captures AMD Zen4 pipeline analysis
# via the vendor-aware bench/profile.sh, records the rig of record, and — unless
# METAL_NO_GIT is set — commits and pushes the results.
#
# NUMA policy (Phase 3 §3):
#   - sweep / c10k / profile : server pinned to SERVER_NUMA_NODE (default 0),
#     loadgen to LOADGEN_NUMA_NODE (default 1) — disjoint cores / private per-CCD
#     L3 / disjoint memory controllers, the confound-free isolation the run is
#     for. The clean ctx-switches/req number depends on this pin.
#   - scaling : run UNPINNED. The multireactor scaling curve climbs to the full
#     socket (1→$(nproc) workers, §5 DoD #5); a single-node pin would cap the
#     server at one CCD group and oversubscribe the top rungs. This reproduces
#     the laptop scaling methodology (single-host, loadgen contention documented)
#     with more cores, keeping the curves apples-to-apples with the archive.
#
# Env knobs:
#   SERVER_NUMA_NODE / LOADGEN_NUMA_NODE  NUMA nodes (default 0 / 1)
#   C10K_CONNS / C10K_RATE                true C10K (default 10000 / 50000)
#   PERF_METRIC_GROUP                     AMD Zen pipeline group from §A.6
#                                         (REQUIRED unless METAL_ALLOW_NO_PERF=1)
#   METAL_ALLOW_NO_PERF=1                 permit the run with no perf group set
#                                         (sweep/c10k/scaling still valid; the
#                                         perf pass self-skips per profile.sh)
#   METAL_NO_GIT=1                        skip the final commit + push
#
# The script is idempotent: every downstream harness truncates its own outputs.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Pinning: prefer explicit core lists (SERVER_CPUS/LOADGEN_CPUS via --physcpubind,
# e.g. under NPS1 where disjoint-node isolation is impossible); otherwise fall back
# to NUMA-node pinning (SERVER_NUMA_NODE/LOADGEN_NUMA_NODE).
if [[ -n "${SERVER_CPUS:-}" || -n "${LOADGEN_CPUS:-}" ]]; then
    export SERVER_CPUS LOADGEN_CPUS MEMBIND_NODE="${MEMBIND_NODE:-0}"
    PIN_DESC="server cpus=${SERVER_CPUS:-<unset>}  loadgen cpus=${LOADGEN_CPUS:-<unset>}  membind=${MEMBIND_NODE}"
else
    export SERVER_NUMA_NODE="${SERVER_NUMA_NODE:-0}"
    export LOADGEN_NUMA_NODE="${LOADGEN_NUMA_NODE:-1}"
    PIN_DESC="server node=$SERVER_NUMA_NODE  loadgen node=$LOADGEN_NUMA_NODE"
fi
C10K_CONNS="${C10K_CONNS:-10000}"
C10K_RATE="${C10K_RATE:-50000}"
RESULTS_DIR="${RESULTS_DIR:-bench/results}"

# ----- §A.6 comprehension gate (fail fast, before any billed sweep time) -----
# Intel TopdownL1,TopdownL2 do not exist on AMD Zen. profile.sh self-skips the
# perf pass on AMD when the group is unset, but a *definitive* run wants the
# pipeline data — so require the human-selected group up front rather than
# discovering a skipped perf pass after the whole suite has run.
if [[ -z "${PERF_METRIC_GROUP:-}" && -z "${METAL_ALLOW_NO_PERF:-}" ]]; then
    cat >&2 <<'EOF'
metal_run.sh: PERF_METRIC_GROUP is not set.

Per Phase 3 §A.6 this must be the AMD Zen4 pipeline-utilization group a human
read from the box and understood before publishing any pipeline claim:

    perf list metricgroups | grep -iE 'pipeline|frontend|backend|retir|spec'

Re-run with, e.g.:

    PERF_METRIC_GROUP='<AMD Zen pipeline group>' bash bench/metal_run.sh

To run the sweep/c10k/scaling now and defer the perf pass (it will self-skip):

    METAL_ALLOW_NO_PERF=1 bash bench/metal_run.sh
EOF
    exit 2
fi

# ----- rig of record (Phase 3 §5 DoD #4; source for BENCHMARKS §2) -----
capture_rig() {
    local out="$RESULTS_DIR/rig.txt"
    mkdir -p "$RESULTS_DIR"
    {
        echo "# Rig of record — captured by bench/metal_run.sh"
        echo "# date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo "# git:  $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
        echo
        echo "## uname -a"; uname -a
        echo; echo "## lscpu (model / sockets / NUMA / caches)"
        lscpu 2>/dev/null | grep -iE 'model name|socket|core|thread|numa|cache|mhz' || lscpu
        echo; echo "## numactl --hardware"; numactl --hardware 2>/dev/null || echo "numactl unavailable"
        echo; echo "## microcode"
        grep -m1 microcode /proc/cpuinfo 2>/dev/null || echo "n/a"
        echo; echo "## scaling governor"
        cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo "n/a"
        echo; echo "## perf_event_paranoid"
        cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo "n/a"
        echo; echo "## PERF_METRIC_GROUP"
        echo "${PERF_METRIC_GROUP:-<unset — perf pass will self-skip>}"
        echo; echo "## Pinning (sweep/c10k/profile)"
        echo "$PIN_DESC"
    } >"$out"
    echo "wrote $out"
}

echo "=== metal_run: building release binaries ==="
cargo build --release

echo "=== metal_run: capturing rig of record ==="
capture_rig

echo "=== metal_run: loopback sanity ==="
ping -c 5 127.0.0.1 >/dev/null || echo "  (ping unavailable; continuing)"

echo "=== metal_run: 11-model sweep ($PIN_DESC) ==="
bash bench/run.sh

echo "=== metal_run: true C10K (conns=$C10K_CONNS rate=$C10K_RATE) ==="
C10K_CONNS="$C10K_CONNS" C10K_RATE="$C10K_RATE" bash bench/c10k.sh

echo "=== metal_run: multireactor scaling grid (UNPINNED — full socket, auto-capped at nproc) ==="
env -u SERVER_NUMA_NODE -u LOADGEN_NUMA_NODE -u SERVER_CPUS -u LOADGEN_CPUS bash bench/scaling.sh

echo "=== metal_run: vendor-aware profiling pass (all 11 models) ==="
bash bench/profile.sh

echo "=== metal_run: regenerating plots ==="
python3 bench/plot.py

if [[ -n "${METAL_NO_GIT:-}" ]]; then
    echo "=== metal_run: METAL_NO_GIT set — skipping commit/push ==="
else
    echo "=== metal_run: committing + pushing results ==="
    git add -A "$RESULTS_DIR"
    if git diff --cached --quiet; then
        echo "  no result changes to commit"
    else
        git commit -m "phase3: definitive Latitude m4.metal.large run (EPYC 9254, AMD Zen4 pipeline analysis)"
        git push || echo "  git push failed (no remote / no auth?) — results are committed locally"
    fi
fi

echo "=== metal_run: done. Results under $RESULTS_DIR/ ==="
