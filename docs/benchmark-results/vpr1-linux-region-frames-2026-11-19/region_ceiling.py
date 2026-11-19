"""region_ceiling.py EXPDIR WxH...: the 100 % region frame's transport loss. For each 16-bit TIFF export in EXPDIR,
a centre WxH crop (no resize) is the float reference; each option encodes its 8-bit copy and decodes it.
abs = chroma PSD(option)/PSD(reference) per band (chroma_metric.py of the LNXJ1 report)."""
import sys, os, io, subprocess, numpy as np
from PIL import Image
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'lnxj1-linux-preview-transport-2026-11-17'))
from chroma_metric import cpsd
E = sys.argv[1]; SIZES = [tuple(map(int, s.split('x'))) for s in sys.argv[2:]]
OPTS = [('q99_444', 99), ('png8', None)]
for W, H in SIZES:
    rows = {}
    for f in sorted(os.listdir(E)):
        if not f.endswith('.tif'): continue
        raw = subprocess.run(['magick', os.path.join(E, f), '-gravity', 'center', '-crop', f'{W}x{H}+0+0', '+repage',
                              '-depth', '16', '-endian', 'LSB', 'rgb:-'], check=True, capture_output=True).stdout
        a = np.frombuffer(raw, '<u2').reshape(H, W, 3).astype(np.float64) / 65535; base = cpsd(a)
        u8 = Image.fromarray(np.clip(np.round(a * 255), 0, 255).astype(np.uint8))
        for name, q in OPTS:
            b = io.BytesIO()
            u8.save(b, 'PNG', compress_level=1) if q is None else u8.save(b, 'JPEG', quality=q, subsampling=0)
            b.seek(0); r = cpsd(np.asarray(Image.open(b).convert('RGB'), dtype=np.float64) / 255) / base
            rows.setdefault(name, []).append(r); print(f'{W}x{H}', f[:-4], name, ' '.join('%.3f' % v for v in r), flush=True)
    print(f'| {W}x{H} | option | abs range | cells out of ±15 % |\n|---|---|---|---|')
    for name, _ in OPTS:
        R = np.array(rows[name]); o = (R < 0.85) | (R > 1.15)
        print(f'| | {name} | {R.min():.2f}-{R.max():.2f} | {o.sum()}/{R.size} |')
