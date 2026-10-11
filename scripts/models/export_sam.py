#!/usr/bin/env python3
"""Export Meta's Segment Anything ViT-B checkpoint to the ONNX pair Rawtonic loads.

Encoder: uint8 [1, 3, 1024, 1024] (the photo resized to a long side of 1024,
top-left, zero padded) -> image embeddings [1, 256, 64, 64]; SAM's pixel
normalisation runs inside the graph. Decoder: segment_anything's
SamOnnxModel with the original image size as input (masks at full size, four
candidates), its crop made dynamic (see Decoder). Both get per-channel dynamic uint8 weight quantisation; against
the unquantised export it agrees better than per-tensor quantisation at the
same size (docs/MODELS.md).

Usage: export_sam.py sam_vit_b_01ec64.pth OUT_DIR
Needs: torch, torchvision, segment-anything, onnx, onnxruntime (see
scripts/models/build_model_pack.py for pinned versions).
"""
import sys
from pathlib import Path

import torch
import torch.nn.functional as F
from onnxruntime.quantization import QuantType, quantize_dynamic
from segment_anything import sam_model_registry
from segment_anything.utils.onnx import SamOnnxModel


class Decoder(SamOnnxModel):
    """SamOnnxModel whose crop to the photo's area follows `orig_im_size` at run time.

    Upstream slices with Python ints derived from the tensor, which the
    current tracer bakes in from the example input: exported as is, every
    photo whose aspect ratio differs from the example (portrait, 4:3, square)
    gets a stretched, shifted mask. check_decoder() guards against that.
    """

    def mask_postprocessing(self, masks: torch.Tensor, orig_im_size: torch.Tensor) -> torch.Tensor:
        size = self.img_size
        masks = F.interpolate(masks, size=(size, size), mode="bilinear", align_corners=False)
        scale = size / torch.max(orig_im_size)
        prepadded = torch.floor(orig_im_size * scale + 0.5).to(torch.int64)
        masks = torch.index_select(masks, 2, torch.arange(prepadded[0]))
        masks = torch.index_select(masks, 3, torch.arange(prepadded[1]))
        height, width = orig_im_size.to(torch.int64)
        return F.interpolate(masks, size=(height, width), mode="bilinear", align_corners=False)


class Encoder(torch.nn.Module):
    def __init__(self, sam):
        super().__init__()
        self.encoder = sam.image_encoder
        self.register_buffer("mean", sam.pixel_mean.view(1, 3, 1, 1))
        self.register_buffer("std", sam.pixel_std.view(1, 3, 1, 1))

    def forward(self, image):
        return self.encoder((image.float() - self.mean) / self.std)


def export(checkpoint: Path, out: Path) -> list[Path]:
    out.mkdir(parents=True, exist_ok=True)
    sam = sam_model_registry["vit_b"](checkpoint=str(checkpoint)).eval()
    encoder_fp32, decoder_fp32 = out / "sam_encoder_fp32.onnx", out / "sam_decoder_fp32.onnx"
    with torch.no_grad():
        torch.onnx.export(
            Encoder(sam),
            (torch.zeros(1, 3, 1024, 1024, dtype=torch.uint8),),
            str(encoder_fp32),
            input_names=["input"],
            output_names=["output"],
            opset_version=17,
            dynamo=False,
        )
        embed_dim = sam.prompt_encoder.embed_dim
        embed_size = sam.prompt_encoder.image_embedding_size
        dummy = {
            "image_embeddings": torch.randn(1, embed_dim, *embed_size),
            "point_coords": torch.randint(0, 1024, (1, 5, 2), dtype=torch.float),
            "point_labels": torch.randint(0, 4, (1, 5), dtype=torch.float),
            "mask_input": torch.randn(1, 1, 256, 256),
            "has_mask_input": torch.tensor([1], dtype=torch.float),
            "orig_im_size": torch.tensor([1500, 2250], dtype=torch.float),
        }
        torch.onnx.export(
            Decoder(sam, return_single_mask=False),
            tuple(dummy.values()),
            str(decoder_fp32),
            input_names=list(dummy),
            output_names=["masks", "iou_predictions", "low_res_masks"],
            dynamic_axes={"point_coords": {1: "num_points"}, "point_labels": {1: "num_points"}},
            opset_version=17,
            dynamo=False,
        )
    outputs = []
    for part, source in (("encoder", encoder_fp32), ("decoder", decoder_fp32)):
        target = out / f"sam_vit_b_01ec64_{part}.onnx"
        quantize_dynamic(str(source), str(target), weight_type=QuantType.QUInt8, per_channel=True)
        outputs.append(target)
    check_decoder(outputs[1])
    return outputs


def check_decoder(path: Path) -> None:
    """Fail unless the full-size masks match the low-res masks for every aspect ratio."""
    import numpy as np
    import onnxruntime as ort

    session = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    rng = np.random.default_rng(0)
    embeddings = rng.standard_normal((1, 256, 64, 64)).astype(np.float32) * 0.1
    for width, height in ((1800, 1200), (1600, 1200), (1200, 1800), (1400, 1400), (1920, 1080)):
        scale = 1024 / max(width, height)
        masks, _, low_res = session.run(None, {
            "image_embeddings": embeddings,
            "point_coords": np.array([[[width * scale / 2, height * scale / 2]]], np.float32),
            "point_labels": np.array([[1]], np.float32),
            "mask_input": np.zeros((1, 1, 256, 256), np.float32),
            "has_mask_input": np.zeros(1, np.float32),
            "orig_im_size": np.array([height, width], np.float32),
        })
        expected = F.interpolate(torch.from_numpy(low_res), size=(1024, 1024), mode="bilinear", align_corners=False)
        expected = expected[..., : round(height * scale), : round(width * scale)]
        expected = F.interpolate(expected, size=(height, width), mode="bilinear", align_corners=False).numpy() > 0
        got = masks > 0
        iou = (expected & got).sum() / max((expected | got).sum(), 1)
        if masks.shape[2:] != (height, width) or iou < 0.99:
            sys.exit(f"{path}: {width}x{height} mask misplaced (shape {masks.shape[2:]}, IoU {iou:.3f})")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    for path in export(Path(sys.argv[1]), Path(sys.argv[2])):
        print(path)
