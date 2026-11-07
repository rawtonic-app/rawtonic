#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Save an ONNX model with its weights as ONNX external data.

Writes OUT_DIR/NAME.onnx (the graph) and OUT_DIR/NAME.onnx.data (every
initializer of 1 KiB or more, in one file). Same graph, same weights, same
licence as the source: only the layout changes. ONNX Runtime maps the data
file instead of copying the weights into the session, so a loaded session
holds only the pages it touches (SigLIP 2's text encoder: +0.35 GB RSS
instead of +2.68 GB, notes: benchmark-results/srs-search-ceiling-2026-11-03
§ SRS2). The data file's name is stored in the graph, so the pair must keep
its names.

With --check TOKENIZER the source and the result run on the same text
prompts in ONNX Runtime (CPU) and must give bit-identical outputs.

Usage: external_data.py SOURCE.onnx OUT_DIR NAME [--check TOKENIZER.json]
"""
import sys
from pathlib import Path

import numpy as np
import onnx

PROMPTS = ["a dog", "a white car", "eine hochzeit in der kirche", "a heron beside a canal", ""]


def convert(source: Path, out: Path, name: str) -> list[Path]:
    out.mkdir(parents=True, exist_ok=True)
    graph, data = out / f"{name}.onnx", out / f"{name}.onnx.data"
    for path in (graph, data):
        if path.exists():
            sys.exit(f"{path} exists; write into an empty directory")
    model = onnx.load(str(source), load_external_data=True)
    onnx.save_model(model, str(graph), save_as_external_data=True, all_tensors_to_one_file=True,
                    location=data.name, size_threshold=1024, convert_attribute=False)
    onnx.checker.check_model(str(graph))
    return [graph, data]


def outputs(model: Path, ids: np.ndarray) -> np.ndarray:
    import onnxruntime as ort

    options = ort.SessionOptions()
    options.enable_cpu_mem_arena = False
    session = ort.InferenceSession(str(model), options, providers=["CPUExecutionProvider"])
    return session.run(None, {session.get_inputs()[0].name: ids})[0]


def check(source: Path, result: Path, tokenizer: Path) -> None:
    from tokenizers import Tokenizer

    tok = Tokenizer.from_file(str(tokenizer))
    ids = np.array([e.ids for e in tok.encode_batch(PROMPTS)], dtype=np.int64)
    a, b = outputs(source, ids), outputs(result, ids)
    if a.shape != b.shape or not np.array_equal(a, b):
        sys.exit(f"outputs differ: largest difference {np.abs(a - b).max()}")
    print(f"identical outputs for {len(PROMPTS)} prompts, shape {a.shape}")


if __name__ == "__main__":
    args = sys.argv[1:]
    tokenizer = None
    if "--check" in args:
        at = args.index("--check")
        tokenizer = Path(args[at + 1])
        del args[at:at + 2]
    if len(args) != 3:
        sys.exit(__doc__)
    paths = convert(Path(args[0]), Path(args[1]), args[2])
    if tokenizer:
        check(Path(args[0]), paths[0], tokenizer)
    for path in paths:
        print(path)
