"""Prompted-mask quality of SAM exports on DUTS-TE with Rawtonic's preprocessing (docs/MODELS.md).

Usage: DUTS_TE=path/to/DUTS-TE eval_sam_prompts.py MODEL ENCODER DECODER OUT.json [N] [THREADS]
Needs: numpy, onnxruntime, pillow, scipy (versions as in build_model_pack.py).
MODEL: sam1 (Rawtonic's ViT-B export: uint8 photo, long side 1024, top-left
padded; four masks, the app uses mask 0) or sam2 (export_sam2.py: photo
stretched to 1024 x 1024, single mask).

Modes per image (prompts from the ground truth, in photo pixels):
- app_click / app_box: run_sam_decoder() of src-tauri/src/ai_processing.rs
  replicated (two passes: the click or box, then interior/exterior points,
  the first mask's box and its mask as mask_input). For sam1 exactly as the
  app does it (no padding point, mask_input stretched to 256 x 256);
  `app_*_fixed` (sam1 only): the same with SAM's padding point and the
  mask_input in the padded frame.
- click1 / click3: SAM's interactive protocol: the first click at the
  ground truth's innermost point, every further click at the innermost point
  of the larger error region (positive for a miss, negative for a false
  area), previous low-res logits as mask_input. Single-mask output.
- box: the ground truth's bounding box, one pass.
- click1_best (sam1 only): one click, the multimask candidate with the
  highest predicted IoU (SAM 1's recommended use for single clicks).
Metrics: IoU and boundary F (3 px tolerance) of the binary decoder mask
(before the app's guided-filter refinement, which is the same for all).
"""
import json
import os
import sys
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image
from scipy import ndimage

DUTS = Path(os.environ["DUTS_TE"])  # holds DUTS-TE-Image/ and DUTS-TE-Mask/
SIZE = 1024


def sample(n, seed=0):
    images = sorted((DUTS / "DUTS-TE-Image").glob("*.jpg"))
    idx = np.random.default_rng(seed).choice(len(images), n, replace=False)
    return [images[i] for i in sorted(idx)]


def boundary(m):
    return m & ~ndimage.binary_erosion(m, border_value=0)


def metrics(pred, gt, tol=3):
    union = (pred | gt).sum()
    iou = float((pred & gt).sum() / union) if union else 1.0
    bp, bg = boundary(pred), boundary(gt)
    if not bp.any() or not bg.any():
        return iou, float(bp.any() == bg.any())
    dist_to_g = ndimage.distance_transform_edt(~bg)
    dist_to_p = ndimage.distance_transform_edt(~bp)
    precision = float((dist_to_g[bp] <= tol).mean())
    recall = float((dist_to_p[bg] <= tol).mean())
    f = 2 * precision * recall / (precision + recall) if precision + recall else 0.0
    return iou, f


def innermost(mask):
    dt = ndimage.distance_transform_edt(mask)
    y, x = np.unravel_index(np.argmax(dt), dt.shape)
    return float(x), float(y), float(dt[y, x])


def resize_f32(arr, w, h):
    return np.array(Image.fromarray(arr.astype(np.float32), mode="F").resize((w, h), Image.BILINEAR))


class Model:
    def __init__(self, kind, encoder, decoder, threads):
        opts = ort.SessionOptions()
        if threads:
            opts.intra_op_num_threads = threads
        self.kind = kind
        self.enc = ort.InferenceSession(encoder, opts, providers=["CPUExecutionProvider"])
        self.dec = ort.InferenceSession(decoder, opts, providers=["CPUExecutionProvider"])

    def encode(self, img):
        w, h = img.size
        self.w, self.h = w, h
        if self.kind == "sam1":
            scale = SIZE / max(w, h)
            nw, nh = round(w * scale), round(h * scale)
            x = np.zeros((1, 3, SIZE, SIZE), np.uint8)
            x[0, :, :nh, :nw] = np.array(img.resize((nw, nh), Image.BILINEAR)).transpose(2, 0, 1)
            self.sx = self.sy = scale
            self.feats = {"image_embeddings": self.enc.run(None, {"input": x})[0]}
        else:
            x = np.array(img.resize((SIZE, SIZE), Image.BILINEAR)).transpose(2, 0, 1)[None].copy()
            embed, f0, f1 = self.enc.run(None, {"image": x})
            self.sx, self.sy = SIZE / w, SIZE / h
            self.feats = {"image_embed": embed, "high_res_feats_0": f0, "high_res_feats_1": f1}

    def decode(self, points, labels, mask_input=None, pad=False, best=False):
        """points in photo pixels; returns (full-size logits of mask 0, low-res logits of mask 0)."""
        coords = [[x * self.sx, y * self.sy] for x, y in points]
        labels = list(labels)
        if pad and self.kind == "sam1":
            coords.append([0.0, 0.0])
            labels.append(-1.0)
        feeds = dict(self.feats)
        feeds |= {
            "point_coords": np.array([coords], np.float32),
            "point_labels": np.array([labels], np.float32),
            "mask_input": np.zeros((1, 1, 256, 256), np.float32) if mask_input is None else mask_input[None, None].astype(np.float32),
            "has_mask_input": np.array([0.0 if mask_input is None else 1.0], np.float32),
            "orig_im_size": np.array([self.h, self.w], np.float32),
        }
        masks, iou, low = self.dec.run(None, feeds)
        if best:
            # SAM 1's multimask candidates 1-3, the one with the highest predicted IoU.
            k = 1 + int(np.argmax(iou[0, 1:]))
            return masks[0, k], low[0, k]
        return masks[0, 0], low[0, 0]


def app_decoder(model, start, end, fixed=False):
    """run_sam_decoder() of ai_processing.rs: two passes."""
    is_point = start == end
    if is_point:
        points, labels = [start], [1.0]
    else:
        points = [(min(start[0], end[0]), min(start[1], end[1])), (max(start[0], end[0]), max(start[1], end[1]))]
        labels = [2.0, 3.0]
    mask_input = None
    for i in range(2):
        full, _ = model.decode(points, labels, mask_input, pad=fixed and is_point and i == 0)
        binary = full > 0
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
        if fixed and model.kind == "sam1":
            # The low-res frame is the padded 1024 square: resize into its
            # top-left part, the padding outside the photo.
            nw, nh = round(w * model.sx / 4), round(h * model.sy / 4)
            m = np.full((256, 256), -15.0, np.float32)
            g = np.ones((256, 256), np.float32)
            m[:nh, :nw] = resize_f32(signed, nw, nh)
            g[:nh, :nw] = resize_f32(gaus, nw, nh)
        else:
            m, g = resize_f32(signed, 256, 256), resize_f32(gaus, 256, 256)
        g = np.where(g <= 0, 1.0, g)
        mask_input = m * g


def interactive(model, gt, clicks):
    points, labels, low, results = [], [], None, []
    pred = np.zeros_like(gt)
    for _ in range(clicks):
        fn, fp = gt & ~pred, pred & ~gt
        xn, yn, dn = innermost(fn) if fn.any() else (0, 0, -1)
        xp, yp, dp = innermost(fp) if fp.any() else (0, 0, -1)
        if dn >= dp:
            points.append((xn, yn)); labels.append(1.0)
        else:
            points.append((xp, yp)); labels.append(0.0)
        full, low = model.decode(points, labels, low, pad=True)
        pred = full > 0
        results.append(metrics(pred, gt))
    return results


def main():
    kind, encoder, decoder, out = sys.argv[1:5]
    n = int(sys.argv[5]) if len(sys.argv) > 5 else 300
    threads = int(sys.argv[6]) if len(sys.argv) > 6 else 0
    model = Model(kind, encoder, decoder, threads)
    rows = []
    t_enc = []
    for path in sample(n):
        img = Image.open(path).convert("RGB")
        gt = np.array(Image.open(DUTS / "DUTS-TE-Mask" / (path.stem + ".png")).convert("L")) > 127
        if gt.shape != (img.size[1], img.size[0]) or not gt.any():
            continue
        t = time.perf_counter()
        model.encode(img)
        t_enc.append(time.perf_counter() - t)
        x, y, _ = innermost(gt)
        ys, xs = np.nonzero(gt)
        box = ((float(xs.min()), float(ys.min())), (float(xs.max()), float(ys.max())))
        row = {"image": path.stem}
        row["app_click"] = metrics(app_decoder(model, (x, y), (x, y)), gt)
        row["app_box"] = metrics(app_decoder(model, *box), gt)
        if kind == "sam1":
            row["app_click_fixed"] = metrics(app_decoder(model, (x, y), (x, y), fixed=True), gt)
            row["app_box_fixed"] = metrics(app_decoder(model, *box, fixed=True), gt)
            full, _ = model.decode([(x, y)], [1.0], pad=True, best=True)
            row["click1_best"] = metrics(full > 0, gt)
        clicks = interactive(model, gt, 3)
        row["click1"], row["click3"] = clicks[0], clicks[2]
        full, _ = model.decode(list(box), [2.0, 3.0])
        row["box"] = metrics(full > 0, gt)
        rows.append(row)
        print(path.stem, {k: round(v[0], 3) for k, v in row.items() if k != "image"}, flush=True)
    Path(out).write_text(json.dumps({"rows": rows, "encode_s": t_enc}))


if __name__ == "__main__":
    main()
