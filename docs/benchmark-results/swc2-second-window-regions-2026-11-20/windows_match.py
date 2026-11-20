"""windows_match.py EXPDIR FRAMEROOT MAINMODE SECONDMODE: the main window's settled 100 % frame against the second window's
own 100 % region where both cover the photograph (same full-resolution px): mean / p99 / max abs difference in 8-bit
codes per channel, and the share of pixels that differ. Same pixels = 0."""
import sys, json, glob, subprocess, numpy as np
from PIL import Image
E, root, mm, sm = sys.argv[1:5]
for p in ['a7c_iso12800', 'd5500_iso6400', 'd5500_b', 'z6_a']:
    W, H = map(int, subprocess.run(['magick', 'identify', '-format', '%w %h', f'{E}/{p}__none__on.tif'], capture_output=True, text=True).stdout.split())
    for r in ['none', 'nrc25', 'nrc', 'nrc100']:
        mj = json.load(open(f'{root}/{p}/{mm}/{r}.region.json')); sj = json.load(open(f'{root}/{p}/{sm}/{r}.region.json'))
        mi = np.asarray(Image.open(glob.glob(f'{root}/{p}/{mm}/{r}.png')[0]).convert('RGB')).astype(int)
        si = np.asarray(Image.open(glob.glob(f'{root}/{p}/{sm}/{r}.*g')[0]).convert('RGB')).astype(int)
        mh, mw = mi.shape[:2]
        mx, my = round(mj['x'] * W), round(mj['y'] * H)
        sx, sy = [int(round(v)) for v in sj['px'][:2]]
        sh, sw = si.shape[:2]
        x0, y0 = max(mx, sx), max(my, sy); x1, y1 = min(mx + mw, sx + sw), min(my + mh, sy + sh)
        a = mi[y0 - my:y1 - my, x0 - mx:x1 - mx]; b = si[y0 - sy:y1 - sy, x0 - sx:x1 - sx]
        d = np.abs(a - b)
        # distance of each overlap pixel to the second region's nearest edge
        yy, xx = np.mgrid[y0:y1, x0:x1]
        edge = np.minimum.reduce([xx - sx, yy - sy, sx + sw - 1 - xx, sy + sh - 1 - yy])
        far = d[edge >= 256]
        print(f'{p} {r}: overlap {x1-x0}x{y1-y0} at ({x0},{y0}); mean {d.mean():.3f} p99 {np.percentile(d,99):.0f} max {d.max()} codes; differing px {(d.max(2)>0).mean()*100:.2f} %; >=256 px from its edges: n {far.shape[0]} mean {far.mean() if far.size else 0:.3f} max {far.max() if far.size else 0}')
