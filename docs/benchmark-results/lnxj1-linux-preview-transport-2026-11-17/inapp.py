"""inapp.py EXPDIR FRAMEROOT MODE...: settled in-app fit frames FRAMEROOT_<mode>/<photo>/<recipe>.{png,jpg}
(QA control editor.preview_image after each change) against EXPDIR/<photo>__<recipe>.tif, the CLI export of
the same recipe. abs = frame/ref chroma PSD per band; norm = (frame_r/frame_none)/(ref_r/ref_none)."""
import sys, glob, numpy as np
from chroma_metric import load, ref, cpsd
E, root, modes = sys.argv[1], sys.argv[2], sys.argv[3:]
R = ['none', 'nrc25', 'nrc', 'nrc100']; P = ['a7c_iso12800', 'd5500_iso6400', 'd5500_b', 'z6_a']; rows = {}
for p in P:
    refs = None
    for m in modes:
        fr = {r: load(glob.glob(f'{root}_{m}/{p}/{r}.*')[0]) for r in R}; h, w = fr['none'].shape[:2]
        refs = refs or {r: cpsd(ref(f'{E}/{p}__{r}.tif', w, h)) for r in R}
        ps = {r: cpsd(fr[r]) for r in R}
        for r in R:
            rows.setdefault((m, 'abs'), []).append(ps[r] / refs[r])
            if r != 'none': rows.setdefault((m, 'norm'), []).append((ps[r] / ps['none']) / (refs[r] / refs['none']))
print('| transport | measure | range | cells out of ±15 % |\n|---|---|---|---|')
for k in sorted(rows):
    a = np.array(rows[k]); o = (a < 0.85) | (a > 1.15); print(f'| {k[0]} | {k[1]} | {a.min():.2f}–{a.max():.2f} | {o.sum()}/{a.size} |')
