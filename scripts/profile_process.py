#!/usr/bin/env python3
"""Sample explicit app/helper PIDs plus system-wide Apple GPU counters; no image data.
CPU percent uses ps convention (100% = one logical core). RSS is not physical footprint.
The GPU counters include every application; never attribute them to Rawtonic alone.
"""
import argparse, json, plistlib, subprocess, time
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--pids', required=True, help='Comma-separated main + identified WebKit helper PIDs')
p.add_argument('--output', required=True)
p.add_argument('--seconds', type=int, default=300)
p.add_argument('--phase-file', required=True)
a = p.parse_args()

def gpu_stats(nodes):
    for node in nodes:
        if 'PerformanceStatistics' in node:
            return node['PerformanceStatistics']
        nested = gpu_stats(node.get('IORegistryEntryChildren', []))
        if nested: return nested
    return {}

with open(a.output, 'a') as out:
    end = time.monotonic() + a.seconds
    while time.monotonic() < end:
        start = time.monotonic()
        phase = Path(a.phase_file).read_text().strip()
        raw = subprocess.run(['ps', '-p', a.pids, '-o', 'pid=,%cpu=,rss=,time='], capture_output=True, text=True)
        processes = []
        for line in raw.stdout.splitlines():
            pid, cpu, rss, cpu_time = line.split()
            processes.append(dict(pid=int(pid), cpu_percent=float(cpu), rss_bytes=int(rss)*1024, cpu_time=cpu_time))
        gpu = subprocess.run(['ioreg', '-a', '-r', '-c', 'AGXAccelerator'], capture_output=True)
        try: stats = gpu_stats(plistlib.loads(gpu.stdout))
        except Exception: stats = {}
        record = dict(time=time.time(), phase=phase, processes=processes, gpu_system=stats)
        out.write(json.dumps(record) + '\n'); out.flush()
        time.sleep(max(0, 2 - (time.monotonic() - start)))
