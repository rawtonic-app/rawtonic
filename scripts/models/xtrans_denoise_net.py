# SPDX-License-Identifier: AGPL-3.0-or-later
"""The network of AI Denoise for X-Trans sensors (xtrans_denoise_v1.onnx, CC BY-SA 4.0; docs/MODELS.md).

A small U-Net on the X-Trans mosaic, Rawtonic's own design. In: [N, 2, H, W] = the mosaic
(black-subtracted, white 1, the canonical 6x6 phase `CANON` at the origin, H and W multiples of 24)
and each sample's noise standard deviation sqrt(shot * x + read) * scale (MatchFilter's contract).
Full-resolution features: the sample split into R, G, B channels by the CFA, the R and B masks, sigma
and sqrt(x); space-to-depth by 3 (one 3x3 X-Trans block) -> 63 channels at 1/3 resolution, then the
U-Net (plain 3x3 conv + ReLU residual blocks, stride-2 conv down, 1x1 conv + PixelShuffle up, as the
Fast network). The tail predicts R, G, B residuals at every site (27 channels, depth-to-space by 3) and
the CFA picks each site's own colour, so the R/B sites whose colour alternates between 3x3 blocks need
no phase-specific weights. The input is normalised by its 3x3 block's mean sigma and the residual
scaled back by it. Out: [N, 1, H, W]. The pack's file is size L (`XTNet(**SIZES["L"])`, 9.6 M
parameters), trained by Rawtonic on RawNIND's X-T1 pairs (CC BY-SA 4.0) and exported at opset 17 on
1008 px tiles with the input `mosaic_with_noise` and the output `denoised_mosaic`; the app runs it at
the canonical phase per tile (crates/denoise/src/xtrans.rs). Training and measurements: Rawtonic's
notes, benchmark-results/xts1-2026-11-25 and xtsh1-2026-11-25."""
import torch
import torch.nn as nn
import torch.nn.functional as F

CANON = [[0, 2, 1, 2, 0, 1], [1, 1, 0, 1, 1, 2], [1, 1, 2, 1, 1, 0],
         [2, 0, 1, 0, 2, 1], [1, 1, 2, 1, 1, 0], [1, 1, 0, 1, 1, 2]]


class Block(nn.Module):
    def __init__(self, c):
        super().__init__()
        self.a = nn.Conv2d(c, c, 3, padding=1)
        self.b = nn.Conv2d(c, c, 3, padding=1)

    def forward(self, x):
        return x + self.b(F.relu(self.a(x)))


class XTNet(nn.Module):
    def __init__(self, c=48, levels=4, blocks=2):
        super().__init__()
        ch = [c * 2 ** i for i in range(levels)]
        onehot = F.one_hot(torch.tensor(CANON), 3).permute(2, 0, 1).float()  # [3, 6, 6]
        self.register_buffer('cfa6', onehot, persistent=False)
        self.head = nn.Conv2d(7 * 9, ch[0], 3, padding=1)
        self.enc = nn.ModuleList(nn.Sequential(*[Block(ch[i]) for _ in range(blocks)]) for i in range(levels - 1))
        self.down = nn.ModuleList(nn.Conv2d(ch[i], ch[i + 1], 2, stride=2) for i in range(levels - 1))
        self.mid = nn.Sequential(*[Block(ch[-1]) for _ in range(blocks)])
        self.up = nn.ModuleList(nn.Conv2d(ch[i + 1], ch[i] * 4, 1) for i in range(levels - 1))
        self.dec = nn.ModuleList(nn.Sequential(*[Block(ch[i]) for _ in range(blocks)]) for i in range(levels - 1))
        self.tail = nn.Conv2d(ch[0], 27, 3, padding=1)
        self.levels = levels

    def cfa(self, h, w, phase=(0, 0)):
        """[1, 3, H, W] one-hot CFA of a mosaic whose sample (0, 0) is canonical site `phase` (multiples of 3)."""
        c6 = torch.roll(self.cfa6, (-phase[0], -phase[1]), (1, 2))
        return c6.repeat(1, h // 6, w // 6).unsqueeze(0)

    def forward(self, x, cfa=None):
        """cfa: [N or 1, 3, H, W]; default the canonical phase at the origin."""
        m, s = x[:, :1], x[:, 1:2]
        n, _, h, w = m.shape
        if cfa is None:
            cfa = self.cfa(h, w)
        sbar = F.avg_pool2d(s, 3).clamp_min(1e-4)  # [N, 1, H/3, W/3]
        sfull = F.interpolate(sbar, scale_factor=3, mode='nearest')
        f = torch.cat([m / sfull * cfa, cfa[:, ::2].expand(n, -1, -1, -1), s * 20.0, torch.sqrt(m.clamp_min(0))], 1)
        hh = F.relu(self.head(F.pixel_unshuffle(f, 3)))
        skips = []
        for i in range(self.levels - 1):
            hh = self.enc[i](hh)
            skips.append(hh)
            hh = F.relu(self.down[i](hh))
        hh = self.mid(hh)
        for i in reversed(range(self.levels - 1)):
            hh = F.pixel_shuffle(self.up[i](hh), 2) + skips[i]
            hh = self.dec[i](hh)
        res = F.pixel_shuffle(self.tail(F.relu(hh)), 3)  # [N, 3, H, W]
        return m + (res * cfa).sum(1, keepdim=True) * sfull


SIZES = {'S': dict(c=16), 'M': dict(c=32), 'L': dict(c=48), 'XL': dict(c=64)}
