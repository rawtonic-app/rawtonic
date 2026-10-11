#!/usr/bin/env python3
"""Export Meta's SAM 2.1 image model to the ONNX pair Rawtonic loads.

Weights: the SAM 2.1 checkpoints of facebookresearch/sam2 (Apache-2.0, "The
SAM 2 model checkpoints ... are licensed under Apache 2.0", README at commit
2b90b9f), loaded with its authors' code (package SAM-2 at that commit).

Encoder `sam2.1_<variant>_encoder.onnx`:
- input  image uint8 [1, 3, 1024, 1024]: the photo resized (stretched) to
  1024 x 1024, as SAM2ImagePredictor does; ImageNet normalisation runs in the
  graph;
- outputs image_embed [1, 256, 64, 64], high_res_feats_0 [1, 32, 256, 256],
  high_res_feats_1 [1, 64, 128, 128] (no_mem_embed already added, as in
  SAM2ImagePredictor.set_image).
Decoder `sam2.1_<variant>_decoder.onnx`:
- inputs image_embed, high_res_feats_0, high_res_feats_1, point_coords
  float32 [1, N, 2] (pixels of the 1024 x 1024 input, i.e. x * 1024 / width,
  y * 1024 / height), point_labels float32 [1, N] (1 positive, 0 negative,
  2/3 box corners), mask_input float32 [1, 1, 256, 256] (logits in the same
  stretched frame), has_mask_input float32 [1], orig_im_size float32 [2]
  (height, width);
- outputs masks [1, 1, height, width] (logits), iou_predictions [1, 1],
  low_res_masks [1, 1, 256, 256].
The decoder appends SAM 2's padding point itself (SAM2ImagePredictor always
does) and returns the single-mask output with SAM 2's stability fallback to
the best multimask candidate (multimask_output=False).

The weights are stored as fp16 behind Cast-to-fp32 nodes (half the size,
computed in fp32; masks checked against the fp32 export in docs/MODELS.md).
check_decoder() fails the export unless masks follow the requested size for
every aspect ratio.

Usage: export_sam2.py CHECKPOINT.pt {tiny,small,base_plus} OUT_DIR
Needs: torch, torchvision, SAM-2 (git+https://github.com/facebookresearch/sam2@2b90b9f5ceec907a1c18123530e92e794ad901a4,
built with SAM2_BUILD_CUDA=0), onnx, onnxruntime (see
scripts/models/build_model_pack.py for pinned versions).
"""
import sys
from pathlib import Path

import numpy as np
import onnx
import torch
import torch.nn.functional as F

CONFIGS = {
    "tiny": "configs/sam2.1/sam2.1_hiera_t.yaml",
    "small": "configs/sam2.1/sam2.1_hiera_s.yaml",
    "base_plus": "configs/sam2.1/sam2.1_hiera_b+.yaml",
}
SIZE = 1024


class Encoder(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model
        self.register_buffer("mean", torch.tensor([0.485, 0.456, 0.406]).view(1, 3, 1, 1) * 255.0)
        self.register_buffer("std", torch.tensor([0.229, 0.224, 0.225]).view(1, 3, 1, 1) * 255.0)

    def forward(self, image):
        x = (image.float() - self.mean) / self.std
        backbone_out = self.model.forward_image(x)
        _, vision_feats, _, sizes = self.model._prepare_backbone_features(backbone_out)
        vision_feats[-1] = vision_feats[-1] + self.model.no_mem_embed
        feats = [f.permute(1, 2, 0).reshape(1, -1, h, w) for f, (h, w) in zip(vision_feats, sizes)]
        return feats[2], feats[0], feats[1]


class Decoder(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, image_embed, high_res_feats_0, high_res_feats_1, point_coords, point_labels,
                mask_input, has_mask_input, orig_im_size):
        prompt = self.model.sam_prompt_encoder
        sparse = prompt._embed_points(point_coords, point_labels, pad=True)
        no_mask = prompt.no_mask_embed.weight.reshape(1, -1, 1, 1)
        dense = has_mask_input * prompt.mask_downscaling(mask_input) + (1 - has_mask_input) * no_mask
        low_res, iou, _, _ = self.model.sam_mask_decoder(
            image_embeddings=image_embed,
            image_pe=prompt.get_dense_pe(),
            sparse_prompt_embeddings=sparse,
            dense_prompt_embeddings=dense,
            multimask_output=False,
            repeat_image=False,
            high_res_features=[high_res_feats_0, high_res_feats_1],
        )
        height, width = orig_im_size.to(torch.int64)
        masks = F.interpolate(low_res, size=(height, width), mode="bilinear", align_corners=False)
        return masks, iou, torch.clamp(low_res, -32.0, 32.0)


def store_fp16(path: Path) -> None:
    """Store the large fp32 initializers as fp16 behind Cast-to-fp32 nodes."""
    model = onnx.load(str(path))
    graph = model.graph
    initializers, casts = [], []
    for init in graph.initializer:
        array = onnx.numpy_helper.to_array(init)
        if array.dtype == np.float32 and array.size >= 1024:
            half = onnx.numpy_helper.from_array(array.astype(np.float16), init.name + "_fp16")
            initializers.append(half)
            casts.append(onnx.helper.make_node("Cast", [half.name], [init.name], to=onnx.TensorProto.FLOAT))
        else:
            initializers.append(init)
    del graph.initializer[:]
    graph.initializer.extend(initializers)
    nodes = list(graph.node)
    del graph.node[:]
    graph.node.extend(casts + nodes)
    onnx.checker.check_model(model)
    onnx.save(model, str(path))


def export(checkpoint: Path, variant: str, out: Path, fp16: bool = True) -> list[Path]:
    from sam2.build_sam import build_sam2

    out.mkdir(parents=True, exist_ok=True)
    torch.manual_seed(0)
    model = build_sam2(CONFIGS[variant], str(checkpoint), device="cpu", mode="eval")
    suffix = "" if fp16 else "_fp32"
    encoder_path = out / f"sam2.1_{variant}_encoder{suffix}.onnx"
    decoder_path = out / f"sam2.1_{variant}_decoder{suffix}.onnx"
    with torch.no_grad():
        encoder = Encoder(model).eval()
        image = torch.zeros(1, 3, SIZE, SIZE, dtype=torch.uint8)
        embed, feats_0, feats_1 = encoder(image)
        torch.onnx.export(encoder, (image,), str(encoder_path), input_names=["image"],
                          output_names=["image_embed", "high_res_feats_0", "high_res_feats_1"],
                          opset_version=17, do_constant_folding=True, dynamo=False)
        dummy = {
            "image_embed": torch.randn(*embed.shape),
            "high_res_feats_0": torch.randn(*feats_0.shape),
            "high_res_feats_1": torch.randn(*feats_1.shape),
            "point_coords": torch.randint(0, SIZE, (1, 5, 2), dtype=torch.float),
            "point_labels": torch.randint(0, 4, (1, 5), dtype=torch.float),
            "mask_input": torch.randn(1, 1, 256, 256),
            "has_mask_input": torch.tensor([1], dtype=torch.float),
            "orig_im_size": torch.tensor([1500, 2250], dtype=torch.float),
        }
        torch.onnx.export(Decoder(model).eval(), tuple(dummy.values()), str(decoder_path),
                          input_names=list(dummy), output_names=["masks", "iou_predictions", "low_res_masks"],
                          dynamic_axes={"point_coords": {1: "num_points"}, "point_labels": {1: "num_points"}},
                          opset_version=17, do_constant_folding=True, dynamo=False)
    if fp16:
        store_fp16(encoder_path)
        store_fp16(decoder_path)
    check_decoder(decoder_path, feats_0.shape[1], feats_1.shape[1])
    return [encoder_path, decoder_path]


def check_decoder(path: Path, channels_0: int, channels_1: int) -> None:
    """Fail unless the full-size masks are the low-res masks scaled to each aspect ratio."""
    import onnxruntime as ort

    session = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    rng = np.random.default_rng(0)
    feeds = {
        "image_embed": rng.standard_normal((1, 256, 64, 64)).astype(np.float32) * 0.1,
        "high_res_feats_0": rng.standard_normal((1, channels_0, 256, 256)).astype(np.float32) * 0.1,
        "high_res_feats_1": rng.standard_normal((1, channels_1, 128, 128)).astype(np.float32) * 0.1,
        "mask_input": np.zeros((1, 1, 256, 256), np.float32),
        "has_mask_input": np.zeros(1, np.float32),
    }
    for width, height in ((1800, 1200), (1600, 1200), (1200, 1800), (1400, 1400), (1920, 1080)):
        masks, _, low_res = session.run(None, feeds | {
            "point_coords": np.array([[[SIZE / 2, SIZE / 2]]], np.float32),
            "point_labels": np.array([[1]], np.float32),
            "orig_im_size": np.array([height, width], np.float32),
        })
        expected = F.interpolate(torch.from_numpy(low_res), size=(height, width), mode="bilinear",
                                 align_corners=False).numpy() > 0
        got = masks > 0
        union = (expected | got).sum()
        iou = (expected & got).sum() / union if union else 1.0
        if masks.shape[2:] != (height, width) or iou < 0.99:
            sys.exit(f"{path}: {width}x{height} mask misplaced (shape {masks.shape[2:]}, IoU {iou:.3f})")


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if a != "--fp32"]
    if len(args) != 3 or args[1] not in CONFIGS:
        sys.exit(__doc__)
    for path in export(Path(args[0]), args[1], Path(args[2]), fp16="--fp32" not in sys.argv):
        print(path)
