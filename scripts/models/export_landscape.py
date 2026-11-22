#!/usr/bin/env python3
"""Export UperNet + ConvNeXt-T (ADE20K, MIT) as Rawtonic's landscape-parts model.

The same checkpoint as the sky model (`export_sky.py`: openmmlab/upernet-
convnext-tiny, revision 876ffc5, MIT), with a different head: seven parts of
a landscape, each the network's probability that a pixel belongs to any
of the ADE20K classes of that part (`PARTS`); the last is the sky, which
the app's edge fitting uses to cut the other parts off the sky. The sky
model's file is not touched.

Writes OUT_DIR/landscape_upernet_convnext_t.onnx (opset 17, fixed size):
- input  pixel_values float32 [1, 3, 512, 512]: as the sky model (sRGB
  8-bit / 255, ImageNet normalised, the photo letterboxed into 512 x 512,
  padding 0 after normalisation);
- output parts float32 [1, 7, 512, 512]: per part (order of `PARTS`) the sum
  of the softmax over the 150 classes of the part's classes, in 0..1. Like
  the sky model, the part's logit margin (logsumexp of its classes minus
  logsumexp of all other classes) is computed at the decoder's 1/4
  resolution and bilinearly upsampled to the input size before a sigmoid,
  which keeps edges as crisp as upsampling all 150 logits.
Weights stored as fp16 behind Cast-to-fp32 nodes, as the sky model.

The part classes were fixed before any measurement (notes:
benchmark-results/lsm1-2026-11-21/REPORT.md) by what each ADE20K class is:
the network has no snow class (snow is mostly earth/land/mountain).

Usage: export_landscape.py OUT_DIR
Needs the environment of export_sky.py.
"""
import sys
from pathlib import Path

import numpy as np
import torch
import torch.nn.functional as F
from transformers import UperNetForSemanticSegmentation

sys.path.insert(0, str(Path(__file__).resolve().parent))
from export_sky import MODEL, REVISION, SIZE, FixedAdaptivePool, store_fp16  # noqa: E402

FILE = "landscape_upernet_convnext_t.onnx"
# Order = output channels = `rawtonic_segment::LandscapePart`.
PARTS = [
    ("mountains", ["mountain", "hill", "rock"]),
    ("water", ["water", "sea", "river", "lake", "waterfall", "swimming pool", "fountain"]),
    ("vegetation", ["tree", "grass", "plant", "flower", "palm", "field"]),
    ("natural_ground", ["earth", "sand", "land", "dirt track"]),
    ("artificial_ground", ["road", "sidewalk", "path", "runway", "floor", "pier"]),
    ("architecture", ["building", "house", "skyscraper", "tower", "bridge", "hovel", "wall",
                      "column", "windowpane", "door", "awning", "grandstand", "booth",
                      "stairway", "stairs", "fence", "railing"]),
    # As the sky model's channel (export_sky.py).
    ("sky", ["sky"]),
]


class Landscape(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.backbone = model.backbone
        self.decode_head = model.decode_head
        last = SIZE // 32
        for block in self.decode_head.psp_modules.blocks:
            pool = block.layers[0]
            assert isinstance(pool, torch.nn.AdaptiveAvgPool2d), pool
            block.layers[0] = FixedAdaptivePool(pool.output_size, last)
            block.add_module("0", block.layers[0])
        label = {v: int(k) for k, v in model.config.id2label.items()}
        n = len(label)
        # [parts, classes] membership matrix.
        member = torch.zeros(len(PARTS), n)
        for i, (_, classes) in enumerate(PARTS):
            for c in classes:
                member[i, label[c]] = 1.0
        assert (member.sum(0) <= 1).all(), "a class in two parts"
        self.register_buffer("member", member, persistent=False)

    def forward(self, pixel_values):
        features = self.backbone.forward_with_filtered_kwargs(pixel_values).feature_maps
        logits = self.decode_head(features)  # [1, 150, h, w]
        margins = []
        for i in range(len(PARTS)):
            inside = self.member[i].view(1, -1, 1, 1)
            # logsumexp over the part's classes and over the rest; a large
            # negative offset removes the other set's classes.
            part = torch.logsumexp(logits - 1e4 * (1 - inside), dim=1, keepdim=True)
            rest = torch.logsumexp(logits - 1e4 * inside, dim=1, keepdim=True)
            margins.append(part - rest)
        margin = torch.cat(margins, dim=1)
        margin = F.interpolate(margin, size=pixel_values.shape[2:], mode="bilinear", align_corners=False)
        return torch.sigmoid(margin)


def export(out: Path) -> Path:
    out.mkdir(parents=True, exist_ok=True)
    model = UperNetForSemanticSegmentation.from_pretrained(MODEL, revision=REVISION, use_safetensors=True).eval()
    path = out / FILE
    probe = torch.from_numpy(np.random.default_rng(0).standard_normal((1, 3, SIZE, SIZE), dtype=np.float32))
    with torch.no_grad():
        # The unmodified model (adaptive pooling, logits upsampled before the
        # softmax), part probabilities summed, as the reference.
        softmax = torch.softmax(model(pixel_values=probe).logits, dim=1)
        land = Landscape(model).eval()
        reference = torch.einsum("pc,bchw->bphw", land.member, softmax)
        print(f"max |export graph - original model| on noise: {float((land(probe) - reference).abs().max()):.5f}")
        torch.onnx.export(land, (torch.zeros(1, 3, SIZE, SIZE),), str(path),
                          input_names=["pixel_values"], output_names=["parts"],
                          opset_version=17, do_constant_folding=True, dynamo=False)
    store_fp16(path)
    return path


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    print(export(Path(sys.argv[1])))
