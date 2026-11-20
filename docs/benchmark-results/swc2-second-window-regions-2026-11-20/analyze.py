"""analyze.py EXPDIR FRAMEROOT MODE... : fine chroma of settled in-app 100 % frames against the CLI export of the recipe
the app held (SWC1/SWC2, after VPR1 inapp100.py; MODE floor_X = the 8-bit floor at the pixels of mode X). FRAMEROOT/<photo>/<mode>/<recipe>.{png,jpg} with <recipe>.region.json:
either normalised {x,y,w,h} (a region frame) or {px:[x,y,w,h]} (full-resolution px); no region file = the frame is the
whole photograph, cut at the region of FRAMEROOT/<photo>/closed/<recipe>.region.json (nearest-rounded, no resize).
MODE 'floor' = the export's own 8-bit rounding. abs = frame/ref chroma PSD per band (80 cells, bar 0.85-1.15);
norm = (frame_r/frame_none)/(ref_r/ref_none) (60 cells)."""
import sys, os, glob, json, subprocess, numpy as np
from PIL import Image
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'lnxj1-linux-preview-transport-2026-11-17'))
from chroma_metric import cpsd
E, root, modes = sys.argv[1], sys.argv[2], sys.argv[3:]
R = ['none', 'nrc25', 'nrc', 'nrc100']; P = ['a7c_iso12800', 'd5500_iso6400', 'd5500_b', 'z6_a']; rows = {}; per = {}
def crop(tif, x, y, w, h):
    raw = subprocess.run(['magick', tif, '-crop', f'{w}x{h}+{x}+{y}', '+repage', '-depth', '16', '-endian', 'LSB', 'rgb:-'],
                         check=True, capture_output=True).stdout
    return np.frombuffer(raw, '<u2').reshape(h, w, 3).astype(np.float64) / 65535
def rect(rj, W, H):
    if 'px' in rj: return [int(round(v)) for v in rj['px']]
    return [round(rj['x'] * W), round(rj['y'] * H), round(rj['w'] * W), round(rj['h'] * H)]
for p in P:
    for m in modes:
        ps, refs = {}, {}
        for r in R:
            tif = f'{E}/{p}__{r}__on.tif'
            W, H = map(int, subprocess.run(['magick', 'identify', '-format', '%w %h', tif], capture_output=True, text=True).stdout.split())
            rf = f'{root}/{p}/{m[6:] if m.startswith("floor_") else m}/{r}.region.json'
            if not os.path.exists(rf): rf = f'{root}/{p}/closed/{r}.region.json'
            rj = json.load(open(rf)); x, y, w, h = rect(rj, W, H); fx = fy = 0
            if 'px' in rj:  # a frame that is the region itself: analyse its centre (bounded cost)
                cw, ch = min(w, 2400), min(h, 1500); fx, fy = (w - cw) // 2, (h - ch) // 2; x, y, w, h = x + fx, y + fy, cw, ch
            ref = crop(tif, x, y, w, h)
            if m.startswith('floor'): fr = np.round(ref * 255) / 255
            else:
                fr = np.asarray(Image.open(glob.glob(f'{root}/{p}/{m}/{r}.*g')[0]).convert('RGB'), dtype=np.float64) / 255
                if 'px' in rj:
                    fr = fr[fy:fy + h, fx:fx + w]
                elif fr.shape[:2] != (h, w):
                    assert fr.shape[0] >= y + h and fr.shape[1] >= x + w, (p, m, r, fr.shape, (x, y, w, h))
                    fr = fr[y:y + h, x:x + w]
            ps[r], refs[r] = cpsd(fr), cpsd(ref)
        for r in R:
            rows.setdefault((m, 'abs'), []).append(ps[r] / refs[r]); per.setdefault((m, p, r), ps[r] / refs[r])
            if r != 'none': rows.setdefault((m, 'norm'), []).append((ps[r] / ps['none']) / (refs[r] / refs['none']))
print('| transport | measure | range | cells out of ±15 % |\n|---|---|---|---|')
for k in sorted(rows):
    a = np.array(rows[k]); o = (a < 0.85) | (a > 1.15)
    print(f'| {k[0]} | {k[1]} | {a.min():.2f}–{a.max():.2f} | {o.sum()}/{a.size} |')
if 'floor' in modes:
    out = {m: (np.array(rows[(m, 'abs')]) < 0.85) | (np.array(rows[(m, 'abs')]) > 1.15) for m in modes}
    for m in modes:
        if m.startswith('floor'): continue
        print(f'{m} vs floor, abs cells: out in both {int((out[m] & out["floor"]).sum())}, only {m} {int((out[m] & ~out["floor"]).sum())}, only floor {int((~out[m] & out["floor"]).sum())}')
print('\nper recipe abs ratios (5 bands):')
for k in sorted(per): print(k, ' '.join(f'{v:.2f}' for v in per[k]))
