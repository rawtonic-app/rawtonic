# SPDX-License-Identifier: AGPL-3.0-or-later
"""The network of AI Denoise's Fast quality (fast_denoise_v1.onnx, CC BY-SA 4.0; docs/MODELS.md).

A small U-Net on the packed Bayer planes with MatchFilter's interface. In: [N, 8, H, W] = 4 packed
planes (R, G beside R, B, G beside B; black-subtracted, white 1) and their noise standard deviations.
Out: [N, 4, H, W] denoised planes (input + predicted residual). Plain 3x3 convs + ReLU residual blocks,
stride-2 conv down, 1x1 conv + PixelShuffle up: ops ONNX Runtime's CPU provider runs well. Rawtonic's
own design. The pack's file is size L (`Student(**SIZES["L"])`), distilled from MatchFilter on 110
RawNIND frames and exported at opset 17 on 512 px tiles with the input `raw_with_noise` and the output
`denoised_raw` (training and measurements: Rawtonic's notes, benchmark-results/dst1-2026-11-25)."""
import torch
import torch.nn as nn
import torch.nn.functional as F


class Block(nn.Module):
    def __init__(self, c):
        super().__init__()
        self.a = nn.Conv2d(c, c, 3, padding=1)
        self.b = nn.Conv2d(c, c, 3, padding=1)

    def forward(self, x):
        return x + self.b(F.relu(self.a(x)))


class Student(nn.Module):
    def __init__(self, c=32, levels=4, blocks=2):
        super().__init__()
        ch = [c * 2 ** i for i in range(levels)]
        self.head = nn.Conv2d(8 + 4, ch[0], 3, padding=1)
        self.enc = nn.ModuleList(nn.Sequential(*[Block(ch[i]) for _ in range(blocks)]) for i in range(levels - 1))
        self.down = nn.ModuleList(nn.Conv2d(ch[i], ch[i + 1], 2, stride=2) for i in range(levels - 1))
        self.mid = nn.Sequential(*[Block(ch[-1]) for _ in range(blocks)])
        self.up = nn.ModuleList(nn.Conv2d(ch[i + 1], ch[i] * 4, 1) for i in range(levels - 1))
        self.dec = nn.ModuleList(nn.Sequential(*[Block(ch[i]) for _ in range(blocks)]) for i in range(levels - 1))
        self.tail = nn.Conv2d(ch[0], 4, 3, padding=1)
        self.levels = levels

    def forward(self, x):
        planes, std = x[:, :4], x[:, 4:]
        # normalised by the local noise: the network sees signal-to-noise
        s = std.clamp_min(1e-4)
        h = torch.cat([planes / s.mean(1, keepdim=True), std * 20.0, torch.sqrt(planes.clamp_min(0))], 1)
        h = F.relu(self.head(h))
        skips = []
        for i in range(self.levels - 1):
            h = self.enc[i](h)
            skips.append(h)
            h = F.relu(self.down[i](h))
        h = self.mid(h)
        for i in reversed(range(self.levels - 1)):
            h = F.pixel_shuffle(self.up[i](h), 2) + skips[i]
            h = self.dec[i](h)
        return planes + self.tail(F.relu(h)) * s.mean(1, keepdim=True)


SIZES = {'S': dict(c=16), 'M': dict(c=32), 'L': dict(c=48), 'XL': dict(c=64)}


def params(m):
    return sum(p.numel() for p in m.parameters())


def macs_per_px(m, levels=4, h=256):
    """Multiply-accumulates per packed pixel, counted from conv shapes on an h x h input."""
    tot = 0
    hooks = []

    def hook(mod, inp, out):
        nonlocal tot
        k = mod.kernel_size[0] * mod.kernel_size[1]
        tot += out.numel() * mod.in_channels * k // mod.groups

    for mod in m.modules():
        if isinstance(mod, nn.Conv2d):
            hooks.append(mod.register_forward_hook(hook))
    with torch.no_grad():
        m(torch.rand(1, 8, h, h) * 0.1 + 0.01)
    for hk in hooks:
        hk.remove()
    return tot / (h * h)
