#!/usr/bin/env python3
"""Symbolize a macOS `sample` taken from a stripped Rawtonic binary.

Build the same Rust code unstripped in a separate target dir, e.g.
  CARGO_PROFILE_RELEASE_STRIP=false CARGO_TARGET_DIR=target-sym npm run tauri -- build --features ... --bundles app
then run
  scripts/symbolize_sample.py STRIPPED_BINARY UNSTRIPPED_BINARY SAMPLE.txt > symbolized.txt
Frontend assets and paths shift code by a few hundred bytes, so each address is
remapped by matching the surrounding instruction bytes before `atos`; the
applied shift is printed per frame ("?" marks frames without a confident match).
Assumes thin arm64 binaries and the load address printed in the sample.
"""
import sys,re,subprocess
old_bin,new_bin,sample=sys.argv[1:4]
old=open(old_bin,'rb').read(); new=open(new_bin,'rb').read()
# arm64 thin? check fat
lines=open(sample).read().splitlines()
addrs=sorted({int(m.group(1),16) for l in lines for m in [re.search(r'load address 0x[0-9a-f]+ \+ (0x[0-9a-f]+)',l)] if m})
def remap(off):
    # file offset == vm offset for __TEXT in thin binary
    w=48
    pat=old[off-w:off+16]
    best=None
    for d in range(0,0x2000,4):
        for s in (off-d, off+d):
            if new[s-w:s+16]==pat: return s
    # fuzzy: compare words ignoring mismatches
    bestscore=-1
    for d in range(-0x2000,0x2000,4):
        s=off+d
        a=old[off-128:off+64]; b=new[s-128:s+64]
        score=sum(1 for i in range(0,len(a),4) if a[i:i+4]==b[i:i+4])
        if score>bestscore: bestscore,best=score,s
    return best if bestscore>30 else None
m={}
for a in addrs:
    r=remap(a); m[a]=r
args=[hex(0x100000000+(m[a] if m[a] else a)) for a in addrs]
out=subprocess.run(['atos','-o',new_bin,'-arch','arm64']+args,capture_output=True,text=True).stdout.splitlines()
sym={a:(o if m[a] else '?'+o) for a,o in zip(addrs,out)}
for l in lines:
    mm=re.search(r'load address 0x[0-9a-f]+ \+ (0x[0-9a-f]+)',l)
    if mm:
        a=int(mm.group(1),16)
        s=re.sub(r' \(in rawtonic-app\)','',sym[a])
        l=l[:l.index('???')]+(f"[{(m[a] or a)-a:+#x}] ")+s[:190]
    print(l)
