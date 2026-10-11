#!/usr/bin/env python3
"""Summarize real-input canvas drags from a profiling build's app log.

Each `pointer_session` record (webview) covers one press-drag-release on the
canvas: pointer event rates, animation-frame gaps, render requests dropped in
favour of newer pointer states, and pointer-to-GPU-complete latency. Native
`display_present` records inside the session window add the rate at which
frames were queued for the screen, and `display_refresh` gives the screen's
maximum refresh rate. Presents are queued for the next vsync (FIFO), so none of
this measures pointer-to-photon time. A present is queued before the GPU has
finished, so latency is taken to the end of the native preview job, which
waits for the GPU: pointer event to GPU-complete frame, without the return of
the response to the webview. That join compares webview and native clocks;
input_to_response (pointer event to the preview response in the webview) uses
the webview clock only.

Builds from the input-split diagnostics on also split the time before the IPC
call of each live frame: delivery (event time stamp to the page's capture
listener: WebKit and main-thread delays), handlers (listener to the preview
request: canvas handlers, overlay request, queue wait) and payload (request to
the IPC call). Times in the webview are whole milliseconds.

Builds that log `display_frame_bound` (frame ids, 2026-10-04) also give
input_to_present: pointer event to the native present call of the frame its
preview job bound. That stays comparable when presentation no longer happens
inside the preview job; it still ends before scanout.

Usage: summarize_pointer_sessions.py APP_LOG [--json]
"""
import bisect
import json
import re
import statistics
import sys

RECORD = re.compile(r"\[PERF\] (\{.*\})\s*$")


def quantile(values, fraction):
    values = sorted(values)
    return values[min(len(values) - 1, int(fraction * len(values)))] if values else 0.0


def spread(values):
    if not values:
        return None
    return {
        "median": round(statistics.median(values), 2),
        "p95": round(quantile(values, 0.95), 2),
        "max": round(max(values), 2),
    }


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    sessions, presents, refresh, live, jobs = [], [], [], [], []
    bound, framed_presents = [], []
    with open(sys.argv[1], encoding="utf-8", errors="replace") as log:
        for line in log:
            match = RECORD.search(line)
            if not match:
                continue
            try:
                record = json.loads(match[1])
            except json.JSONDecodeError:
                continue
            stage = record.get("stage")
            if stage == "pointer_session":
                sessions.append(record)
            elif stage == "display_present":
                presents.append(record["epoch_ms"])
                if "frame" in record:
                    framed_presents.append((record["frame"], record["epoch_ms"]))
            elif stage == "display_frame_bound":
                bound.append((record["epoch_ms"], record["frame"]))
            elif stage == "display_refresh":
                refresh.append(record)
            elif stage == "preview_live_roundtrip" and "input_ms" in record:
                live.append(record)
            elif stage == "preview_job":
                jobs.append((record["end_epoch_ms"], record["start_epoch_ms"]))
    presents.sort()
    jobs.sort()
    bound.sort()
    bound_times = [t for t, _ in bound]
    framed_presents.sort(key=lambda item: item[1])

    def present_of(job_start, job_end):
        """Present epoch of the last frame bound within a preview job."""
        lo = bisect.bisect_left(bound_times, job_start)
        hi = bisect.bisect_right(bound_times, job_end)
        if hi <= lo:
            return None
        frame = bound[hi - 1][1]
        for shown, epoch in framed_presents:
            if shown >= frame and epoch >= bound[hi - 1][0]:
                return epoch
        return None
    job_ends = [end for end, _ in jobs]
    max_fps = refresh[-1]["max_fps"] if refresh else None

    rows = []
    for session in sessions:
        start, end = session["start_epoch_ms"], session["end_epoch_ms"]
        inside = [t for t in presents if start <= t <= end]
        gaps = [b - a for a, b in zip(inside, inside[1:])]
        to_gpu, to_present = [], []
        split = {"delivery": [], "handlers": [], "payload": [], "response": []}
        for frame in live:
            if not start <= frame["end_epoch_ms"] <= end:
                continue
            split["response"].append(frame["input_ms"])
            if "dispatch_ms" in frame:
                split["delivery"].append(frame["dispatch_ms"])
                split["handlers"].append(frame["handler_ms"])
                split["payload"].append(frame["prep_ms"])
            index = bisect.bisect_right(job_ends, frame["end_epoch_ms"]) - 1
            if index >= 0 and jobs[index][1] >= frame["start_epoch_ms"] - 50:
                to_gpu.append(jobs[index][0] - (frame["end_epoch_ms"] - frame["input_ms"]))
                shown = present_of(jobs[index][1], jobs[index][0])
                if shown is not None:
                    to_present.append(shown - (frame["end_epoch_ms"] - frame["input_ms"]))
        span = (inside[-1] - inside[0]) if len(inside) > 1 else 0
        row = {
            "mask_type": session.get("mask_type"),
            "pointer_type": session.get("pointer_type"),
            "duration_ms": round(session["duration_ms"]),
            "moves": session["moves"],
            "coalesced_moves": session.get("coalesced_moves"),
            "move_hz": round(session["move_hz"], 1),
            "move_interval_p50_ms": round(session["move_interval_p50_ms"], 2),
            "dispatch_interval_p95_ms": round(session.get("dispatch_interval_p95_ms", 0), 2),
            "webview_frame_p50_ms": round(session["frame_p50_ms"], 2),
            "webview_long_frames": session["long_frames"],
            "max_moves_per_frame": session["max_moves_per_frame"],
            "preview": {
                key: session.get(f"preview_{key}")
                for key in ("submitted", "superseded", "dispatched", "frames")
            },
            "preview_input_ms": {
                "p50": round(session.get("preview_input_p50_ms", 0), 2),
                "p95": round(session.get("preview_input_p95_ms", 0), 2),
                "max": round(session.get("preview_input_max_ms", 0), 2),
            },
            "overlay": {
                key: session.get(f"overlay_{key}")
                for key in ("submitted", "superseded", "dispatched", "frames")
            },
            "overlay_input_ms": {
                "p50": round(session.get("overlay_input_p50_ms", 0), 2),
                "p95": round(session.get("overlay_input_p95_ms", 0), 2),
                "max": round(session.get("overlay_input_max_ms", 0), 2),
            },
            "input_to_gpu_ms": spread(to_gpu),
            "input_to_present_ms": spread(to_present),
            "_to_gpu": to_gpu,
            "_to_present": to_present,
            "_split": split,
            "presents": len(inside),
            "present_hz": round((len(inside) - 1) * 1000 / span, 1) if span else 0,
            "present_gap_ms": spread(gaps),
        }
        if max_fps:
            refresh_ms = 1000 / max_fps
            row["present_gaps_over_2_refreshes"] = sum(gap > 2 * refresh_ms for gap in gaps)
        rows.append(row)

    totals = {}
    for row in rows:
        group = totals.setdefault(row["mask_type"], {"sessions": 0, "move_hz": [], "present_hz": [],
                                                     "input_to_gpu": [],
                                                     "input_to_present": [],
                                                     "superseded_share": [],
                                                     "webview_frame_ms": [],
                                                     "input_to_response_ms": [],
                                                     "delivery_ms": [], "handlers_ms": [],
                                                     "payload_ms": []})
        group["sessions"] += 1
        group["move_hz"].append(row["move_hz"])
        group["present_hz"].append(row["present_hz"])
        group["input_to_gpu"].extend(row.pop("_to_gpu"))
        group["input_to_present"].extend(row.pop("_to_present"))
        group["webview_frame_ms"].append(row["webview_frame_p50_ms"])
        split = row.pop("_split")
        group["input_to_response_ms"].extend(split["response"])
        group["delivery_ms"].extend(split["delivery"])
        group["handlers_ms"].extend(split["handlers"])
        group["payload_ms"].extend(split["payload"])
        if row["preview"]["submitted"]:
            group["superseded_share"].append(row["preview"]["superseded"] / row["preview"]["submitted"])
    summary = {
        "display_max_fps": max_fps,
        "sessions": len(rows),
        "by_mask_type": {
            name: {"sessions": group["sessions"],
                   "input_to_gpu_frames": len(group["input_to_gpu"]),
                   **{key: spread(values) for key, values in group.items()
                      if key != "sessions" and values}}
            for name, group in totals.items()
        },
    }
    if "--json" in sys.argv:
        print(json.dumps({"summary": summary, "sessions": rows}, indent=1))
        return
    print(json.dumps(summary, indent=1))
    for row in rows:
        print(json.dumps(row))


if __name__ == "__main__":
    main()
