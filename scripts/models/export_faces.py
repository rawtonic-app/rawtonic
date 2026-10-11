#!/usr/bin/env python3
"""Export the face models (MediaPipe, Apache-2.0) as ONNX for Rawtonic.

Sources (Google's own releases; each model card says "Licensed under Apache
License, Version 2.0"):
- `face_landmarker.task` (float16, version 1): Face Mesh V2, 478 landmarks
  from a 256 x 256 face crop (the bundle's `face_landmarks_detector.tflite`;
  its face detector and blendshape model are not used);
- `selfie_multiclass_256x256.tflite` (float32, version 1): background, hair,
  body skin, face skin, clothes and accessories per pixel.

Both are converted by tf2onnx (opset 17) and checked against the TFLite
interpreter on random input. Inputs and outputs get descriptive names:

face_mesh_v2.onnx
- input  image float32 [1, 256, 256, 3]: RGB, sRGB 8-bit / 255 (the model's
  metadata: mean 0, std 255), the face crop rotated so the eyes are level;
- output landmarks float32 [1, 1, 1, 1434]: 478 x (x, y, z) in crop pixels;
- output presence float32 [1, 1, 1, 1]: face presence logit;
- output tongue float32 [1, 1]: unused.

selfie_multiclass_256.onnx
- input  image float32 [1, 256, 256, 3]: RGB, (sRGB 8-bit - 127.5) / 127.5
  (metadata: mean 127.5, std 127.5);
- output classes float32 [1, 256, 256, 6]: logits of background, hair,
  body skin, face skin, clothes, others (accessories).

YuNet (MIT, OpenCV Zoo) is used unchanged and needs no export.

Usage:
  uv run --python 3.11 --with tensorflow==2.15.1 --with tf2onnx==1.16.1 \
      --with "numpy<2" --with onnx==1.16.2 --with onnxruntime==1.19.2 \
      python scripts/models/export_faces.py OUT_DIR [--cache CACHE_DIR]
"""
import argparse
import hashlib
import shutil
import sys
import urllib.request
import zipfile
from pathlib import Path

import numpy as np

LANDMARKER = {
    "url": "https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task",
    "sha256": "64184e229b263107bc2b804c6625db1341ff2bb731874b0bcc2fe6544e0bc9ff",
}
MULTICLASS = {
    "url": "https://storage.googleapis.com/mediapipe-models/image_segmenter/selfie_multiclass_256x256/float32/1/selfie_multiclass_256x256.tflite",
    "sha256": "c6748b1253a99067ef71f7e26ca71096cd449baefa8f101900ea23016507e0e0",
}
MESH_FILE = "face_mesh_v2.onnx"
PARSE_FILE = "selfie_multiclass_256.onnx"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fetch(source: dict, cache: Path) -> Path:
    target = cache / source["url"].rsplit("/", 1)[1]
    if not target.exists() or sha256(target) != source["sha256"]:
        with urllib.request.urlopen(source["url"]) as response, target.open("wb") as out:
            shutil.copyfileobj(response, out)
    if sha256(target) != source["sha256"]:
        sys.exit(f"{source['url']}: unexpected SHA-256")
    return target


def convert(tflite: Path, out: Path, inputs: list[str], outputs: list[str]) -> None:
    import onnx
    import tf2onnx

    model, _ = tf2onnx.convert.from_tflite(str(tflite), opset=17)
    graph = model.graph
    renames = dict(zip([i.name for i in graph.input], inputs))
    renames |= dict(zip([o.name for o in graph.output], outputs))
    for node in graph.node:
        node.input[:] = [renames.get(n, n) for n in node.input]
        node.output[:] = [renames.get(n, n) for n in node.output]
    for value in list(graph.input) + list(graph.output):
        value.name = renames.get(value.name, value.name)
        dim = value.type.tensor_type.shape.dim[0]
        dim.Clear()
        dim.dim_value = 1
    # tf2onnx numbers its internal names from counters that differ from run
    # to run; name every tensor and node by its place in the graph so two
    # exports are the same bytes.
    fixed = set(inputs) | set(outputs)
    canonical: dict[str, str] = {}
    for i, node in enumerate(graph.node):
        node.name = f"n{i}"
        for name in list(node.input) + list(node.output):
            if name and name not in fixed and name not in canonical:
                canonical[name] = f"t{len(canonical)}"
        node.input[:] = [canonical.get(n, n) for n in node.input]
        node.output[:] = [canonical.get(n, n) for n in node.output]
    for init in graph.initializer:
        init.name = canonical.get(init.name, init.name)
    ordered = sorted(graph.initializer, key=lambda t: int(t.name[1:]) if t.name[1:].isdigit() else -1)
    del graph.initializer[:]
    graph.initializer.extend(ordered)
    del graph.value_info[:]
    model.producer_name = "rawtonic export_faces.py"
    model.producer_version = ""
    onnx.checker.check_model(model)
    out.write_bytes(model.SerializeToString())


def check(tflite: Path, onnx_path: Path) -> None:
    import onnxruntime as ort
    import tensorflow as tf

    interpreter = tf.lite.Interpreter(model_path=str(tflite))
    interpreter.allocate_tensors()
    details = interpreter.get_input_details()[0]
    session = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    for seed in range(3):
        x = np.random.default_rng(seed).random(details["shape"], dtype=np.float32)
        interpreter.set_tensor(details["index"], x)
        interpreter.invoke()
        expected = [interpreter.get_tensor(o["index"]) for o in interpreter.get_output_details()]
        got = session.run(None, {session.get_inputs()[0].name: x})
        for want in expected:
            diff = min(
                float(np.abs(g.reshape(-1) - want.reshape(-1)).max()) for g in got if g.size == want.size
            )
            scale = max(1.0, float(np.abs(want).max()))
            if diff > 1e-4 * scale:
                sys.exit(f"{onnx_path.name}: differs from TFLite by {diff}")


def export(cache: Path) -> list[Path]:
    cache.mkdir(parents=True, exist_ok=True)
    task = fetch(LANDMARKER, cache)
    mesh_tflite = cache / "face_landmarks_detector.tflite"
    with zipfile.ZipFile(task) as bundle:
        mesh_tflite.write_bytes(bundle.read("face_landmarks_detector.tflite"))
    parse_tflite = fetch(MULTICLASS, cache)

    mesh = cache / MESH_FILE
    convert(mesh_tflite, mesh, ["image"], ["landmarks", "presence", "tongue"])
    check(mesh_tflite, mesh)
    parse = cache / PARSE_FILE
    convert(parse_tflite, parse, ["image"], ["classes"])
    check(parse_tflite, parse)
    return [mesh, parse]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", type=Path)
    parser.add_argument("--cache", type=Path, default=Path("model-pack-cache/faces"))
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    for path in export(args.cache):
        target = args.out / path.name
        shutil.copyfile(path, target)
        print(f"{sha256(target)}  {target.stat().st_size:>11}  {target.name}")


if __name__ == "__main__":
    main()
