"""transport.py EXPDIR [1to1]: the transport's own loss. For each EXPDIR/*.tif (16-bit CLI export):
fit = Lanczos in linear light to 1536x1024 (z6_a 1536x1022); 1to1 = a 2048x1536 centre crop.
Each option encodes the 8-bit reference and decodes it; prints chroma PSD option/float per band."""
import sys, os, io, subprocess, numpy as np
from PIL import Image
from chroma_metric import ref, cpsd
E = sys.argv[1]; one = len(sys.argv) > 2
OPTS = [('q94_420', 94, 2), ('q94', 94, 0), ('q96', 96, 0), ('q97', 97, 0), ('q98', 98, 0), ('q99', 99, 0), ('q100', 100, 0), ('png8', None, None)]
rows = {}
for f in sorted(x for x in os.listdir(E) if x.endswith('.tif')):
    if one:
        raw = subprocess.run(['magick', os.path.join(E, f), '-gravity', 'center', '-crop', '2048x1536+0+0', '+repage',
                              '-depth', '16', '-endian', 'LSB', 'rgb:-'], check=True, capture_output=True).stdout
        a = np.frombuffer(raw, '<u2').reshape(1536, 2048, 3).astype(np.float64) / 65535
    else:
        a = ref(os.path.join(E, f), 1536, 1022 if f.startswith('z6_a') else 1024)
    base = cpsd(a); u8 = Image.fromarray(np.clip(np.round(a * 255), 0, 255).astype(np.uint8))
    for name, q, ss in OPTS:
        b = io.BytesIO()
        u8.save(b, 'PNG', compress_level=1) if q is None else u8.save(b, 'JPEG', quality=q, subsampling=ss)
        b.seek(0); r = cpsd(np.asarray(Image.open(b).convert('RGB'), dtype=np.float64) / 255) / base
        rows.setdefault(name, []).append(r); print(f[:-4], name, ' '.join('%.3f' % v for v in r), flush=True)
print('| option | abs range | cells out of ±15 % |\n|---|---|---|')
for name, _, _ in OPTS:
    R = np.array(rows[name]); o = (R < 0.85) | (R > 1.15); print(f'| {name} | {R.min():.2f}–{R.max():.2f} | {o.sum()}/{R.size} |')
