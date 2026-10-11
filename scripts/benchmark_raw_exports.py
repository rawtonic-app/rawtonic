#!/usr/bin/env python3
"""Run actual headless RAW exports on disposable working copies.
The manifest stays local; the numeric report contains only camera IDs and dimensions.
Outputs are exclusively created in a new run directory. No inputs are modified/deleted.
"""
import argparse, hashlib, json, re, subprocess, time
from pathlib import Path
p=argparse.ArgumentParser()
p.add_argument('--binary', required=True)
p.add_argument('--samples', required=True)
p.add_argument('--output', required=True)
p.add_argument('--repeats', type=int, default=1)
a=p.parse_args()
root=Path(a.output); root.mkdir(parents=True, exist_ok=False)
samples=json.loads(Path(a.samples).read_text())
results=[]
for sample in samples:
    path=Path(sample['sample'])
    for repeat in range(a.repeats):
        name=f"{sample['model']}_{sample['id']}_{repeat}"
        dest=root/f'{name}.jpg'; log=root/f'{name}.log'
        begin=time.monotonic()
        r=subprocess.run(['/usr/bin/time','-l',a.binary,'export',str(path),'--output',str(dest),'--quality','100'],capture_output=True,text=True,timeout=180)
        seconds=time.monotonic()-begin
        log.write_text(r.stdout+'\n'+r.stderr)
        rss=re.search(r'(\d+)\s+maximum resident set size',r.stderr)
        footprint=re.search(r'(\d+)\s+peak memory footprint',r.stderr)
        times=re.search(r'([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys',r.stderr)
        dims={}
        if dest.exists():
            info=subprocess.run(['exiftool','-json','-ImageWidth','-ImageHeight',str(dest)],capture_output=True,text=True,check=True)
            dims=json.loads(info.stdout)[0];dims.pop('SourceFile',None)
        record=dict(model=sample['model'],sample_id=sample['id'],repeat=repeat,seconds=seconds,exit_code=r.returncode,max_rss_bytes=int(rss[1]) if rss else None,peak_footprint_bytes=int(footprint[1]) if footprint else None,user_seconds=float(times[2]) if times else None,system_seconds=float(times[3]) if times else None,dimensions=dims,output_bytes=dest.stat().st_size if dest.exists() else 0,embedded_fallback='Using embedded preview fallback' in r.stderr,original_unchanged=hashlib.file_digest(open(sample['source'],'rb'),'sha256').hexdigest()==sample['sha256'],copy_unchanged=hashlib.file_digest(open(path,'rb'),'sha256').hexdigest()==sample['sha256'])
        results.append(record)
        (root/'results.json').write_text(json.dumps(results,indent=2))
        print(json.dumps(record),flush=True)
