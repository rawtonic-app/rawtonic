# SRF16: compare two 16-bit RGB TIFF exports (REF TEST NAME [CROPDIR]); uv run --with tifffile --with numpy --with pillow.
# Stats over the whole image (16-bit and 8-bit-rounded), crops at 200 % (nearest):
# 3 most detailed 192 px blocks, 1 flat block (least detail, upper third = sky), the largest-difference block.
import sys, numpy as np, tifffile
from PIL import Image
def load(p):
    try: return tifffile.memmap(p)
    except Exception: return tifffile.imread(p)
a = load(sys.argv[1]); b = load(sys.argv[2]); name = sys.argv[3]
assert a.shape == b.shape and a.dtype == np.uint16, (a.shape, b.shape, a.dtype)
H, W = a.shape[:2]; a = a[..., :3]; b = b[..., :3]
mse = 0.0; mx = 0; over = {k: 0 for k in (0, 64, 128, 257)}; mx8 = 0; n8 = 0; best = (-1, 0, 0)
for y in range(0, H, 512):
    ra = a[y:y+512].astype(np.int32); rb = b[y:y+512].astype(np.int32); d = np.abs(ra - rb)
    mse += float((d.astype(np.float64) ** 2).sum()); mx = max(mx, int(d.max()))
    for k in over: over[k] += int((d > k).sum())
    d8 = np.abs(np.round(ra / 257.0) - np.round(rb / 257.0)); mx8 = max(mx8, int(d8.max())); n8 += int((d8 > 0).sum())
    dm = d.max(axis=2)
    if dm.max() > best[0]:
        iy, ix = np.unravel_index(int(dm.argmax()), dm.shape); best = (int(dm.max()), y + iy, ix)
mse /= a.size
psnr = float('inf') if mse == 0 else 10 * np.log10(65535.0 ** 2 / mse)
print(f"{name}: {W}x{H} 16-bit max={mx} codes ({mx/65535:.2e}) rms={np.sqrt(mse):.2f} PSNR={psnr:.2f} dB "
      + " ".join(f">{k}:{v/a.size*100:.4f}%" for k, v in over.items())
      + f" | 8-bit max={mx8} differ={n8/a.size*100:.3f}%")
if len(sys.argv) > 4:
    out = sys.argv[4]; C = 192
    g = a[::8, ::8].astype(np.float32).mean(axis=2)
    e = np.abs(np.diff(g, axis=1))[:-1] + np.abs(np.diff(g, axis=0))[:, :-1]
    bs = C // 8; eh, ew = e.shape[0] // bs, e.shape[1] // bs
    blk = e[:eh*bs, :ew*bs].reshape(eh, bs, ew, bs).sum(axis=(1, 3))
    order = np.argsort(blk.ravel())[::-1]
    picks = [np.unravel_index(i, blk.shape) for i in order[:60:20]]
    top = blk[: eh // 3]
    picks.append(np.unravel_index(int(np.argmin(top)), top.shape))
    crops = [(py * C, px * C, lab) for (py, px), lab in zip(picks, ["detail", "detail", "detail", "flat-sky"])]
    crops.append((max(0, min(H - C, best[1] - C // 2)), max(0, min(W - C, best[2] - C // 2)), "largest-diff"))
    for i, (y, x, lab) in enumerate(crops):
        ra = a[y:y+C, x:x+C].astype(np.float64) / 65535; rb = b[y:y+C, x:x+C].astype(np.float64) / 65535
        diff = np.clip(np.abs(ra - rb) * 64, 0, 1)
        t = [np.uint8(np.round(v * 255)) for v in (ra, rb, diff)]
        sep = np.full((C, 4, 3), 255, np.uint8)
        strip = np.concatenate([t[0], sep, t[1], sep, t[2]], axis=1)
        Image.fromarray(strip).resize((strip.shape[1] * 2, C * 2), Image.NEAREST).save(f"{out}/{name}-crop{i}-{lab}-y{y}-x{x}.png")
        print(f"  crop{i} {lab} y={y} x={x} max={np.abs(ra-rb).max()*65535:.0f} codes")
