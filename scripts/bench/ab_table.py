#!/usr/bin/env python3
"""Table of an A/B benchmark run (scripts/bench/ab.sh).

Reads criterion's results under WORK/criterion/<side>/ (one saved baseline
per round, `r1`, `r2`, ...) and prints, per benchmark, the median time of
base and head (median of the rounds' medians), the change and its noise
band. The band is the larger of
  - the spread of one side's round medians ((max - min) / median), and
  - criterion's 95 % confidence interval of one run's median (half width),
over both sides, at least --min-band (default 3 %: separate runs of the
same binary differ by up to ~2 % on an otherwise idle 6-core machine, more
than criterion's interval within one run shows). A change inside the band
is noise.

Usage: scripts/bench/ab_table.py WORK [--json OUT] [--min-band PCT] [--fail-on-slower]
Exit status 3 with --fail-on-slower when a benchmark is slower beyond its band.
"""
from __future__ import annotations

import argparse
import json
import pathlib
import statistics
import sys

MIN_BAND = 3.0  # percent


def runs(side_dir: pathlib.Path) -> dict[str, list[dict]]:
    """full benchmark id -> [{median, ci_half}] per round."""
    out: dict[str, list[dict]] = {}
    for estimates in sorted(side_dir.rglob("estimates.json")):
        round_dir = estimates.parent
        if not round_dir.name.startswith("r") or not round_dir.name[1:].isdigit():
            continue  # criterion's own `new`/`base`/`change` directories
        meta = round_dir / "benchmark.json"
        if not meta.exists():
            continue
        full_id = json.loads(meta.read_text())["full_id"]
        median = json.loads(estimates.read_text())["median"]
        point = median["point_estimate"]
        ci = median["confidence_interval"]
        half = (ci["upper_bound"] - ci["lower_bound"]) / 2 / point if point else 0.0
        out.setdefault(full_id, []).append({"round": int(round_dir.name[1:]), "median": point, "ci_half": half})
    return out


def fmt_time(ns: float) -> str:
    for unit, scale in (("s", 1e9), ("ms", 1e6), ("µs", 1e3)):
        if ns >= scale:
            return f"{ns / scale:.3g} {unit}"
    return f"{ns:.3g} ns"


def summary(samples: list[dict]) -> tuple[float, float]:
    medians = [s["median"] for s in samples]
    mid = statistics.median(medians)
    spread = (max(medians) - min(medians)) / mid if len(medians) > 1 and mid else 0.0
    return mid, max([spread] + [s["ci_half"] for s in samples])


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("work", type=pathlib.Path)
    parser.add_argument("--json", type=pathlib.Path)
    parser.add_argument("--fail-on-slower", action="store_true")
    parser.add_argument("--min-band", type=float, default=MIN_BAND, help="smallest noise band in percent")
    args = parser.parse_args()
    base = runs(args.work / "criterion" / "base")
    head = runs(args.work / "criterion" / "head")
    rows = []
    for full_id in sorted(set(base) | set(head)):
        row = {"id": full_id, "base_ns": None, "head_ns": None, "change": None, "band": None, "verdict": ""}
        b = summary(base[full_id]) if full_id in base else None
        h = summary(head[full_id]) if full_id in head else None
        if b:
            row["base_ns"] = b[0]
        if h:
            row["head_ns"] = h[0]
        if b and h:
            row["change"] = h[0] / b[0] - 1
            row["band"] = max(b[1], h[1], args.min_band / 100)
            if abs(row["change"]) <= row["band"]:
                row["verdict"] = "noise"
            else:
                row["verdict"] = "SLOWER" if row["change"] > 0 else "faster"
        else:
            row["verdict"] = "only head" if h else "only base"
        row["rounds"] = {"base": len(base.get(full_id, [])), "head": len(head.get(full_id, []))}
        rows.append(row)

    print("| benchmark | base | head | change | noise ± | verdict |")
    print("|---|---:|---:|---:|---:|---|")
    for r in rows:
        cell = lambda v: fmt_time(v) if v is not None else "—"  # noqa: E731
        change = f"{r['change'] * 100:+.1f} %" if r["change"] is not None else "—"
        band = f"{r['band'] * 100:.1f} %" if r["band"] is not None else "—"
        print(f"| {r['id']} | {cell(r['base_ns'])} | {cell(r['head_ns'])} | {change} | {band} | {r['verdict']} |")
    if args.json:
        args.json.write_text(json.dumps(rows, indent=1) + "\n")
    slower = [r["id"] for r in rows if r["verdict"] == "SLOWER"]
    if slower:
        print(f"\nslower beyond noise: {', '.join(slower)}")
    return 3 if slower and args.fail_on_slower else 0


if __name__ == "__main__":
    sys.exit(main())
