#!/usr/bin/env python3
"""Export EasyPortrait's FPN + ResNet-50 face parser as Rawtonic's eye and
teeth model (the face retouch's Iris, Sclera and Teeth).

Weights: `fpn-fp-1024.pth` of EasyPortrait (SberDevices; Kvanchiani,
Petrova, Efremyan, Sautin, Kapitanov; github.com/hukenovs/easyportrait at
1b3d3e5), licensed under the authors' variant of CC BY-SA 4.0
(licenses/models/easyportrait-LICENSE.txt). The converted file is published
under CC BY-SA 4.0 as that licence allows for adaptations; changes: the
network re-implemented in plain PyTorch below (mmsegmentation 0.30's
EncoderDecoder with ResNetV1c-50, FPN and FPNHead, as the authors' fpn-fp
config builds it; every key of the checkpoint loads strictly), the
optimizer state left out, the input normalisation, a mirror pass and the
class merging put into the graph, converted to ONNX, weights stored as fp16
(docs/MODELS.md).

Writes OUT_DIR/easyportrait_fpn_1024.onnx (opset 17, fixed size):
- input  image float32 [1, 3, 1024, 1024]: sRGB 8-bit values 0..255 (not
  divided), a square crop around a face (rawtonic-faces `portrait`);
- output parts float32 [1, 2, 1024, 1024]: per pixel the probability of an
  eye (left + right eye classes) and of teeth. The network runs on the crop
  and on its mirror image; their logits (the mirror's flipped back, its
  left and right classes swapped) are averaged, upsampled bilinearly to the
  input size inside the network, then the softmax over the 8 classes.
The weights are stored as fp16 behind Cast-to-fp32 nodes (export_sky.py's
`store_fp16`): half the size, fp32 compute.

Usage: export_easyportrait.py CHECKPOINT OUT_DIR [--check]
  --check compares the file with the PyTorch graph on noise (ONNX Runtime CPU).
Needs: torch, onnx, onnxruntime (and transformers, which export_sky.py
imports); scripts/models/build_model_pack.py pins the versions.
"""
import argparse
import sys
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

SIZE = 1024
FILE = "easyportrait_fpn_1024.onnx"
CHECKPOINT = {
    "url": "https://rndml-team-cv.obs.ru-moscow-1.hc.sbercloud.ru/datasets/easyportrait/experiments/models/fpn-fp-1024.pth",
    "sha256": "3543661ad51e08fe49c2f202fafc39d99d1a1c78e9de033c67b532ee5401ef40",
}
# EasyPortrait's training normalisation (its configs' img_norm_cfg), RGB.
MEAN = (143.55267075, 132.96705975, 126.94924335)
STD = (60.2625333, 60.32740275, 59.30988645)
# Classes: background, skin, left brow, right brow, left eye, right eye,
# lips, teeth. A mirror image swaps the left and right ones.
MIRROR = [0, 1, 3, 2, 5, 4, 6, 7]
EYES, TEETH = (4, 5), 7


def conv_bn_relu(i, o, stride=1):
    return [nn.Conv2d(i, o, 3, stride, 1, bias=False), nn.BatchNorm2d(o), nn.ReLU(inplace=True)]


class Bottleneck(nn.Module):
    def __init__(self, i, p, stride, down):
        super().__init__()
        self.conv1, self.bn1 = nn.Conv2d(i, p, 1, bias=False), nn.BatchNorm2d(p)
        self.conv2, self.bn2 = nn.Conv2d(p, p, 3, stride, 1, bias=False), nn.BatchNorm2d(p)
        self.conv3, self.bn3 = nn.Conv2d(p, p * 4, 1, bias=False), nn.BatchNorm2d(p * 4)
        self.downsample = (nn.Sequential(nn.Conv2d(i, p * 4, 1, stride, bias=False), nn.BatchNorm2d(p * 4))
                           if down else None)

    def forward(self, x):
        o = F.relu(self.bn1(self.conv1(x)))
        o = F.relu(self.bn2(self.conv2(o)))
        o = self.bn3(self.conv3(o))
        return F.relu(o + (self.downsample(x) if self.downsample is not None else x))


class Backbone(nn.Module):
    """ResNetV1c-50: a three-convolution stem, then four stages."""

    def __init__(self):
        super().__init__()
        self.stem = nn.Sequential(*conv_bn_relu(3, 32, 2), *conv_bn_relu(32, 32), *conv_bn_relu(32, 64))
        width = 64
        for k, (blocks, planes, stride) in enumerate([(3, 64, 1), (4, 128, 2), (6, 256, 2), (3, 512, 2)]):
            layer = [Bottleneck(width, planes, stride, True)]
            layer += [Bottleneck(planes * 4, planes, 1, False) for _ in range(blocks - 1)]
            setattr(self, f"layer{k + 1}", nn.Sequential(*layer))
            width = planes * 4

    def forward(self, x):
        x = F.max_pool2d(self.stem(x), 3, 2, 1)
        outs = []
        for k in range(1, 5):
            x = getattr(self, f"layer{k}")(x)
            outs.append(x)
        return outs


class ConvModule(nn.Module):
    def __init__(self, i, o, k, norm):
        super().__init__()
        self.conv = nn.Conv2d(i, o, k, 1, k // 2, bias=not norm)
        self.bn = nn.BatchNorm2d(o) if norm else None

    def forward(self, x):
        x = self.conv(x)
        return F.relu(self.bn(x)) if self.bn is not None else x


class Up(nn.Module):
    def forward(self, x):
        return F.interpolate(x, scale_factor=2, mode="bilinear", align_corners=False)


class Neck(nn.Module):
    def __init__(self):
        super().__init__()
        self.lateral_convs = nn.ModuleList([ConvModule(c, 256, 1, False) for c in (256, 512, 1024, 2048)])
        self.fpn_convs = nn.ModuleList([ConvModule(256, 256, 3, False) for _ in range(4)])

    def forward(self, xs):
        lateral = [conv(x) for conv, x in zip(self.lateral_convs, xs)]
        for i in range(3, 0, -1):
            lateral[i - 1] = lateral[i - 1] + F.interpolate(lateral[i], size=lateral[i - 1].shape[2:], mode="nearest")
        return [conv(x) for conv, x in zip(self.fpn_convs, lateral)]


class Head(nn.Module):
    def __init__(self, classes=8):
        super().__init__()
        heads = []
        for i in range(4):
            layers = [ConvModule(256, 128, 3, True)]
            for k in range(i):
                if k:
                    layers.append(ConvModule(128, 128, 3, True))
                layers.append(Up())
            heads.append(nn.Sequential(*layers))
        self.scale_heads = nn.ModuleList(heads)
        self.conv_seg = nn.Conv2d(128, classes, 1)

    def forward(self, xs):
        out = self.scale_heads[0](xs[0])
        for i in range(1, 4):
            out = out + F.interpolate(self.scale_heads[i](xs[i]), size=out.shape[2:], mode="bilinear",
                                      align_corners=False)
        return self.conv_seg(out)


class Net(nn.Module):
    """The checkpoint's network: logits at a quarter of the input size."""

    def __init__(self):
        super().__init__()
        self.backbone, self.neck, self.decode_head = Backbone(), Neck(), Head()

    def forward(self, x):
        return self.decode_head(self.neck(self.backbone(x)))


class EyesTeeth(nn.Module):
    """The exported graph (see the module documentation)."""

    def __init__(self, net):
        super().__init__()
        self.net = net
        self.register_buffer("mean", torch.tensor(MEAN).view(1, 3, 1, 1), persistent=False)
        self.register_buffer("std", torch.tensor(STD).view(1, 3, 1, 1), persistent=False)

    def forward(self, image):
        x = (image - self.mean) / self.std
        logits = self.net(torch.cat([x, x.flip(3)], 0))
        logits = (logits[0:1] + logits[1:2].flip(3)[:, MIRROR]) / 2
        logits = F.interpolate(logits, size=image.shape[2:], mode="bilinear", align_corners=False)
        p = logits.softmax(1)
        return torch.cat([p[:, EYES[0]:EYES[0] + 1] + p[:, EYES[1]:EYES[1] + 1], p[:, TEETH:TEETH + 1]], 1)


def load(checkpoint: Path) -> EyesTeeth:
    state = torch.load(str(checkpoint), map_location="cpu", weights_only=False)["state_dict"]
    net = Net()
    net.load_state_dict(state, strict=True)
    return EyesTeeth(net.eval()).eval()


def export(checkpoint: Path, out: Path) -> Path:
    import onnx  # noqa: F401  (store_fp16 needs it)
    from export_sky import store_fp16

    out.mkdir(parents=True, exist_ok=True)
    path = out / FILE
    model = load(checkpoint)
    with torch.no_grad():
        torch.onnx.export(model, (torch.zeros(1, 3, SIZE, SIZE),), str(path),
                          input_names=["image"], output_names=["parts"],
                          opset_version=17, do_constant_folding=True, dynamo=False)
    store_fp16(path)
    return path


def check(checkpoint: Path, path: Path) -> None:
    import onnxruntime as ort

    model = load(checkpoint)
    probe = np.random.default_rng(0).uniform(0, 255, (1, 3, SIZE, SIZE)).astype(np.float32)
    with torch.no_grad():
        reference = model(torch.from_numpy(probe)).numpy()
    session = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    got = session.run(None, {"image": probe})[0]
    print(f"max |onnx - torch| {float(np.abs(got - reference).max()):.5f}")


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("checkpoint", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    exported = export(args.checkpoint, args.out)
    print(exported, exported.stat().st_size)
    if args.check:
        check(args.checkpoint, exported)
