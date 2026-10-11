#!/usr/bin/env python3
"""Compare Rawtonic's ONNX models on the CPU against a GPU execution provider.

Every model file of the pack in MODELS runs on the CPU and on the provider
with the same inputs, prepared from the JPEGs in IMAGES as the app prepares
them (license-clean samples only, e.g. renders of raw.pixls.us CC0 files).
Prints and writes per image: mask IoU, largest and mean difference, and the
nodes each provider ran (from ONNX Runtime's profile); timings are recorded
but only mean something on real hardware. docs/decisions/0011-gpu-ai.md.

Usage:
  uv run --with onnxruntime==1.30.0 --with onnxruntime-ep-webgpu==0.4.0 --with numpy --with pillow \\
      python -I compare_providers.py MODELS IMAGES OUT.json [--ep webgpu|cuda] [--lib PLUGIN] \\
      [--limit N] [--only birefnet_subject,...]
--lib is the plugin library (onnxruntime_ep_webgpu.get_library_path()).
"""
import argparse, glob, json, os, sys, time, tempfile
import numpy as np
import onnxruntime as ort
from PIL import Image

MEAN = np.array([0.485, 0.456, 0.406], np.float32)
STD = np.array([0.229, 0.224, 0.225], np.float32)


def imagenet(img, size):
    a = np.asarray(img.convert("RGB").resize((size, size), Image.BILINEAR), np.float32) / 255.0
    return ((a - MEAN) / STD).transpose(2, 0, 1)[None].astype(np.float32)


def iou(a, b):
    u = np.logical_or(a, b).sum()
    return 1.0 if u == 0 else float(np.logical_and(a, b).sum() / u)


def cosine(a, b):
    a, b = a.ravel().astype(np.float64), b.ravel().astype(np.float64)
    return float(a @ b / (np.linalg.norm(a) * np.linalg.norm(b) + 1e-30))


def err(a, b):
    a, b = a.astype(np.float64), b.astype(np.float64)
    rng = float(a.max() - a.min()) or 1.0
    d = np.abs(a - b)
    return {"max_abs": float(d.max()), "mean_abs": float(d.mean()), "max_rel_range": float(d.max() / rng),
            "finite": bool(np.isfinite(b).all())}


def inputs_for(name, img, rng):
    if name == "birefnet_subject":
        return {"pixel_values": imagenet(img, 1024)}
    if name == "skyseg_upernet_convnext_t":
        return {"pixel_values": imagenet(img, 512)}
    if name == "depth_anything_v2_vits":
        return {"l_x_": imagenet(img, 518)}
    if name == "siglip2_vision":
        a = np.asarray(img.convert("RGB").resize((224, 224), Image.BICUBIC), np.float32) / 255.0
        return {"pixel_values": ((a - 0.5) / 0.5).transpose(2, 0, 1)[None].astype(np.float32)}
    if name == "sam2.1_tiny_encoder":
        a = np.asarray(img.convert("RGB").resize((1024, 1024), Image.BILINEAR), np.uint8)
        return {"image": a.transpose(2, 0, 1)[None].copy()}
    if name == "migan_pipeline_v2":
        a = np.asarray(img.convert("RGB").resize((768, 512), Image.BILINEAR), np.uint8)
        m = np.full((1, 1, 512, 768), 255, np.uint8)
        m[:, :, 200:300, 330:450] = 0
        return {"image": a.transpose(2, 0, 1)[None].copy(), "mask": m}
    if name == "face_detection_yunet_2023mar":
        a = np.asarray(img.convert("RGB").resize((640, 640), Image.BILINEAR), np.float32)[:, :, ::-1]
        return {"input": a.transpose(2, 0, 1)[None].copy()}
    if name in ("face_mesh_v2", "selfie_multiclass_256"):
        a = np.asarray(img.convert("RGB").resize((256, 256), Image.BILINEAR), np.float32) / 255.0
        return {"image": a[None].copy()}
    if name == "nlmf":
        # A Bayer-like mosaic from the photo: linear RGB on a 640 px crop,
        # sampled R G B G, plus shot/read noise and its standard deviation
        # (what rawtonic-denoise feeds the network).
        a = np.asarray(img.convert("RGB").resize((640, 640), Image.BILINEAR), np.float32) / 255.0
        lin = np.where(a <= 0.04045, a / 12.92, ((a + 0.055) / 1.055) ** 2.4) * 0.5
        planes = np.stack([lin[0::2, 0::2, 0], lin[0::2, 1::2, 1], lin[1::2, 1::2, 2], lin[1::2, 0::2, 1]])
        shot, read = 2e-3, 1e-5
        sd = np.sqrt(shot * planes + read)
        noisy = np.clip(planes + rng.standard_normal(planes.shape).astype(np.float32) * sd, 0, 1)
        sd = np.sqrt(shot * np.clip(noisy, 0, 1) + read)
        return {"raw_with_noise": np.concatenate([noisy, sd])[None].astype(np.float32)}
    raise KeyError(name)


def metrics(name, cpu, gpu, feeds):
    if name in ("birefnet_subject", "skyseg_upernet_convnext_t"):
        return {"iou_0.5": iou(cpu[0] > 0.5, gpu[0] > 0.5), **err(cpu[0], gpu[0])}
    if name == "selfie_multiclass_256":
        return {"argmax_agree": float((cpu[0].argmax(-1) == gpu[0].argmax(-1)).mean()), **err(cpu[0], gpu[0])}
    if name in ("siglip2_vision",):
        return {"cosine": cosine(cpu[0], gpu[0]), **err(cpu[0], gpu[0])}
    if name == "sam2.1_tiny_encoder":
        return {"cosine": min(cosine(c, g) for c, g in zip(cpu, gpu)), **err(cpu[0], gpu[0])}
    if name == "migan_pipeline_v2":
        hole = feeds["mask"][0, 0] == 0
        d = np.abs(cpu[0][0].astype(np.int32) - gpu[0][0].astype(np.int32))
        return {"hole_mean_abs_u8": float(d[:, hole].mean()), "max_abs_u8": int(d.max()),
                "outside_max_abs_u8": int(d[:, ~hole].max())}
    if name == "face_detection_yunet_2023mar":
        return {"max_abs_any": max(float(np.abs(c - g).max()) for c, g in zip(cpu, gpu)),
                "finite": all(np.isfinite(g).all() for g in gpu)}
    if name == "face_mesh_v2":
        return {"landmark_max_abs_px": float(np.abs(cpu[0] - gpu[0]).max()),
                "presence_abs": float(np.abs(cpu[1] - gpu[1]).max())}
    return err(cpu[0], gpu[0])


def sam_decoder_check(dec_cpu, dec_gpu, emb):
    feeds = {"image_embed": emb[0], "high_res_feats_0": emb[1], "high_res_feats_1": emb[2],
             "point_coords": np.array([[[512.0, 512.0], [0.0, 0.0]]], np.float32),
             "point_labels": np.array([[1.0, -1.0]], np.float32),
             "mask_input": np.zeros((1, 1, 256, 256), np.float32), "has_mask_input": np.zeros(1, np.float32),
             "orig_im_size": np.array([1024.0, 1536.0], np.float32)}
    c = dec_cpu.run(None, feeds)
    g = dec_gpu.run(None, feeds)
    return {"iou_0": iou(c[0][0, 0] > 0, g[0][0, 0] > 0), **err(c[0], g[0])}


def provider_share(profile_path):
    with open(profile_path) as f:
        events = json.load(f)
    count = {}
    for e in events:
        p = e.get("args", {}).get("provider")
        if e.get("cat") == "Node" and p and e.get("name", "").endswith("_kernel_time"):
            count[p] = count.get(p, 0) + 1
    os.remove(profile_path)
    return count


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("models"); ap.add_argument("images"); ap.add_argument("out")
    ap.add_argument("--ep", default="webgpu"); ap.add_argument("--lib")
    ap.add_argument("--limit", type=int, default=6); ap.add_argument("--only", default="")
    ap.add_argument("--options", default="", help="key=value,... EP options")
    args = ap.parse_args()
    info = {"ort": ort.__version__, "ep": args.ep}
    if args.lib:
        ort.register_execution_provider_library(args.ep, args.lib)
    devices = ort.get_ep_devices()
    info["devices"] = [{"ep": d.ep_name, "vendor": d.ep_vendor, "device_type": str(d.device.type),
                        "device_vendor": d.device.vendor, "vendor_id": d.device.vendor_id,
                        "metadata": dict(d.device.metadata), "ep_metadata": dict(d.ep_metadata)} for d in devices]
    print(json.dumps(info, indent=1), flush=True)
    names = {"webgpu": "WebGpuExecutionProvider", "cuda": "CUDAExecutionProvider", "dml": "DmlExecutionProvider"}
    chosen = [d for d in devices if d.ep_name == names[args.ep]]
    options = dict(kv.split("=", 1) for kv in args.options.split(",") if kv)
    if not chosen and args.ep != "dml":
        sys.exit(f"no {args.ep} device")

    def session(path, gpu, profile=False):
        so = ort.SessionOptions()
        so.log_severity_level = 3
        if profile:
            so.enable_profiling = True
            so.profile_file_prefix = os.path.join(tempfile.gettempdir(), "epprof")
        if gpu:
            if args.ep == "dml":
                return ort.InferenceSession(path, so, providers=["DmlExecutionProvider", "CPUExecutionProvider"])
            so.add_provider_for_devices(chosen[:1], options)
        return ort.InferenceSession(path, so, providers=None if gpu else ["CPUExecutionProvider"])

    images = sorted(glob.glob(os.path.join(args.images, "*.jpg")))[: args.limit]
    only = set(filter(None, args.only.split(",")))
    results = {"info": info, "models": {}}
    rng = np.random.default_rng(0)
    for path in sorted(glob.glob(os.path.join(args.models, "*.onnx"))):
        name = os.path.basename(path)[:-5]
        if name == "sam2.1_tiny_decoder" or (only and name not in only):
            continue
        entry = {"per_image": []}
        try:
            t = time.time(); cpu = session(path, False); entry["cpu_load_s"] = time.time() - t
            t = time.time(); gpu = session(path, True, profile=True); entry["gpu_load_s"] = time.time() - t
        except Exception as e:  # noqa: BLE001
            entry["error"] = f"session: {e}"
            results["models"][name] = entry
            print(name, entry["error"], flush=True)
            continue
        dec = None
        if name == "sam2.1_tiny_encoder":
            dpath = os.path.join(args.models, "sam2.1_tiny_decoder.onnx")
            dec = (session(dpath, False), session(dpath, True))
        cpu_times, gpu_times = [], []
        for i, img_path in enumerate(images):
            img = Image.open(img_path)
            feeds = inputs_for(name, img, rng)
            try:
                t = time.time(); c = cpu.run(None, feeds); cpu_times.append(time.time() - t)
                t = time.time(); g = gpu.run(None, feeds); gpu_times.append(time.time() - t)
                if i == 0:
                    entry["nodes_by_provider"] = provider_share(gpu.end_profiling())
                m = metrics(name, c, g, feeds)
                if dec:
                    m["decoder"] = sam_decoder_check(dec[0], dec[1], c)
            except Exception as e:  # noqa: BLE001
                m = {"error": str(e)}
            m["image"] = os.path.basename(img_path)
            entry["per_image"].append(m)
            print(name, json.dumps(m), flush=True)
        entry["cpu_run_s"] = cpu_times
        entry["gpu_run_s"] = gpu_times
        results["models"][name] = entry
        with open(args.out, "w") as f:
            json.dump(results, f, indent=1)
    with open(args.out, "w") as f:
        json.dump(results, f, indent=1)


if __name__ == "__main__":
    main()
