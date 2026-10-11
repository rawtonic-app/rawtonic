#!/usr/bin/env python3
"""Export Google's SigLIP 2 B/16 (Apache-2.0) for Rawtonic's automatic tags.

Writes into OUT_DIR:
- siglip2_vision.onnx: pixel_values [n, 3, 224, 224] ((rgb / 255 - 0.5) / 0.5,
  the photo squashed to 224 x 224) -> L2-normalised image_embeds [n, 768];
- siglip2_text.onnx: input_ids [n, 64] -> L2-normalised text_embeds [n, 768],
  needed only for the user's own tags;
- siglip2_tokenizer.json: the Gemma tokenizer, padding to 64 ids;
- siglip2_tags.bin: the embeddings of "a photo of <tag>" for every entry of
  TAG_CANDIDATES (src-tauri/src/tagging_utils/candidates.rs), in that order,
  little-endian f32, 768 per tag.
Score of a tag: sigmoid(LOGIT_SCALE * cosine + LOGIT_BIAS) (printed). The
weights stay fp32: dynamic uint8 quantisation drops the embedding cosine to
0.80 (docs/MODELS.md).

Usage: export_siglip2.py OUT_DIR
"""
import re
import sys
from pathlib import Path

import numpy as np
import torch
from transformers import AutoModel, AutoProcessor

MODEL = "google/siglip2-base-patch16-224"
REVISION = "75de2d55ec2d0b4efc50b3e9ad70dba96a7b2fa2"
CANDIDATES = Path(__file__).resolve().parents[2] / "src-tauri/src/tagging_utils/candidates.rs"
PROMPT = "a photo of {}"


class Vision(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, pixel_values):
        embeds = self.model.get_image_features(pixel_values=pixel_values)
        return embeds / embeds.norm(dim=-1, keepdim=True)


class Text(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, input_ids):
        embeds = self.model.get_text_features(input_ids=input_ids)
        return embeds / embeds.norm(dim=-1, keepdim=True)


def candidates() -> list[str]:
    source = CANDIDATES.read_text()
    tags = re.findall(r'^\s*"([^"]+)",', source, re.M)
    declared = int(re.search(r"TAG_CANDIDATES: \[&str; (\d+)\]", source).group(1))
    assert len(tags) == declared, (len(tags), declared)
    return tags


def export(out: Path) -> list[Path]:
    out.mkdir(parents=True, exist_ok=True)
    model = AutoModel.from_pretrained(MODEL, revision=REVISION).eval()
    processor = AutoProcessor.from_pretrained(MODEL, revision=REVISION)
    tokenizer = processor.tokenizer
    ids = processor(text=[PROMPT.format("a dog")], padding="max_length", max_length=64, return_tensors="pt")["input_ids"]
    with torch.no_grad():
        torch.onnx.export(Vision(model), (torch.zeros(1, 3, 224, 224),), str(out / "siglip2_vision.onnx"),
                          input_names=["pixel_values"], output_names=["image_embeds"],
                          dynamic_axes={"pixel_values": {0: "n"}}, opset_version=17, dynamo=False)
        torch.onnx.export(Text(model), (ids,), str(out / "siglip2_text.onnx"),
                          input_names=["input_ids"], output_names=["text_embeds"],
                          dynamic_axes={"input_ids": {0: "n"}}, opset_version=17, dynamo=False)
        tags = candidates()
        batch = processor(text=[PROMPT.format(t) for t in tags], padding="max_length", max_length=64,
                          return_tensors="pt")["input_ids"]
        embeds = Text(model)(batch).numpy().astype("<f4")
    assert embeds.shape == (len(tags), 768)
    (out / "siglip2_tags.bin").write_bytes(embeds.tobytes())
    backend = tokenizer.backend_tokenizer
    backend.enable_truncation(64)
    backend.enable_padding(length=64, pad_id=tokenizer.pad_token_id, pad_token=tokenizer.pad_token)
    backend.save(str(out / "siglip2_tokenizer.json"))
    print(f"LOGIT_SCALE {float(model.logit_scale.exp())} LOGIT_BIAS {float(model.logit_bias)}; {len(tags)} tags")
    return [out / n for n in ("siglip2_vision.onnx", "siglip2_text.onnx", "siglip2_tokenizer.json", "siglip2_tags.bin")]


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    for path in export(Path(sys.argv[1])):
        print(path)
