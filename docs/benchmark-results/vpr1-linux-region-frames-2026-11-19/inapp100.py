"""inapp100.py EXPDIR FRAMEROOT MODE...: settled in-app 100 % frames FRAMEROOT/<photo>/<mode>/<recipe>.{png,jpg}
(QA control editor.preview_image: a region frame with <recipe>.region.json = its normalised x, y, w, h; the old whole
frame is cut at the region of the 'region' run) (MODE 'floor': the export's own 8-bit rounding) against EXPDIR/<photo>__<recipe>__on.tif, the CLI export of the recipe
the app held, cut at the same pixels (no resize). abs = frame/ref chroma PSD per band; norm = (frame_r/frame_none)/(ref_r/ref_none)."""
import sys, os, glob, json, subprocess, numpy as np
from PIL import Image
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'lnxj1-linux-preview-transport-2026-11-17'))
from chroma_metric import cpsd
E, root, modes = sys.argv[1], sys.argv[2], sys.argv[3:]
R = ['none', 'nrc25', 'nrc', 'nrc100']; P = ['a7c_iso12800', 'd5500_iso6400', 'd5500_b', 'z6_a']; rows = {}
def crop(tif, x, y, w, h):
    raw = subprocess.run(['magick', tif, '-crop', f'{w}x{h}+{x}+{y}', '+repage', '-depth', '16', '-endian', 'LSB', 'rgb:-'],
                         check=True, capture_output=True).stdout
    return np.frombuffer(raw, '<u2').reshape(h, w, 3).astype(np.float64) / 65535
for p in P:
    for m in modes:
        ps, refs = {}, {}
        for r in R:
            reg = json.load(open(f'{root}/{p}/region/{r}.region.json')); tif = f'{E}/{p}__{r}__on.tif'
            W, H = map(int, subprocess.run(['magick', 'identify', '-format', '%w %h', tif], capture_output=True, text=True).stdout.split())
            x, y, w, h = round(reg['x'] * W), round(reg['y'] * H), round(reg['w'] * W), round(reg['h'] * H)
            ref = crop(tif, x, y, w, h)
            if m == 'floor':  # the reference itself rounded to 8 bits: what any 8-bit frame of it shows
                fr = np.round(ref * 255) / 255
            else:
                fr = np.asarray(Image.open(glob.glob(f'{root}/{p}/{m}/{r}.*g')[0]).convert('RGB'), dtype=np.float64) / 255
                if fr.shape[:2] != (h, w): fr = fr[y:y + h, x:x + w]
            ps[r], refs[r] = cpsd(fr), cpsd(ref)
        for r in R:
            rows.setdefault((m, 'abs'), []).append(ps[r] / refs[r])
            if r != 'none': rows.setdefault((m, 'norm'), []).append((ps[r] / ps['none']) / (refs[r] / refs['none']))
print('| transport | measure | range | cells out of ±15 % |\n|---|---|---|---|')
for k in sorted(rows):
    a = np.array(rows[k]); o = (a < 0.85) | (a > 1.15)
    print(f'| {k[0]} | {k[1]} | {a.min():.2f}–{a.max():.2f} | {o.sum()}/{a.size} |')
# Cell by cell: which abs cells are out for a mode and not for the 8-bit floor, and the reverse.
if 'floor' in modes:
    out = {m: (np.array(rows[(m, 'abs')]) < 0.85) | (np.array(rows[(m, 'abs')]) > 1.15) for m in modes}
    for m in modes:
        if m == 'floor': continue
        print(f'{m} vs floor, abs cells: out in both {int((out[m] & out["floor"]).sum())}, only {m} {int((out[m] & ~out["floor"]).sum())}, only floor {int((~out[m] & out["floor"]).sum())}')
