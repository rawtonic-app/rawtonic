// SPDX-License-Identifier: AGPL-3.0-or-later
// Padding pass of the GPU X-Trans demosaic (xtrans_demosaic.wgsl).

const BORDER: u32 = 20u;
// Padding: the encoded mosaic with `BORDER` pixels around it, filled like
// the CPU tiles' borders (`nearest_same_color`). Separate bindings.
struct PadParams {
    width: u32,
    height: u32,
    padded_width: u32,
    padded_height: u32,
    pattern_width: u32,
    pattern_height: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> pad_params: PadParams;
@group(0) @binding(1) var<storage, read> raw_mosaic: array<f32>;
@group(0) @binding(2) var<storage, read> pad_pattern: array<u32>;
@group(0) @binding(3) var<storage, read_write> padded: array<f32>;

fn pad_color(y: i32, x: i32) -> u32 {
    let ph = i32(pad_params.pattern_height);
    let pw = i32(pad_params.pattern_width);
    return pad_pattern[u32(((y % ph) + ph) % ph * pw + ((x % pw) + pw) % pw)];
}

@compute @workgroup_size(16, 16, 1)
fn pad_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= pad_params.padded_width || id.y >= pad_params.padded_height) {
        return;
    }
    let w = i32(pad_params.width);
    let h = i32(pad_params.height);
    let ay = i32(id.y) - i32(BORDER);
    let ax = i32(id.x) - i32(BORDER);
    var value = 0.0;
    if (ay >= 0 && ax >= 0 && ay < h && ax < w) {
        value = raw_mosaic[u32(ay * w + ax)];
    } else {
        let color = pad_color(ay, ax);
        let cy = clamp(ay, 0, h - 1);
        let cx = clamp(ax, 0, w - 1);
        let sy = i32(pad_params.pattern_height);
        let sx = i32(pad_params.pattern_width);
        var best = 2147483647;
        for (var dy = -sy; dy <= sy; dy = dy + 1) {
            for (var dx = -sx; dx <= sx; dx = dx + 1) {
                let y = cy + dy;
                let x = cx + dx;
                if (y < 0 || x < 0 || y >= h || x >= w || pad_color(y, x) != color) {
                    continue;
                }
                let d = (y - ay) * (y - ay) + (x - ax) * (x - ax);
                if (d < best) {
                    best = d;
                    value = raw_mosaic[u32(y * w + x)];
                }
            }
        }
    }
    padded[id.y * pad_params.padded_width + id.x] = sqrt(max(value, 0.0));
}
