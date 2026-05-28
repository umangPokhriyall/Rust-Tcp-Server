#!/usr/bin/env python3
"""bench/plot.py — Phase 1 §7 plot rendering.

Renders three figures into ``bench/results/``:

  1. ``distribution_c<conns>.png`` — full latency distribution, one curve per
     model, at a chosen concurrency level. X = latency (µs, log). Y =
     ``1 / (1 - percentile)`` (log) — the standard HdrHistogram view in which
     each major gridline is one extra "nine" of the tail. The spec calls this
     the "interior latency-distribution plot (full histogram, log-y)".

  2. ``throughput_vs_concurrency.png`` — sustained rps per model as concurrency
     scales 1 → 10 → ... → 10 000.

  3. ``p99_vs_concurrency.png`` — p99 latency per model as concurrency scales.

Data source is whatever ``bench/run.sh`` produced under ``bench/results/``:
  * ``<model>.csv`` rows ``model,rate,connections,throughput_rps,errors,p50,
    p90,p99,p999,p9999,max``.
  * ``<model>_r<rate>_c<conns>.hgrm`` columns
    ``value_us,percentile,total_count,inverse_1_minus_p``.

Use ``--results-dir`` to point elsewhere. ``--dist-concurrency`` picks which
concurrency level the distribution plot uses (default 100). Missing files are
skipped with a stderr note — the script is meant to be re-runnable mid-sweep.
"""

import argparse
import csv
import os
import sys
from pathlib import Path

import matplotlib
matplotlib.use("Agg")  # render without an X server
import matplotlib.pyplot as plt

MODELS = [
    "iterative",
    "forking",
    "preforked",
    "thread-per-conn",
    "thread-pool",
    "poll",
    "epoll-lt",
    "epoll-et",
    "event-loop",
]

# A distinguishable color per model. tab10 ordered so blocking models cluster
# (cool hues) and event-loop models cluster (warm hues), making the plots
# legible at a glance.
COLORS = {
    "iterative":       "#1f77b4",
    "forking":         "#17becf",
    "preforked":       "#9467bd",
    "thread-per-conn": "#2ca02c",
    "thread-pool":     "#8c564b",
    "poll":            "#7f7f7f",
    "epoll-lt":        "#ff7f0e",
    "epoll-et":        "#d62728",
    "event-loop":      "#e377c2",
}


def load_csv(path: Path):
    """Return a list of dict rows with numeric fields parsed."""
    if not path.exists():
        return []
    out = []
    with path.open() as f:
        for row in csv.DictReader(f):
            try:
                out.append({
                    "model": row["model"],
                    "rate": int(row["rate"]),
                    "connections": int(row["connections"]),
                    "throughput_rps": float(row["throughput_rps"]),
                    "errors": int(row["errors"]),
                    "p50": int(row["p50"]),
                    "p90": int(row["p90"]),
                    "p99": int(row["p99"]),
                    "p999": int(row["p999"]),
                    "p9999": int(row["p9999"]),
                    "max": int(row["max"]),
                })
            except (KeyError, ValueError) as e:
                print(f"  skip malformed row in {path}: {e}", file=sys.stderr)
    return out


def load_hgrm(path: Path):
    """Return (values_us, inverse_1_minus_p) lists, dropping the 1.0 row whose
    inverse is +inf and any zero-latency rows that would break a log axis."""
    if not path.exists():
        return [], []
    values, inv = [], []
    with path.open() as f:
        for row in csv.DictReader(f):
            try:
                v = int(row["value_us"])
                i = float(row["inverse_1_minus_p"])
            except (KeyError, ValueError):
                continue
            if v <= 0:
                continue
            if i == float("inf"):
                continue
            values.append(v)
            inv.append(i)
    return values, inv


def plot_distribution(results_dir: Path, conns: int) -> Path | None:
    """One curve per model; latency on x (log), inverse-percentile on y (log).
    A rightward shift at high y = a worse tail. Returns the output path or
    None if no .hgrm files matched."""
    fig, ax = plt.subplots(figsize=(10, 6))
    plotted = 0
    for model in MODELS:
        # The matching .hgrm has whatever rate the sweep paired with `conns`;
        # we glob to avoid hard-coding the RATE_FOR table here.
        candidates = sorted(results_dir.glob(f"{model}_r*_c{conns}.hgrm"))
        if not candidates:
            continue
        values, inv = load_hgrm(candidates[-1])
        if not values:
            continue
        ax.plot(values, inv, label=model, color=COLORS.get(model), linewidth=1.4)
        plotted += 1
    if plotted == 0:
        plt.close(fig)
        print(f"  no .hgrm files for c={conns}; skipping distribution plot",
              file=sys.stderr)
        return None
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xlabel("latency (µs)")
    ax.set_ylabel("1 / (1 − percentile)")
    ax.set_title(f"Latency distribution at concurrency = {conns}")
    ax.grid(True, which="both", linestyle=":", linewidth=0.5, alpha=0.6)
    ax.legend(loc="lower right", fontsize=8)
    fig.tight_layout()
    out = results_dir / f"distribution_c{conns}.png"
    fig.savefig(out, dpi=140)
    plt.close(fig)
    return out


def plot_metric_vs_concurrency(
    results_dir: Path,
    field: str,
    ylabel: str,
    title: str,
    filename: str,
    yscale: str = "log",
) -> Path | None:
    """Line plot per model of `field` vs `connections`. Used for both
    throughput-vs-concurrency and p99-vs-concurrency."""
    fig, ax = plt.subplots(figsize=(10, 6))
    plotted = 0
    for model in MODELS:
        rows = load_csv(results_dir / f"{model}.csv")
        if not rows:
            continue
        # If multiple rows share a concurrency (e.g. re-runs), keep the last.
        by_conn = {}
        for r in rows:
            by_conn[r["connections"]] = r[field]
        xs = sorted(by_conn.keys())
        ys = [by_conn[x] for x in xs]
        ax.plot(xs, ys, marker="o", label=model, color=COLORS.get(model),
                linewidth=1.5)
        plotted += 1
    if plotted == 0:
        plt.close(fig)
        print(f"  no CSV data for {field}; skipping {filename}", file=sys.stderr)
        return None
    ax.set_xscale("log")
    if yscale == "log":
        ax.set_yscale("log")
    ax.set_xlabel("concurrency (connections)")
    ax.set_ylabel(ylabel)
    ax.set_title(title)
    ax.grid(True, which="both", linestyle=":", linewidth=0.5, alpha=0.6)
    ax.legend(loc="best", fontsize=8)
    fig.tight_layout()
    out = results_dir / filename
    fig.savefig(out, dpi=140)
    plt.close(fig)
    return out


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--results-dir", default="bench/results",
                   help="directory holding <model>.csv and *.hgrm files")
    p.add_argument("--dist-concurrency", type=int, default=100,
                   help="concurrency level used by the distribution plot")
    args = p.parse_args()

    results_dir = Path(args.results_dir)
    if not results_dir.is_dir():
        print(f"results dir not found: {results_dir}", file=sys.stderr)
        return 1

    written = []
    out = plot_distribution(results_dir, args.dist_concurrency)
    if out: written.append(out)
    out = plot_metric_vs_concurrency(
        results_dir,
        field="throughput_rps",
        ylabel="sustained throughput (requests/sec)",
        title="Throughput vs concurrency",
        filename="throughput_vs_concurrency.png",
        yscale="log",
    )
    if out: written.append(out)
    out = plot_metric_vs_concurrency(
        results_dir,
        field="p99",
        ylabel="p99 latency (µs)",
        title="p99 latency vs concurrency",
        filename="p99_vs_concurrency.png",
        yscale="log",
    )
    if out: written.append(out)

    if not written:
        print("no plots written — is the sweep results directory populated?",
              file=sys.stderr)
        return 1
    for p in written:
        print(f"wrote {p}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
