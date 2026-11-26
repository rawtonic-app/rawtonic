#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Rewrite the Subject network (BiRefNet) for a lower working-memory peak on the CPU.

Source: birefnet_subject.onnx as export_birefnet.py writes it (BiRefNet
general checkpoint, MIT), checked against its pinned SHA-256. The result,
birefnet_subject_low_peak.onnx, has the same weights and computes the same
network with two changes to the graph (MIT asks for none; they are recorded
in docs/MODELS.md):

1. The last stage, a 1x1 convolution over the concatenation of the decoder's
   192 channels upsampled to 1024 x 1024 and the 48 channels of the input
   block, becomes the 192-channel part of that convolution at 256 x 256,
   upsampled (one channel), plus the 48-channel part with the bias. A
   bilinear resize and a 1x1 convolution commute (the resize weights sum to
   one), so only the rounding changes: on six photos the probability moved
   by at most 8.3e-7 and 0 or 1 of 1,048,576 8-bit codes changed
   (docs/benchmark-results/mem21-2026-11-26 in the notes). It drops two
   tensors of 768 and 960 MB.
2. The deformable convolutions of decoder blocks 1 and 2 (GridSample per
   kernel tap, the taps stacked, one matrix product) run in bands of
   `BAND_ROWS` output rows, so only one band's taps are alive at a time (the
   7 x 7 one stacked 49 taps of 16 MB). Per output pixel the operations and
   their order are unchanged; the output is byte for byte the same.

The app uses this graph only where the CPU runs the Subject network on a
machine with at most 16 GB of memory, in a session planned without buffer
reuse (rawtonic_onnx::MemoryPlan::LowPeak); the source graph stays every
other machine's and older builds' file.

Usage: convert_birefnet_low_peak.py SOURCE.onnx OUT_DIR
Needs: onnx 1.23.2, onnxruntime 1.30.0 (its symbolic shape inference, for the
band shapes), sympy (build_model_pack.py SUBJECT_LOW_PEAK_EXPORT). The output
repeats byte for byte.
"""
import hashlib
import os
import sys
import tempfile
from pathlib import Path

import numpy as np
import onnx
from onnx import helper, numpy_helper

SOURCE_SHA256 = "1019aa4d150a10830d95d05b0e0e08ed2703c39d3e168dea3333bb0272c16cbf"
OUT_FILE = "birefnet_subject_low_peak.onnx"
# Output rows per band, and the smallest output (rows x columns) worth banding:
# decoder block 1 (256 x 256) gets four bands, block 2 (128 x 128) two.
BAND_ROWS = 64
MIN_PIXELS = 128 * 128
INPUT_BLOCK_CHANNELS = 48


def shapes_of(path: Path) -> dict:
    """Static shapes of every tensor (ONNX Runtime's symbolic shape inference:
    the exported graph computes its shapes at run time)."""
    from onnxruntime.tools.symbolic_shape_infer import SymbolicShapeInference

    inferred = SymbolicShapeInference.infer_shapes(onnx.load(str(path)), auto_merge=True)
    return {vi.name: [d.dim_value for d in vi.type.tensor_type.shape.dim] for vi in inferred.graph.value_info}


def last_stage(model: onnx.ModelProto) -> None:
    g = model.graph
    by_out = {o: n for n in g.node for o in n.output}
    inits = {i.name: i for i in g.initializer}
    sig = by_out["subject"]
    conv = by_out[sig.input[0]]
    cat = by_out[conv.input[0]]
    res = by_out[cat.input[0]]
    assert (sig.op_type, conv.op_type, cat.op_type, res.op_type) == ("Sigmoid", "Conv", "Concat", "Resize")
    w = numpy_helper.to_array(inits[conv.input[1]])
    b = numpy_helper.to_array(inits[conv.input[2]])
    split = w.shape[1] - INPUT_BLOCK_CHANNELS
    assert w.shape == (1, 240, 1, 1) and split == 192, w.shape
    g.initializer.extend([
        numpy_helper.from_array(np.ascontiguousarray(w[:, :split]), "low_peak/out_w_decoder"),
        numpy_helper.from_array(np.ascontiguousarray(w[:, split:]), "low_peak/out_w_input"),
        numpy_helper.from_array(b, "low_peak/out_b"),
        numpy_helper.from_array(np.array([1, 1, 1024, 1024], dtype=np.int64), "low_peak/out_size"),
    ])
    decoder = helper.make_node("Conv", [res.input[0], "low_peak/out_w_decoder"], ["low_peak/decoder"],
                               name="low_peak/conv_decoder")
    decoder.attribute.extend(conv.attribute)
    up = helper.make_node("Resize", ["low_peak/decoder", res.input[1], res.input[2], "low_peak/out_size"],
                          ["low_peak/decoder_up"], name="low_peak/resize")
    up.attribute.extend(res.attribute)
    block = helper.make_node("Conv", [cat.input[1], "low_peak/out_w_input", "low_peak/out_b"], ["low_peak/input"],
                             name="low_peak/conv_input")
    block.attribute.extend(conv.attribute)
    add = helper.make_node("Add", ["low_peak/decoder_up", "low_peak/input"], ["low_peak/logit"], name="low_peak/add")
    sig.input[0] = "low_peak/logit"
    nodes = list(g.node)
    at = nodes.index(res)
    nodes[at:at + 1] = [decoder, up]
    # The input block's resize reads the 1024 x 1024 size from the upsampled tensor.
    for n in nodes:
        if n.op_type == "Shape":
            for k, x in enumerate(n.input):
                if x == res.output[0]:
                    n.input[k] = "low_peak/decoder_up"
    nodes.remove(cat)
    nodes.remove(conv)
    at = nodes.index(sig)
    nodes[at:at] = [block, add]
    del g.node[:]
    g.node.extend(nodes)


def banded_deformable_convs(model: onnx.ModelProto, shapes: dict) -> int:
    g = model.graph
    prod = {o: n for n in g.node for o in n.output}
    cons = {}
    for n in g.node:
        for x in n.input:
            cons.setdefault(x, []).append(n)
    serial = [0]

    def const(values):
        serial[0] += 1
        name = "low_peak/c%d" % serial[0]
        g.initializer.append(numpy_helper.from_array(np.array(values, dtype=np.int64), name))
        return name

    replace, drop = {}, set()
    for mm in [n for n in g.node if n.op_type == "MatMul" and "atrous_conv" in n.name]:
        cols = shapes.get(mm.input[1])
        if not cols or cols[2] < MIN_PIXELS:
            continue
        reshape = prod[mm.input[1]]
        stack = prod[reshape.input[0]]
        assert reshape.op_type == "Reshape" and stack.op_type == "Concat"
        assert [a.i for a in stack.attribute if a.name == "axis"] == [2]
        taps = []
        for u in stack.input:
            unsq = prod[u]
            assert unsq.op_type == "Unsqueeze"
            mul = prod[unsq.input[0]]
            if mul.op_type == "Mul":
                grid_sample, modulation = prod[mul.input[0]], mul.input[1]
            else:
                grid_sample, modulation, mul = mul, None, None
            assert grid_sample.op_type == "GridSample"
            taps.append((unsq, mul, grid_sample, modulation))
        removed = [stack, reshape] + [n for unsq, mul, gs, _ in taps for n in (unsq, mul, gs) if n is not None]
        for n in removed:
            for o in n.output:
                for c in cons.get(o, []):
                    assert c in removed or c is mm, "outside consumer %s of %s" % (c.name, o)
        source = taps[0][2].input[0]
        batch, channels, kernel, rows, width = shapes[stack.output[0]]
        prefix = mm.name + "/low_peak"
        nodes, bands = [], []
        for j, r0 in enumerate(range(0, rows, BAND_ROWS)):
            r1 = min(rows, r0 + BAND_ROWS)
            stacked = []
            for t, (_, mul, gs, modulation) in enumerate(taps):
                grid = "%s/grid%d_%d" % (prefix, j, t)
                nodes.append(helper.make_node("Slice", [gs.input[1], const([r0]), const([r1]), const([1])], [grid], name=grid))
                sample = "%s/sample%d_%d" % (prefix, j, t)
                node = helper.make_node("GridSample", [source, grid], [sample], name=sample)
                node.attribute.extend(gs.attribute)
                nodes.append(node)
                if modulation:
                    weight = "%s/modulation%d_%d" % (prefix, j, t)
                    nodes.append(helper.make_node("Slice", [modulation, const([r0]), const([r1]), const([2])], [weight],
                                                  name=weight))
                    product = "%s/modulated%d_%d" % (prefix, j, t)
                    nodes.append(helper.make_node("Mul", [sample, weight], [product], name=product))
                    sample = product
                tap = "%s/tap%d_%d" % (prefix, j, t)
                nodes.append(helper.make_node("Unsqueeze", [sample, const([2])], [tap], name=tap))
                stacked.append(tap)
            cat = "%s/stack%d" % (prefix, j)
            nodes.append(helper.make_node("Concat", stacked, [cat], name=cat, axis=2))
            flat = "%s/columns%d" % (prefix, j)
            nodes.append(helper.make_node("Reshape", [cat, const([batch, channels * kernel, (r1 - r0) * width])], [flat],
                                          name=flat))
            band = "%s/band%d" % (prefix, j)
            nodes.append(helper.make_node("MatMul", [mm.input[0], flat], [band], name=band))
            bands.append(band)
        nodes.append(helper.make_node("Concat", bands, [mm.output[0]], name=prefix + "/bands", axis=2))
        replace[mm.name] = nodes
        drop |= {id(n) for n in removed} | {id(mm)}
    nodes = []
    for n in g.node:
        if n.name in replace:
            nodes.extend(replace[n.name])
        elif id(n) not in drop:
            nodes.append(n)
    del g.node[:]
    g.node.extend(nodes)
    return len(replace)


def convert(source: Path, out: Path) -> Path:
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    if digest != SOURCE_SHA256:
        sys.exit(f"{source}: SHA-256 {digest}, expected {SOURCE_SHA256}")
    shapes = shapes_of(source)
    model = onnx.load(str(source))
    last_stage(model)
    banded = banded_deformable_convs(model, shapes)
    assert banded == 8, banded
    onnx.checker.check_model(model)
    out.mkdir(parents=True, exist_ok=True)
    path = out / OUT_FILE
    with tempfile.NamedTemporaryFile(dir=out, suffix=".part", delete=False) as part:
        part.write(model.SerializeToString())
    os.chmod(part.name, 0o644)
    Path(part.name).replace(path)
    return path


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    print(convert(Path(sys.argv[1]), Path(sys.argv[2])))
