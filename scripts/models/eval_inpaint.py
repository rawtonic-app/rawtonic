"""Object removal: fills of Rawtonic's Remove compared on real objects and on
holes with known content (docs/MODELS.md, "Object removal").

Usage:
  eval_inpaint.py masks  CASES.json PHOTOS/ OUT/  SAM_ENCODER SAM_DECODER
  eval_inpaint.py fill   CASES.json PHOTOS/ OUT/  METHOD [METHOD ...]
  eval_inpaint.py synth  CASES.json PHOTOS/ OUT/
  eval_inpaint.py score  CASES.json PHOTOS/ OUT/  METHOD [METHOD ...]
  eval_inpaint.py sheet  CASES.json PHOTOS/ OUT/  METHOD [METHOD ...]

Needs numpy, onnxruntime, opencv-python-headless, scipy (and torch + lpips
for `score`). Environment: MIGAN (migan_pipeline_v2.onnx), PATCH_FILL (the `patch_fill` example of rawtonic-engine, built
with --release).

CASES.json: [{"id", "photo", "box": [x0, y0, x1, y1]} or "click": [x, y]],
in percent of the photo; PHOTOS/<photo>.png are 8- or 16-bit sRGB renders.

- masks: the object mask as Remove's Detect objects makes it: SAM 2.1 in
  the app's two-pass flow (run_sam_decoder), made binary, grown by 0.5 % and
  feathered by 0.25 % of the short side (the quick-eraser defaults
  grow = feather = 50). Saves OUT/masks/<id>.png.
- synth: holes with known content: each case's mask shape moved to a
  structured place (most edges among 64 random positions) of the next
  case's photo. Saves OUT/synth/<id>.png (mask) and its photo name.
- fill: runs the methods on the real cases (OUT/real/<method>/<id>.png)
  and on the synthetic holes (OUT/synthfill/<method>/<id>.png), crops only.
  Methods:
    migan      MI-GAN alone (run_inpainting's crop 1.5 x the hole around
               it, the authors' MI-GAN pipeline, blended by the mask)
    patch      patch_fill without a guide on the same crop
    hybrid     patch_fill guided by the MI-GAN fill
    hybrid:K=V,... the same with patch_fill parameters; harness options:
               ctx=F widens the pipeline's own crop to F x the hole (the
               app uses 2.5), pad, th, tile, grain
- score: LPIPS (AlexNet), PSNR and mean Lab difference in the hole of the
  synthetic cases against the photo, and the hole's detail (high-pass
  energy) against the photo's, per method; time per fill.
- sheet: side-by-side crops of the real cases (OUT/sheets/<id>.jpg).
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import cv2
import numpy as np
from scipy import ndimage

SAM = 1024


def load_photo(photos, name):
    img = cv2.imread(str(Path(photos) / f"{name}.png"), cv2.IMREAD_UNCHANGED)
    if img.dtype == np.uint16:
        img = np.round(img.astype(np.float32) / 257.0).astype(np.uint8)
    return cv2.cvtColor(img[:, :, :3], cv2.COLOR_BGR2RGB)


def save_rgb(path, rgb):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    cv2.imwrite(str(path), cv2.cvtColor(rgb, cv2.COLOR_RGB2BGR))


def resize_f32(arr, w, h):
    return cv2.resize(arr.astype(np.float32), (w, h), interpolation=cv2.INTER_LINEAR)


class Sam:
    def __init__(self, encoder, decoder):
        import onnxruntime as ort

        self.enc = ort.InferenceSession(encoder, providers=["CPUExecutionProvider"])
        self.dec = ort.InferenceSession(decoder, providers=["CPUExecutionProvider"])

    def encode(self, rgb):
        h, w = rgb.shape[:2]
        self.w, self.h = w, h
        x = cv2.resize(rgb, (SAM, SAM), interpolation=cv2.INTER_LINEAR).transpose(2, 0, 1)[None].copy()
        embed, f0, f1 = self.enc.run(None, {"image": x})
        self.sx, self.sy = SAM / w, SAM / h
        self.feats = {"image_embed": embed, "high_res_feats_0": f0, "high_res_feats_1": f1}

    def decode(self, points, labels, mask_input=None):
        feeds = dict(self.feats)
        feeds |= {
            "point_coords": np.array([[[x * self.sx, y * self.sy] for x, y in points]], np.float32),
            "point_labels": np.array([labels], np.float32),
            "mask_input": np.zeros((1, 1, 256, 256), np.float32) if mask_input is None else mask_input[None, None].astype(np.float32),
            "has_mask_input": np.array([0.0 if mask_input is None else 1.0], np.float32),
            "orig_im_size": np.array([self.h, self.w], np.float32),
        }
        masks, _, _ = self.dec.run(None, feeds)
        return masks[0, 0]

    def app_mask(self, start, end):
        """run_sam_decoder() of src-tauri/src/ai_processing.rs (two passes)."""
        if start == end:
            points, labels = [start], [1.0]
        else:
            points, labels = [start, end], [2.0, 3.0]
        mask_input = None
        for i in range(2):
            binary = self.decode(points, labels, mask_input) > 0
            if i == 1 or not binary.any():
                return binary
            h, w = binary.shape
            ys, xs = np.nonzero(binary)
            min_x, max_x, min_y, max_y = xs.min(), xs.max(), ys.min(), ys.max()
            dt_in = ndimage.distance_transform_edt(binary)
            pos = np.argmax(dt_in)
            max_in = dt_in.flat[pos]
            dt_out = ndimage.distance_transform_edt(~binary)
            outside = np.ones_like(binary)
            outside[min_y:max_y + 1, min_x:max_x + 1] = False
            dt_out[outside] = 0
            neg = np.argmax(dt_out)
            points = [(pos % w, pos // w), (neg % w, neg // w), (min_x, min_y), (max_x, max_y)]
            labels = [1.0, 0.0, 2.0, 3.0]
            variance = max(binary.sum() / 4.0, 1.0)
            gaus = np.where(binary, np.exp(-((dt_in - max_in) ** 2) / variance), 0.0)
            signed = np.where(binary, 15.0, -15.0)
            m, g = resize_f32(signed, 256, 256), resize_f32(gaus, 256, 256)
            g = np.where(g <= 0, 1.0, g)
            mask_input = m * g


def removal_mask(binary):
    """quick-eraser: binary, grown 0.5 % and feathered 0.25 % of the short side."""
    h, w = binary.shape
    base = min(w, h)
    grow = int(round(0.5 * base * 0.01))
    m = binary.astype(np.uint8) * 255
    if grow > 0:
        k = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (2 * grow + 1, 2 * grow + 1))
        m = cv2.dilate(m, k)
    sigma = 0.5 * base * 0.005
    if sigma > 0.01:
        m = cv2.GaussianBlur(m, (0, 0), sigma)
    return m


def crop_box(mask, w, h, pad=1.5):
    """The crop run_inpainting gives the model: 1.5 x the hole on each side."""
    ys, xs = np.nonzero(mask)
    x0m, x1m, y0m, y1m = xs.min(), xs.max(), ys.min(), ys.max()
    mw, mh = x1m - x0m + 1, y1m - y0m + 1
    px, py = max(128, int(mw * pad)), max(128, int(mh * pad))
    x0, y0 = max(0, x0m - px), max(0, y0m - py)
    x1, y1 = min(w - 1, x1m + px), min(h - 1, y1m + py)
    return x0, y0, x1 + 1, y1 + 1


_sessions = {}


def session(path):
    import onnxruntime as ort

    if path not in _sessions:
        _sessions[path] = ort.InferenceSession(path, providers=["CPUExecutionProvider"])
    return _sessions[path]


def migan_raw(img, hole, ctx=0.0):
    """The MI-GAN pipeline's output on a crop (uint8 RGB), hole = bool.
    The pipeline crops tightly around the hole by itself; with `ctx` its
    crop is widened to `ctx` x the hole by four 2 x 2 px marks at that
    box's corners (filled by the model, then put back)."""
    s = session(os.environ["MIGAN"])
    x = img.transpose(2, 0, 1)[None].copy()
    hole_in = hole.copy()
    if ctx > 0:
        h, w = hole.shape
        ys, xs = np.nonzero(hole)
        cx, cy = (xs.min() + xs.max()) / 2, (ys.min() + ys.max()) / 2
        hw, hh = (xs.max() - xs.min() + 1) * ctx / 2, (ys.max() - ys.min() + 1) * ctx / 2
        bx0, bx1 = int(max(0, cx - hw)), int(min(w - 2, cx + hw))
        by0, by1 = int(max(0, cy - hh)), int(min(h - 2, cy + hh))
        for bx, by in ((bx0, by0), (bx1, by0), (bx0, by1), (bx1, by1)):
            hole_in[by:by + 2, bx:bx + 2] = True
    m = np.where(hole_in, 0, 255).astype(np.uint8)[None, None]
    out = s.run(None, {"image": x, "mask": m})[0][0].transpose(1, 2, 0)
    if ctx > 0:
        out = np.where(hole[..., None], out, img)
    return out


def gen_fill(photo, mask, th, size, pad):
    """MI-GAN's generator alone (MIGAN_GEN: the pipeline's generator
    subgraph) on a square crop around the hole, scaled to size x size."""
    s = session(os.environ["MIGAN_GEN"])
    h, w = mask.shape
    hole_full = mask > th
    ys, xs = np.nonzero(hole_full)
    bx0, bx1, by0, by1 = xs.min(), xs.max() + 1, ys.min(), ys.max() + 1
    side = int(max(bx1 - bx0, by1 - by0) * (1 + 2 * pad))
    side = max(side, 256)
    sw, sh = min(side, w), min(side, h)
    cx, cy = (bx0 + bx1) // 2, (by0 + by1) // 2
    x0 = int(np.clip(cx - sw // 2, 0, w - sw))
    y0 = int(np.clip(cy - sh // 2, 0, h - sh))
    img = photo[y0:y0 + sh, x0:x0 + sw]
    hole = hole_full[y0:y0 + sh, x0:x0 + sw]
    small = cv2.resize(img, (size, size), interpolation=cv2.INTER_AREA).astype(np.float32)
    known = cv2.resize((~hole).astype(np.float32), (size, size), interpolation=cv2.INTER_AREA)
    known = (known > 0.999).astype(np.float32)
    x = np.concatenate([known[None] - 0.5, (small.transpose(2, 0, 1) / 127.5 - 1) * known[None]], 0)[None]
    out = s.run(None, {s.get_inputs()[0].name: x.astype(np.float32)})[0][0]
    out = np.clip((out.transpose(1, 2, 0) * 0.5 + 0.5) * 255, 0, 255).astype(np.float32)
    big = cv2.resize(out, (sw, sh), interpolation=cv2.INTER_CUBIC)
    filled = np.where(hole[..., None], big, img.astype(np.float32))
    return (x0, y0, x0 + sw, y0 + sh), np.clip(filled, 0, 255).astype(np.uint8)


def migan_tiled(img, hole, tile):
    """The pipeline on the whole hole, then again on tiles of the hole (at
    most `tile` pixels square, overlapping by a quarter) so that each runs
    near the photo's own resolution, with the rest of the fill as context;
    tiles blend with a feathered window."""
    cur = migan_raw(img, hole).astype(np.float32)
    ys, xs = np.nonzero(hole)
    x0, x1, y0, y1 = xs.min(), xs.max() + 1, ys.min(), ys.max() + 1
    if max(x1 - x0, y1 - y0) <= tile:
        return cur.astype(np.uint8)
    step = tile * 3 // 4
    ov = tile - step
    for ty in range(y0, y1, step):
        for tx in range(x0, x1, step):
            tx1, ty1 = min(tx + tile, x1), min(ty + tile, y1)
            sub = np.zeros_like(hole)
            sub[ty:ty1, tx:tx1] = hole[ty:ty1, tx:tx1]
            if not sub.any():
                continue
            out = migan_raw(np.round(cur).astype(np.uint8), sub).astype(np.float32)
            # Feathered window: ramps over the overlap on every side.
            wy = np.minimum(np.minimum(np.arange(ty, ty1) - ty + 1, ty1 - np.arange(ty, ty1)) / ov, 1.0)
            wx = np.minimum(np.minimum(np.arange(tx, tx1) - tx + 1, tx1 - np.arange(tx, tx1)) / ov, 1.0)
            # No ramp at the hole's own edge.
            if ty == y0:
                wy[: ov] = np.maximum(wy[: ov], 1.0)
            if ty1 == y1:
                wy[-ov:] = 1.0
            if tx == x0:
                wx[: ov] = 1.0
            if tx1 == x1:
                wx[-ov:] = 1.0
            wgt = (wy[:, None] * wx[None, :])[..., None] * sub[ty:ty1, tx:tx1, None]
            cur[ty:ty1, tx:tx1] = out[ty:ty1, tx:tx1] * wgt + cur[ty:ty1, tx:tx1] * (1 - wgt)
    return np.clip(np.round(cur), 0, 255).astype(np.uint8)


def match_grain(filled, img, hole, seed=0):
    """Adds noise to the fill so that its fine detail (high-pass energy) is
    that of a ring of the photo around the hole."""
    f = filled.astype(np.float32)
    g = img.astype(np.float32)
    ring = cv2.dilate(hole.astype(np.uint8), np.ones((31, 31), np.uint8)).astype(bool) & ~hole
    hp = lambda a: a - cv2.GaussianBlur(a, (0, 0), 1.0)
    target = hp(g)[ring].std(axis=0)
    have = hp(f)[hole].std(axis=0)
    add = np.sqrt(np.maximum(target ** 2 - have ** 2, 0))
    rng = np.random.default_rng(seed)
    n = rng.standard_normal(hole.shape).astype(np.float32)
    n = hp(n) / max(hp(n).std(), 1e-6)
    f = f + np.where(hole[..., None], n[..., None] * add[None, None, :], 0)
    return np.clip(np.round(f), 0, 255).astype(np.uint8)


_hifill = None


def hifill_raw(img, hole):
    """HiFill (Huawei, BSD-3 weights in its repository; HIFILL: hifill.pb):
    its GPU_CPU/test.py pre- and post-processing (512 px generator, then
    contextual residual aggregation at `multiple` x 512)."""
    global _hifill
    import tensorflow as tf

    if _hifill is None:
        g = tf.Graph()
        with g.as_default():
            gd = tf.compat.v1.GraphDef()
            gd.ParseFromString(open(os.environ["HIFILL"], "rb").read())
            tf.import_graph_def(gd, name="")
        sess = tf.compat.v1.Session(graph=g)
        _hifill = (sess, [g.get_tensor_by_name(n) for n in ("img:0", "mask:0", "inpainted:0", "attention:0", "mask_processed:0")])
    sess, (img_ph, mask_ph, inp, att, mproc) = _hifill
    size, att_size = 512, 32
    h, w = hole.shape
    multiple = int(min(8, max(1, np.ceil(max(h, w) / size))))
    raw_mask = np.where(hole, 0, 255).astype(np.float32)[..., None].repeat(3, 2) / 255.0
    raw = img.astype(np.float32)
    big = size * multiple
    large_img = cv2.resize(raw, (big, big), interpolation=cv2.INTER_LINEAR)
    large_mask = cv2.resize(raw_mask, (big, big), interpolation=cv2.INTER_NEAREST)
    small_img = large_img.reshape(size, multiple, size, multiple, 3).mean(axis=(1, 3))
    small_mask = 1.0 - cv2.resize(raw_mask, (size, size), interpolation=cv2.INTER_NEAREST)
    res, attention, m512 = sess.run([inp, att, mproc], {img_ph: [small_img], mask_ph: [small_mask[:, :, 0:1]]})
    low_base = cv2.resize(res[0].astype(np.float32), (big, big), interpolation=cv2.INTER_LINEAR)
    low_large = cv2.resize(small_img, (big, big), interpolation=cv2.INTER_LINEAR)
    residual = (large_img - low_large) * large_mask
    k = multiple * size // att_size
    r = residual.reshape(att_size, k, att_size, k, 3).transpose(0, 2, 1, 3, 4).reshape(1, att_size * att_size, -1)
    r = np.matmul(attention[0], r).reshape(att_size, att_size, k, k, 3).transpose(0, 2, 1, 3, 4).reshape(big, big, 3)
    out = np.clip(low_base + r, 0, 255)
    out = cv2.resize(out, (w, h), interpolation=cv2.INTER_LINEAR)
    return np.where(hole[..., None], out, raw).astype(np.uint8)


def patch_raw(img, hole, guide, params, tmp):
    tmp.mkdir(parents=True, exist_ok=True)
    save_rgb(tmp / "img.png", img)
    cv2.imwrite(str(tmp / "mask.png"), hole.astype(np.uint8) * 255)
    args = [os.environ["PATCH_FILL"], str(tmp / "img.png"), str(tmp / "mask.png"), str(tmp / "out.png")]
    if guide is not None:
        save_rgb(tmp / "guide.png", guide)
        args.append(str(tmp / "guide.png"))
    args += params
    subprocess.run(args, check=True, capture_output=True)
    return cv2.cvtColor(cv2.imread(str(tmp / "out.png")), cv2.COLOR_BGR2RGB)


def run_method(method, photo, mask, tmp):
    """Fills `photo` where `mask` > 0 like the app; returns (crop box, filled crop, seconds)."""
    h, w = mask.shape
    name, _, opts = method.partition(":")
    params = [kv for kv in opts.split(",") if kv]
    # Options of the harness itself (the rest go to patch_fill).
    keys = ("pad", "th", "size", "tile", "grain", "ctx")
    own = {k: float(v) for k, v in (kv.split("=") for kv in params if kv.split("=")[0] in keys)}
    params = [kv for kv in params if kv.split("=")[0] not in keys]
    if name == "gen":
        t = time.perf_counter()
        (x0, y0, x1, y1), filled = gen_fill(photo, mask, own.get("th", 0), int(own.get("size", 512)), own.get("pad", 0.5))
        secs = time.perf_counter() - t
        m = mask[y0:y1, x0:x1]
        img = photo[y0:y1, x0:x1]
        alpha = (m.astype(np.float32) / 255.0)[..., None]
        out = filled.astype(np.float32) * alpha + img.astype(np.float32) * (1 - alpha)
        return (x0, y0, x1, y1), np.round(out).astype(np.uint8), secs
    x0, y0, x1, y1 = crop_box(mask, w, h, own.get("pad", 1.5))
    img = photo[y0:y1, x0:x1]
    m = mask[y0:y1, x0:x1]
    hole = m > own.get("th", 0)
    t = time.perf_counter()
    if name == "migan":
        tile = int(own.get("tile", 0))
        filled = migan_tiled(img, hole, tile) if tile else migan_raw(img, hole, own.get("ctx", 0))
    elif name == "hifill":
        filled = hifill_raw(img, hole)
    elif name == "patch":
        filled = patch_raw(img, hole, None, params, tmp)
    elif name == "hybrid":
        filled = patch_raw(img, hole, migan_raw(img, hole, own.get("ctx", 0)), params, tmp)
    else:
        raise SystemExit(f"unknown method {method}")
    if own.get("grain"):
        filled = match_grain(filled, img, hole)
    secs = time.perf_counter() - t
    alpha = (m.astype(np.float32) / 255.0)[..., None]
    out = filled.astype(np.float32) * alpha + img.astype(np.float32) * (1 - alpha)
    return (x0, y0, x1, y1), np.round(out).astype(np.uint8), secs


def view_box(mask, w, h, margin=0.6):
    ys, xs = np.nonzero(mask)
    x0, x1, y0, y1 = xs.min(), xs.max(), ys.min(), ys.max()
    mw, mh = x1 - x0 + 1, y1 - y0 + 1
    px, py = int(mw * margin) + 16, int(mh * margin) + 16
    return max(0, x0 - px), max(0, y0 - py), min(w, x1 + px + 1), min(h, y1 + py + 1)


def main():
    stage, cases_path, photos, out = sys.argv[1:5]
    rest = sys.argv[5:]
    cases = json.load(open(cases_path))
    out = Path(out)
    if stage == "masks":
        sam = Sam(rest[0], rest[1])
        for c in cases:
            path = out / "masks" / f"{c['id']}.png"
            if path.exists():
                continue
            rgb = load_photo(photos, c["photo"])
            h, w = rgb.shape[:2]
            sam.encode(rgb)
            if "click" in c:
                p = (c["click"][0] * w / 100, c["click"][1] * h / 100)
                binary = sam.app_mask(p, p)
            else:
                b = c["box"]
                binary = sam.app_mask((b[0] * w / 100, b[1] * h / 100), (b[2] * w / 100, b[3] * h / 100))
            path.parent.mkdir(parents=True, exist_ok=True)
            cv2.imwrite(str(path), removal_mask(binary))
            print(c["id"], int(binary.sum()), flush=True)
    elif stage == "synth":
        rng = np.random.default_rng(0)
        meta = {}
        for i, c in enumerate(cases):
            src = cv2.imread(str(out / "masks" / f"{c['id']}.png"), cv2.IMREAD_GRAYSCALE)
            target = cases[(i + 1) % len(cases)]
            rgb = load_photo(photos, target["photo"])
            h, w = rgb.shape[:2]
            ys, xs = np.nonzero(src)
            shape = src[ys.min():ys.max() + 1, xs.min():xs.max() + 1]
            scale = min(w, h) / min(src.shape)
            sh, sw = shape.shape
            nw, nh = max(8, int(sw * scale)), max(8, int(sh * scale))
            # At most 40 % of the photo's width and height.
            f = min(1.0, 0.4 * w / nw, 0.4 * h / nh)
            nw, nh = max(8, int(nw * f)), max(8, int(nh * f))
            shape = cv2.resize(shape, (nw, nh), interpolation=cv2.INTER_LINEAR)
            grey = cv2.cvtColor(rgb, cv2.COLOR_RGB2GRAY).astype(np.float32)
            edges = np.hypot(cv2.Sobel(grey, cv2.CV_32F, 1, 0), cv2.Sobel(grey, cv2.CV_32F, 0, 1))
            best = None
            for _ in range(64):
                x = int(rng.integers(0, w - nw))
                y = int(rng.integers(0, h - nh))
                e = float((edges[y:y + nh, x:x + nw] * (shape > 0)).sum() / max((shape > 0).sum(), 1))
                if best is None or e > best[0]:
                    best = (e, x, y)
            _, x, y = best
            mask = np.zeros((h, w), np.uint8)
            mask[y:y + nh, x:x + nw] = shape
            p = out / "synth" / f"{c['id']}.png"
            p.parent.mkdir(parents=True, exist_ok=True)
            cv2.imwrite(str(p), mask)
            meta[c["id"]] = target["photo"]
        json.dump(meta, open(out / "synth" / "photos.json", "w"), indent=1)
    elif stage == "fill":
        times = {}
        tpath = out / "times.json"
        if tpath.exists():
            times = json.load(open(tpath))
        synth = json.load(open(out / "synth" / "photos.json")) if (out / "synth" / "photos.json").exists() else {}
        for method in rest:
            tag = method.replace(":", "_").replace(",", "_").replace("=", "")
            for kind in ("real", "synthfill"):
                for c in cases:
                    if kind == "real":
                        photo_name, mask_path = c["photo"], out / "masks" / f"{c['id']}.png"
                    else:
                        if c["id"] not in synth:
                            continue
                        photo_name, mask_path = synth[c["id"]], out / "synth" / f"{c['id']}.png"
                    dest = out / kind / tag / f"{c['id']}.png"
                    if dest.exists():
                        continue
                    photo = load_photo(photos, photo_name)
                    mask = cv2.imread(str(mask_path), cv2.IMREAD_GRAYSCALE)
                    box, crop, secs = run_method(method, photo, mask, out / "tmp")
                    full = photo.copy()
                    x0, y0, x1, y1 = box
                    full[y0:y1, x0:x1] = crop
                    v = view_box(mask, mask.shape[1], mask.shape[0])
                    save_rgb(dest, full[v[1]:v[3], v[0]:v[2]])
                    times.setdefault(tag, {}).setdefault(kind, {})[c["id"]] = secs
                    print(kind, tag, c["id"], f"{secs:.2f}s", flush=True)
                    json.dump(times, open(tpath, "w"), indent=1)
    elif stage == "score":
        import lpips
        import torch

        net = lpips.LPIPS(net="alex", verbose=False)
        synth = json.load(open(out / "synth" / "photos.json"))
        times = json.load(open(out / "times.json"))
        rows = {}
        for method in rest:
            tag = method.replace(":", "_").replace(",", "_").replace("=", "")
            vals = []
            for c in cases:
                photo = load_photo(photos, synth[c["id"]])
                mask = cv2.imread(str(out / "synth" / f"{c['id']}.png"), cv2.IMREAD_GRAYSCALE)
                v = view_box(mask, mask.shape[1], mask.shape[0])
                gt = photo[v[1]:v[3], v[0]:v[2]]
                res = cv2.cvtColor(cv2.imread(str(out / "synthfill" / tag / f"{c['id']}.png")), cv2.COLOR_BGR2RGB)
                hole = mask[v[1]:v[3], v[0]:v[2]] > 127
                # LPIPS on the crop (at most 1024 px on the long side).
                s = min(1.0, 1024 / max(gt.shape[:2]))
                a = cv2.resize(gt, None, fx=s, fy=s, interpolation=cv2.INTER_AREA) if s < 1 else gt
                b = cv2.resize(res, None, fx=s, fy=s, interpolation=cv2.INTER_AREA) if s < 1 else res
                ta = torch.from_numpy(a).permute(2, 0, 1)[None].float() / 127.5 - 1
                tb = torch.from_numpy(b).permute(2, 0, 1)[None].float() / 127.5 - 1
                with torch.no_grad():
                    lp = float(net(ta, tb))
                diff = (gt.astype(np.float32) - res.astype(np.float32))[hole]
                psnr = 10 * np.log10(255.0 ** 2 / max(float((diff ** 2).mean()), 1e-6))
                lab_a = cv2.cvtColor(gt, cv2.COLOR_RGB2LAB).astype(np.float32)
                lab_b = cv2.cvtColor(res, cv2.COLOR_RGB2LAB).astype(np.float32)
                de = float(np.linalg.norm((lab_a - lab_b)[hole] * [100 / 255, 1, 1], axis=1).mean())

                def detail(img):
                    g = cv2.cvtColor(img, cv2.COLOR_RGB2GRAY).astype(np.float32)
                    hp = g - cv2.GaussianBlur(g, (0, 0), 2.0)
                    return float(np.abs(hp[hole]).mean())

                ratio = detail(res) / max(detail(gt), 1e-3)
                vals.append((lp, psnr, de, ratio))
            arr = np.array(vals)
            t = times.get(tag, {}).get("synthfill", {})
            rows[tag] = {
                "lpips": float(arr[:, 0].mean()),
                "psnr": float(arr[:, 1].mean()),
                "lab_de": float(arr[:, 2].mean()),
                "detail_ratio": float(np.median(arr[:, 3])),
                "seconds_median": float(np.median(list(t.values()))) if t else None,
                "per_case_lpips": {c["id"]: float(v[0]) for c, v in zip(cases, vals)},
            }
            print(f"{tag:40s} LPIPS {rows[tag]['lpips']:.4f}  PSNR {rows[tag]['psnr']:.2f}  dE {rows[tag]['lab_de']:.2f}  detail {rows[tag]['detail_ratio']:.2f}  t {rows[tag]['seconds_median']}")
        json.dump(rows, open(out / "scores.json", "w"), indent=1)
    elif stage == "sheet":
        for c in cases:
            tiles = []
            mask = cv2.imread(str(out / "masks" / f"{c['id']}.png"), cv2.IMREAD_GRAYSCALE)
            photo = load_photo(photos, c["photo"])
            v = view_box(mask, mask.shape[1], mask.shape[0])
            orig = photo[v[1]:v[3], v[0]:v[2]].copy()
            edge = cv2.Canny((mask[v[1]:v[3], v[0]:v[2]] > 127).astype(np.uint8) * 255, 50, 150) > 0
            orig[cv2.dilate(edge.astype(np.uint8), np.ones((3, 3), np.uint8)) > 0] = (255, 0, 255)
            tiles.append(("photo + mask", orig))
            for method in rest:
                tag = method.replace(":", "_").replace(",", "_").replace("=", "")
                img = cv2.cvtColor(cv2.imread(str(out / "real" / tag / f"{c['id']}.png")), cv2.COLOR_BGR2RGB)
                tiles.append((tag, img))
            th = 520
            row = []
            for label, t in tiles:
                s = th / t.shape[0]
                t = cv2.resize(t, (max(1, int(t.shape[1] * s)), th), interpolation=cv2.INTER_AREA)
                t = cv2.copyMakeBorder(t, 28, 4, 4, 4, cv2.BORDER_CONSTANT, value=(30, 30, 30))
                cv2.putText(t, label, (6, 20), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (255, 255, 0), 1)
                row.append(t)
            save_rgb(out / "sheets" / f"{c['id']}.jpg", np.concatenate(row, axis=1))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
