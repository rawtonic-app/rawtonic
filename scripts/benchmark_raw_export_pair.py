#!/usr/bin/env python3
"""Compare native RAW export time/memory and exact bytes using private fixtures.

The sample manifest has the same schema as benchmark_raw_exports.py. Inputs must
be disposable working copies. Output paths, logs, recipes and source hashes stay
in the specified private run directory; the numeric report contains no paths.
Run with exclusive GPU access and no concurrent build or test process.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import statistics
import subprocess
import time


def sha(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True)
    parser.add_argument("--current", required=True)
    parser.add_argument("--samples", required=True)
    parser.add_argument("--masked-recipe", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--repeats", type=int, default=3)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error("--repeats must be positive")
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=False)
    samples = json.loads(Path(args.samples).read_text())
    binaries = {"baseline": str(Path(args.baseline).resolve()),
                "current": str(Path(args.current).resolve())}
    recipes = {"neutral": {}, "masked": json.loads(Path(args.masked_recipe).read_text())}
    for name, recipe in recipes.items():
        (out / (name + ".json")).write_text(json.dumps(recipe, indent=2))
    report = {"metric": "fresh-process wall time including startup and full-resolution JPEG100 export",
              "repeats": args.repeats, "samples": [], "summary": []}

    def persist():
        (out / "results.json").write_text(json.dumps(report, indent=2))

    for sample in samples:
        assert sha(sample["source"]) == sha(sample["sample"]) == sample["sha256"]
        for recipe in recipes:
            for repeat in range(args.repeats):
                # Alternate order within each recipe; hashing above warms source
                # data but OS file/shader caches otherwise remain uncontrolled.
                stages = ["baseline", "current"] if repeat % 2 == 0 else ["current", "baseline"]
                pair = {}
                for stage in stages:
                    name = f'{sample["model"]}-{sample["id"]}-{recipe}-{repeat}-{stage}'
                    dest = out / (name + ".jpg")
                    start = time.monotonic()
                    run = subprocess.run([
                        "/usr/bin/time", "-l", binaries[stage], "export", sample["sample"],
                        "--output", str(dest), "--quality", "100", "--adjustments",
                        str(out / (recipe + ".json")),
                    ], capture_output=True, text=True, timeout=240)
                    elapsed = time.monotonic() - start
                    dest.with_suffix(".log").write_text(run.stdout + run.stderr)
                    assert run.returncode == 0 and dest.exists(), name + " failed"
                    assert "Using embedded preview fallback" not in run.stderr, name + " fallback"
                    rss = re.search(r"(\d+)\s+maximum resident set size", run.stderr)
                    footprint = re.search(r"(\d+)\s+peak memory footprint", run.stderr)
                    info = subprocess.run(["exiftool", "-json", "-ImageWidth", "-ImageHeight", str(dest)],
                                          capture_output=True, text=True, check=True)
                    dimensions = json.loads(info.stdout)[0]
                    row = {"camera": sample["model"], "sample_id": sample["id"], "recipe": recipe,
                           "repeat": repeat, "stage": stage, "seconds": elapsed,
                           "max_rss_bytes": int(rss[1]) if rss else None,
                           "peak_footprint_bytes": int(footprint[1]) if footprint else None,
                           "width": dimensions["ImageWidth"], "height": dimensions["ImageHeight"],
                           "output_bytes": dest.stat().st_size, "embedded_fallback": False}
                    pair[stage] = (row, sha(dest))
                identical = pair["baseline"][1] == pair["current"][1]
                unchanged = sha(sample["source"]) == sha(sample["sample"]) == sample["sha256"]
                for row, _ in pair.values():
                    row.update(byte_identical=identical, source_and_copy_unchanged=unchanged)
                    report["samples"].append(row)
                    print(json.dumps(row), flush=True)
                persist()
                assert identical and unchanged, "Pixel/export or original-integrity regression"
    groups = sorted({(r["camera"], r["recipe"], r["stage"]) for r in report["samples"]})
    for camera, recipe, stage in groups:
        rows = [r for r in report["samples"] if (r["camera"], r["recipe"], r["stage"]) == (camera, recipe, stage)]
        summary = {"camera": camera, "recipe": recipe, "stage": stage, "n": len(rows)}
        for metric in ["seconds", "max_rss_bytes", "peak_footprint_bytes"]:
            values = [r[metric] for r in rows if r[metric] is not None]
            if values:
                summary[metric] = {"median": statistics.median(values), "min": min(values), "max": max(values)}
        report["summary"].append(summary)
    persist()
    print(json.dumps({"complete": True, "exports": len(report["samples"])}), flush=True)


if __name__ == "__main__":
    main()
