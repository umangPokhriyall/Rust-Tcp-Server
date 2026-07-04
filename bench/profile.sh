#!/usr/bin/env bash
# bench/profile.sh — Phase 2 §7 / Phase 3 §4 profiling harness.
#
# Captures, for all 11 models (Phase 3 §4a), three independent passes:
#   1. syscalls/req  : run the server under `strace -c -f -- server ...`,
#                      drive a fixed open-loop request budget, send SIGINT,
#                      let strace flush the summary to a file. Divide
#                      total syscalls by completed requests.
#   2. ctx-switches  : run the server normally, snapshot
#                      /proc/<pid>/status's voluntary_ctxt_switches and
#                      nonvoluntary_ctxt_switches before and after the
#                      load, divide the delta by completed requests.
#   3. pipeline      : (Phase 3 §4b) a dedicated steady-state run — start the
#                      server natively, drive a fixed loadgen load, and sample
#                      the server PID with `perf stat -M <group> -p <pid> --
#                      sleep <dur>`, writing perf_<model>.txt. The metric group
#                      is vendor-selected: on Intel the Top-down Microarchitecture
#                      Analysis groups (TopdownL1,TopdownL2); on AMD the Zen
#                      pipeline-utilization group named in $PERF_METRIC_GROUP
#                      (the architectural counterpart to Intel TMA, discovered
#                      on the box via `perf list metricgroups`, Phase 3 §A.6 —
#                      NOT Intel TMA relabeled). $PERF_METRIC_GROUP overrides the
#                      auto-selected group on any vendor when set.
#                      For the signal models (epoll-et, multireactor,
#                      io-uring) a second capture runs under C10K-level
#                      load → perf_<model>_c10k.txt (Phase 3 §4c).
#
# The TMA pass needs PMU access (perf_event_paranoid <= 0 / CAP_PERFMON),
# available on the bare-metal host. Where perf is missing or denied (e.g. a
# laptop with perf_event_paranoid = 4), the TMA capture writes a diagnostic
# note and returns success so the strace/ctx passes are unaffected; the perf
# path is validated on metal.
#
# Outputs to bench/results/profiles/:
#   strace_<model>.txt              — `strace -c -f` summary
#   ctx_<model>.txt                 — pre/post /proc/<pid>/status deltas
#   perf_<model>.txt                — perf stat pipeline metric group (steady)
#   perf_<model>_c10k.txt           — pipeline capture under C10K load (signal models)
#   loadgen_strace_<model>.csv      — loadgen results under strace
#   loadgen_ctx_<model>.csv         — loadgen results without strace
#   loadgen_tma_<model>.csv         — loadgen results during TMA capture
#   loadgen_tma_c10k_<model>.csv    — loadgen results during C10K TMA capture
#   server_<model>.log              — server stdout/stderr
#   summary.csv                     — derived syscalls/req, ctxs/req table
#
# The script is idempotent: each run truncates its own outputs.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SERVER_BIN="${SERVER_BIN:-target/release/server}"
LOADGEN_BIN="${LOADGEN_BIN:-target/release/loadgen}"
ASSETS_DIR="${ASSETS_DIR:-server/assets}"
OUT="${OUT:-bench/results/profiles}"
LOADGEN_WRAP="${LOADGEN_WRAP:-bench/_loadgen_wrap.sh}"
export LOADGEN_BIN
export LOADGEN_STACK_KIB="${LOADGEN_STACK_KIB:-128}"

# Optional NUMA pinning (Phase 3 §3). Unset = empty prefix, byte-identical.
SERVER_NUMA=()
LOADGEN_NUMA=()
# Core-pin (SERVER_CPUS/LOADGEN_CPUS via --physcpubind) takes precedence over
# node-pin (SERVER_NUMA_NODE). Required under NPS1: with one NUMA node, disjoint-
# node isolation is impossible, so --physcpubind gives disjoint cores + disjoint
# per-CCD L3 (memory interleaved — the documented NPS1 caveat).
if [[ -n "${SERVER_CPUS:-}" ]]; then
    SERVER_NUMA=(numactl --physcpubind="$SERVER_CPUS" --membind="${MEMBIND_NODE:-0}")
elif [[ -n "${SERVER_NUMA_NODE:-}" ]]; then
    SERVER_NUMA=(numactl --cpunodebind="$SERVER_NUMA_NODE" --membind="$SERVER_NUMA_NODE")
fi
if [[ -n "${LOADGEN_CPUS:-}" ]]; then
    LOADGEN_NUMA=(numactl --physcpubind="$LOADGEN_CPUS" --membind="${MEMBIND_NODE:-0}")
elif [[ -n "${LOADGEN_NUMA_NODE:-}" ]]; then
    LOADGEN_NUMA=(numactl --cpunodebind="$LOADGEN_NUMA_NODE" --membind="$LOADGEN_NUMA_NODE")
fi

# All 11 models, run when no positional model list is given (Phase 3 §4a).
ALL_MODELS=(iterative forking preforked thread-per-conn thread-pool poll epoll-lt epoll-et event-loop multireactor io-uring)

# Signal models get a second TMA capture under C10K-level load (Phase 3 §4c).
SIGNAL_MODELS="${SIGNAL_MODELS:-epoll-et multireactor io-uring}"

# Workload knobs. Defaults chosen so the strace-attached server can keep
# up (low rate + low concurrency) and the request budget is large enough
# that the per-request denominators are statistically meaningful but the
# strace summary file stays small.
STRACE_RATE="${STRACE_RATE:-500}"
STRACE_CONNS="${STRACE_CONNS:-10}"
STRACE_DURATION="${STRACE_DURATION:-20}"

# Native (no strace) load for the ctx-switch capture: same shape, longer
# duration so the ctx-switch delta dominates the few hundred switches
# that fire on process startup/teardown.
CTX_RATE="${CTX_RATE:-2000}"
CTX_CONNS="${CTX_CONNS:-100}"
CTX_DURATION="${CTX_DURATION:-30}"

# TMA top-down capture. A dedicated steady-state run: perf samples the server
# PID for TMA_DURATION while loadgen supplies a fixed mid-range load.
TMA_RATE="${TMA_RATE:-20000}"
TMA_CONNS="${TMA_CONNS:-100}"
TMA_DURATION="${TMA_DURATION:-20}"

# C10K-level TMA capture (signal models only). Mirrors bench/c10k.sh: 10000
# connections, with the server's max-connections bumped so event-loop models
# accept them.
TMA_C10K_RATE="${TMA_C10K_RATE:-50000}"
TMA_C10K_CONNS="${TMA_C10K_CONNS:-10000}"
TMA_C10K_DURATION="${TMA_C10K_DURATION:-20}"
TMA_C10K_MAX_CONNS="${TMA_C10K_MAX_CONNS:-16384}"

# Seconds to let loadgen reach steady state before the perf window opens.
TMA_WARMUP="${TMA_WARMUP:-3}"

# Vendor-aware pipeline metric group (Phase 3 §4, §A.6).
#
# Intel's `TopdownL1,TopdownL2` metric groups exist only on Intel silicon;
# running them verbatim on AMD Zen errors or silently misleads. This resolves
# the group used by the perf pass:
#   - $PERF_METRIC_GROUP set  → use it verbatim on any vendor (the on-box path:
#     the human reads `perf list metricgroups` on the EPYC box per §A.6, picks
#     the Zen pipeline-utilization group, and passes it in).
#   - unset + AMD CPU          → no Intel default exists; the group MUST be
#     supplied. The perf pass is skipped with a diagnostic pointing at §A.6
#     rather than running a group that does not exist on Zen.
#   - unset + non-AMD CPU      → fall back to the Intel TMA groups (laptop path).
if [[ -n "${PERF_METRIC_GROUP:-}" ]]; then
    :                                    # caller-supplied group wins on any vendor
elif grep -qi amd /proc/cpuinfo 2>/dev/null; then
    PERF_METRIC_GROUP=""
else
    PERF_METRIC_GROUP="TopdownL1,TopdownL2"
fi

if [[ $# -gt 0 ]]; then
    MODELS=("$@")
else
    MODELS=("${ALL_MODELS[@]}")
fi
PORT_BASE="${PORT_BASE:-31000}"

mkdir -p "$OUT"
ulimit -n 65536 2>/dev/null || true

# ----- helpers -----

wait_for_port() {
    local port=$1
    for _ in {1..400}; do
        if (exec 3<>/dev/tcp/127.0.0.1/"$port") 2>/dev/null; then
            exec 3<&- 3>&-
            return 0
        fi
        sleep 0.05
    done
    return 1
}

sum_ctxt() {
    # Sum voluntary + nonvoluntary context switches across the PID and
    # every thread under /proc/<pid>/task/*. Returns "vol nonvol total".
    local pid=$1
    awk '
        /^voluntary_ctxt_switches:/    { v += $2 }
        /^nonvoluntary_ctxt_switches:/ { n += $2 }
        END                            { printf "%d %d %d\n", v, n, v+n }
    ' /proc/"$pid"/task/*/status 2>/dev/null || echo "0 0 0"
}

requests_from_csv() {
    # The loadgen CSV row reports throughput_rps and the script knows the
    # duration — multiply for completed request count. Falls back to 0
    # when the CSV has no data row.
    local csv=$1 duration=$2
    awk -F, -v d="$duration" '
        NR==1 { next }
        { rps = $4; printf "%d\n", rps * d; exit }
    ' "$csv"
}

total_syscalls_from_strace() {
    # `strace -c -f` emits a summary table; the final " total" line carries
    # the grand syscall count in column 4 (after % time, seconds, usecs/call).
    local file=$1
    awk '
        /^[- ]+$/ { sep++; next }
        sep >= 2 && $NF == "total" { print $4; exit }
        sep >= 2 && /total$/       { print $4; exit }
    ' "$file"
}

# ----- per-model capture -----

start_under_strace() {
    local model=$1 port=$2 strace_out=$3 server_log=$4
    # strace as parent → ptrace_scope=1 is satisfied. -f follows clones so
    # multireactor's worker threads are traced too. -c gives the summary.
    strace -c -f -o "$strace_out" -- \
        "${SERVER_NUMA[@]}" "$SERVER_BIN" --model "$model" --port "$port" --assets-dir "$ASSETS_DIR" \
        >"$server_log" 2>&1 &
    STRACE_PID=$!
    # The strace child is the server. Wait briefly for it to appear, then
    # for the port to come up.
    for _ in {1..200}; do
        SERVER_PID=$(pgrep -P "$STRACE_PID" 2>/dev/null | head -1 || true)
        [[ -n "$SERVER_PID" ]] && break
        sleep 0.05
    done
    if [[ -z "${SERVER_PID:-}" ]]; then
        echo "[$model] could not locate server PID under strace $STRACE_PID" >&2
        return 1
    fi
    wait_for_port "$port"
}

stop_under_strace() {
    # SIGINT the server; strace forwards it via PTRACE and exits, flushing
    # its -c summary to the -o file.
    local pid=$1
    if kill -0 "$pid" 2>/dev/null; then
        kill -INT "$pid" 2>/dev/null || true
        for _ in {1..60}; do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$pid" 2>/dev/null || true
    fi
    wait "$STRACE_PID" 2>/dev/null || true
}

start_native() {
    local model=$1 port=$2 server_log=$3
    "${SERVER_NUMA[@]}" "$SERVER_BIN" --model "$model" --port "$port" --assets-dir "$ASSETS_DIR" \
        >"$server_log" 2>&1 &
    SERVER_PID=$!
    wait_for_port "$port"
}

stop_native() {
    local pid=$1
    if kill -0 "$pid" 2>/dev/null; then
        kill -INT "$pid" 2>/dev/null || true
        for _ in {1..60}; do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
}

is_signal_model() {
    local m=$1 s
    for s in $SIGNAL_MODELS; do
        [[ "$m" == "$s" ]] && return 0
    done
    return 1
}

# Pipeline metric-group capture (Phase 3 §4b/§4c). Starts the server natively,
# drives a fixed loadgen load in the background, then samples the server PID with
# `perf stat -M "$PERF_METRIC_GROUP" -p <pid> -- sleep <dur>`. The group is the
# vendor-resolved value from the top of the script. The throughput sweep/c10k/
# scaling runs stay perf-free; only this dedicated run uses perf.
#
# perf access is denied on hosts with perf_event_paranoid > 0 and no
# CAP_PERFMON (e.g. the laptop). On any such failure — and on AMD with no
# $PERF_METRIC_GROUP supplied — this writes a diagnostic note to the output file
# and returns 0, so the strace/ctx passes, which do not depend on perf, are
# never broken. The perf path is validated on metal.
tma_capture() {
    local model=$1 port=$2 rate=$3 conns=$4 dur=$5
    local out=$6 srv_log=$7 lg_csv=$8 max_conns="${9:-}"

    : >"$out"; : >"$lg_csv"

    if ! command -v perf >/dev/null 2>&1; then
        echo "# perf not found on PATH — pipeline capture skipped (validated on metal host)." >"$out"
        return 0
    fi

    if [[ -z "$PERF_METRIC_GROUP" ]]; then
        {
            echo "# AMD CPU detected and no \$PERF_METRIC_GROUP supplied — pipeline capture skipped."
            echo "# Intel TopdownL1,TopdownL2 do not exist on AMD Zen. Per Phase 3 §A.6, run"
            echo "#   perf list metricgroups | grep -iE 'pipeline|frontend|backend|retir|spec'"
            echo "# on the box, then re-run with PERF_METRIC_GROUP='<AMD Zen pipeline group>'."
        } >"$out"
        return 0
    fi

    local extra=()
    [[ -n "$max_conns" ]] && extra=(--max-connections "$max_conns")
    "${SERVER_NUMA[@]}" "$SERVER_BIN" --model "$model" --port "$port" \
        --assets-dir "$ASSETS_DIR" "${extra[@]}" >"$srv_log" 2>&1 &
    SERVER_PID=$!
    if ! wait_for_port "$port"; then
        echo "# server failed to come up for TMA capture; see $srv_log" >"$out"
        stop_native "$SERVER_PID"
        return 0
    fi
    sleep 0.2

    # Background loadgen supplies steady-state load across the perf window.
    "${LOADGEN_NUMA[@]}" "$LOADGEN_WRAP" --target "127.0.0.1:$port" --model "$model" \
        --rate "$rate" --connections "$conns" \
        --duration "$((dur + TMA_WARMUP + 5))" --out "$lg_csv" \
        >>"$srv_log" 2>&1 &
    local lg_pid=$!
    sleep "$TMA_WARMUP"

    perf stat -M "$PERF_METRIC_GROUP" -p "$SERVER_PID" -o "$out" -- sleep "$dur" \
        || echo "# perf stat returned nonzero (perf_event_paranoid / no PMU / bad metric group '$PERF_METRIC_GROUP'?)." >>"$out"

    kill "$lg_pid" 2>/dev/null || true
    wait "$lg_pid" 2>/dev/null || true
    stop_native "$SERVER_PID"
}

capture_model() {
    local model=$1 idx=$2
    local strace_port=$((PORT_BASE + idx*4))
    local ctx_port=$((PORT_BASE + idx*4 + 1))
    local tma_port=$((PORT_BASE + idx*4 + 2))
    local tma_c10k_port=$((PORT_BASE + idx*4 + 3))

    local strace_out="$OUT/strace_${model}.txt"
    local ctx_out="$OUT/ctx_${model}.txt"
    local strace_csv="$OUT/loadgen_strace_${model}.csv"
    local ctx_csv="$OUT/loadgen_ctx_${model}.csv"
    local s_log="$OUT/server_strace_${model}.log"
    local c_log="$OUT/server_ctx_${model}.log"

    : >"$strace_out"; : >"$ctx_out"; : >"$strace_csv"; : >"$ctx_csv"

    echo "=== $model: strace -c -f (rate=$STRACE_RATE c=$STRACE_CONNS dur=${STRACE_DURATION}s port=$strace_port) ==="
    start_under_strace "$model" "$strace_port" "$strace_out" "$s_log"
    sleep 0.2
    "${LOADGEN_NUMA[@]}" "$LOADGEN_WRAP" --target "127.0.0.1:$strace_port" --model "$model" \
        --rate "$STRACE_RATE" --connections "$STRACE_CONNS" \
        --duration "$STRACE_DURATION" --out "$strace_csv" \
        >>"$s_log" 2>&1 || echo "  loadgen exited nonzero (strace pass)"
    stop_under_strace "$SERVER_PID"

    echo "=== $model: native ctx-switch (rate=$CTX_RATE c=$CTX_CONNS dur=${CTX_DURATION}s port=$ctx_port) ==="
    start_native "$model" "$ctx_port" "$c_log"
    sleep 0.2
    local before; before=$(sum_ctxt "$SERVER_PID")
    "${LOADGEN_NUMA[@]}" "$LOADGEN_WRAP" --target "127.0.0.1:$ctx_port" --model "$model" \
        --rate "$CTX_RATE" --connections "$CTX_CONNS" \
        --duration "$CTX_DURATION" --out "$ctx_csv" \
        >>"$c_log" 2>&1 || echo "  loadgen exited nonzero (ctx pass)"
    local after; after=$(sum_ctxt "$SERVER_PID")
    {
        echo "# /proc/<pid>/task/*/status snapshots, summed across all threads."
        echo "# Columns: voluntary_ctxt_switches nonvoluntary_ctxt_switches total"
        echo "before $before"
        echo "after  $after"
        local bv bn bt av an at
        read -r bv bn bt <<<"$before"
        read -r av an at <<<"$after"
        echo "delta  $((av-bv)) $((an-bn)) $((at-bt))"
    } >"$ctx_out"
    stop_native "$SERVER_PID"

    echo "=== $model: TMA top-down (rate=$TMA_RATE c=$TMA_CONNS dur=${TMA_DURATION}s port=$tma_port) ==="
    tma_capture "$model" "$tma_port" "$TMA_RATE" "$TMA_CONNS" "$TMA_DURATION" \
        "$OUT/perf_${model}.txt" "$OUT/server_tma_${model}.log" \
        "$OUT/loadgen_tma_${model}.csv"

    if is_signal_model "$model"; then
        echo "=== $model: TMA top-down C10K (rate=$TMA_C10K_RATE c=$TMA_C10K_CONNS dur=${TMA_C10K_DURATION}s port=$tma_c10k_port) ==="
        tma_capture "$model" "$tma_c10k_port" "$TMA_C10K_RATE" "$TMA_C10K_CONNS" "$TMA_C10K_DURATION" \
            "$OUT/perf_${model}_c10k.txt" "$OUT/server_tma_c10k_${model}.log" \
            "$OUT/loadgen_tma_c10k_${model}.csv" "$TMA_C10K_MAX_CONNS"
    fi
}

# ----- main -----

SERVER_PID=""
STRACE_PID=""
trap '[[ -n "$SERVER_PID" ]] && kill -KILL "$SERVER_PID" 2>/dev/null; [[ -n "$STRACE_PID" ]] && kill -KILL "$STRACE_PID" 2>/dev/null' EXIT

idx=0
for m in "${MODELS[@]}"; do
    capture_model "$m" "$idx"
    idx=$((idx + 1))
done

# ----- derive summary table -----

SUMMARY="$OUT/summary.csv"
echo "model,strace_total_syscalls,strace_requests,syscalls_per_req,ctx_vol_delta,ctx_nonvol_delta,ctx_total_delta,ctx_requests,ctx_switches_per_req" >"$SUMMARY"
for m in "${MODELS[@]}"; do
    strace_out="$OUT/strace_${m}.txt"
    strace_csv="$OUT/loadgen_strace_${m}.csv"
    ctx_out="$OUT/ctx_${m}.txt"
    ctx_csv="$OUT/loadgen_ctx_${m}.csv"

    sc=$(total_syscalls_from_strace "$strace_out" 2>/dev/null || echo 0)
    [[ -z "$sc" ]] && sc=0
    sr=$(requests_from_csv "$strace_csv" "$STRACE_DURATION" 2>/dev/null || echo 0)
    [[ -z "$sr" ]] && sr=0
    spr=$(awk -v s="$sc" -v r="$sr" 'BEGIN{if(r>0)printf "%.3f", s/r; else print "NA"}')

    read -r cv cn ct < <(awk '/^delta/ {print $2, $3, $4}' "$ctx_out")
    cv=${cv:-0}; cn=${cn:-0}; ct=${ct:-0}
    cr=$(requests_from_csv "$ctx_csv" "$CTX_DURATION" 2>/dev/null || echo 0)
    [[ -z "$cr" ]] && cr=0
    cpr=$(awk -v s="$ct" -v r="$cr" 'BEGIN{if(r>0)printf "%.3f", s/r; else print "NA"}')

    echo "$m,$sc,$sr,$spr,$cv,$cn,$ct,$cr,$cpr" >>"$SUMMARY"
done

echo
echo "--- summary ($SUMMARY) ---"
cat "$SUMMARY"
echo "done."
