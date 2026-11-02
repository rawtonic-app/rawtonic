// SPDX-License-Identifier: AGPL-3.0-or-later
// Highlight reconstruction on the CFA mosaic (highlight_reconstruction.rs,
// `HighlightRebuild::Ratios`) as compute passes: the same steps, sample by
// sample, with the small grid steps (fill, relax) on the CPU in between
// (highlight_reconstruction/gpu.rs). Sums per grid cell are reduced in a
// fixed order inside one workgroup, so a run is deterministic on a device.

struct Params {
    width: u32,
    height: u32,
    left: u32,
    top: u32,
    aw: u32,
    ah: u32,
    words: u32,
    grid_cols: u32,
    grid_rows: u32,
    share_cols: u32,
    share_rows: u32,
    ratios: u32,
    thresholds: vec4<f32>,
    wb: vec4<f32>,
    clip_wb: vec4<f32>,
    // x: neutral fill level; y: onset low fraction; z: onset bins
    misc: vec4<f32>,
    ratio_measured: vec4<u32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> data: array<f32>;
@group(0) @binding(2) var<storage, read_write> mask: array<u32>;
@group(0) @binding(3) var<storage, read_write> hnear: array<u32>;
@group(0) @binding(4) var<storage, read_write> near_bits: array<u32>;
@group(0) @binding(5) var<storage, read_write> cells: array<f32>;
@group(0) @binding(6) var<storage, read_write> blocks: array<f32>;
@group(0) @binding(7) var<storage, read_write> share: array<f32>;
@group(0) @binding(8) var<storage, read> grids: array<f32>;
@group(0) @binding(9) var<storage, read_write> result_data: array<f32>;
@group(0) @binding(10) var<storage, read> pattern: array<u32>;
@group(0) @binding(11) var<storage, read_write> counters: array<atomic<u32>>;

const CELL: u32 = 64u;
const SHARE_BLOCK: u32 = 8u;
const SHARE_BLUR: i32 = 2;
const RATIO_REACH: u32 = 8u;
const CANDIDATE_FRACTION: f32 = 0.5;
const CANDIDATE_MIN_RATIO: f32 = 0.75;
const RATIO_UNIFORMITY: f32 = 0.08;
const RATIO_RAMP_START: f32 = 0.5;
const RATIO_RAMP: f32 = 0.4;
const DARK_EDGE_RANGE: f32 = 0.4;
const NEAR_CLIP: f32 = 0.08;
const LEVEL_ROW_STEP: u32 = 4u;
const GRID_STRIDE: u32 = 8u;
const ONSET_BINS: u32 = 50u;
// counters: 0 clipped samples, 1 raised samples, 4..6 maxima (f32 bits),
// 8.. onset histograms (3 x ONSET_BINS)
const C_CLIPPED: u32 = 0u;
const C_RAISED: u32 = 1u;
const C_MAX: u32 = 4u;
const C_HIST: u32 = 8u;

fn color_at(row: u32, col: u32) -> u32 {
    return pattern[(row % 48u) * 48u + (col % 48u)];
}

fn is_clipped(row: u32, col: u32) -> bool {
    return ((mask[row * p.words + col / 32u] >> (col % 32u)) & 1u) != 0u;
}

fn smooth01(t: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0);
    return x * x * (3.0 - 2.0 * x);
}

struct Window {
    sum: vec3<f32>,
    count: vec3<u32>,
    clipped: vec3<u32>,
    sum_unclipped: vec3<f32>,
};

fn window(row: u32, col: u32, radius: u32, unclipped_only: bool) -> Window {
    var w: Window;
    w.sum = vec3<f32>(0.0);
    w.count = vec3<u32>(0u);
    w.clipped = vec3<u32>(0u);
    w.sum_unclipped = vec3<f32>(0.0);
    let r0 = max(select(row - radius, 0u, row < radius), p.top);
    let r1 = min(row + radius, p.top + p.ah - 1u);
    let c0 = max(select(col - radius, 0u, col < radius), p.left);
    let c1 = min(col + radius, p.left + p.aw - 1u);
    for (var r = r0; r <= r1; r++) {
        for (var c = c0; c <= c1; c++) {
            let color = color_at(r, c);
            let clipped = is_clipped(r, c);
            if (color >= 3u || (unclipped_only && clipped)) {
                continue;
            }
            let value = data[r * p.width + c] * p.wb[color];
            w.sum[color] += value;
            w.count[color] += 1u;
            if (clipped) {
                w.clipped[color] += 1u;
            } else {
                w.sum_unclipped[color] += value;
            }
        }
    }
    return w;
}

// Mean of the two colours other than `color`; negative count flag in .y.
fn mean_others(w: Window, color: u32) -> vec2<f32> {
    var total = 0.0;
    for (var o = 0u; o < 3u; o++) {
        if (o == color) {
            continue;
        }
        if (w.count[o] == 0u) {
            return vec2<f32>(0.0, 0.0);
        }
        total += w.sum[o] / f32(w.count[o]);
    }
    return vec2<f32>(total / 2.0, 1.0);
}

fn min_other(w: Window, color: u32) -> f32 {
    var low = 3.0e38;
    for (var o = 0u; o < 3u; o++) {
        if (o == color) {
            continue;
        }
        if (w.count[o] == 0u) {
            return 0.0;
        }
        low = min(low, w.sum[o] / f32(w.count[o]));
    }
    return low;
}

fn any_clipped_near(row: u32, col: u32) -> bool {
    let r0 = max(select(row - 1u, 0u, row < 1u), p.top);
    let r1 = min(row + 1u, p.top + p.ah - 1u);
    let c0 = max(select(col - 1u, 0u, col < 1u), p.left);
    let c1 = min(col + 1u, p.left + p.aw - 1u);
    for (var r = r0; r <= r1; r++) {
        for (var c = c0; c <= c1; c++) {
            if (is_clipped(r, c)) {
                return true;
            }
        }
    }
    return false;
}

fn uniform_around(row: u32, col: u32, color: u32, value: f32, nearw: Window) -> bool {
    let wide = window(row, col, 2u, false);
    if (any(wide.clipped != vec3<u32>(0u))) {
        return false;
    }
    for (var k = 0u; k < 3u; k++) {
        if (wide.count[k] == 0u) {
            continue;
        }
        let outer = wide.sum[k] / f32(wide.count[k]);
        var inner = outer;
        if (k == color) {
            inner = value;
        } else if (nearw.count[k] > 0u) {
            inner = nearw.sum[k] / f32(nearw.count[k]);
        }
        if (abs(inner / max(outer, 1e-12) - 1.0) > RATIO_UNIFORMITY) {
            return false;
        }
    }
    return true;
}

// Bilinear lookup in a per-cell grid (cell centres at CELL / 2), field k.
fn grid_at(row: u32, col: u32, k: u32) -> f32 {
    let fy = (f32(row) + 0.5) / f32(CELL) - 0.5;
    let fx = (f32(col) + 0.5) / f32(CELL) - 0.5;
    let y0 = u32(clamp(floor(fy), 0.0, f32(p.grid_rows - 1u)));
    let x0 = u32(clamp(floor(fx), 0.0, f32(p.grid_cols - 1u)));
    let y1 = min(y0 + 1u, p.grid_rows - 1u);
    let x1 = min(x0 + 1u, p.grid_cols - 1u);
    let ty = clamp(fy - f32(y0), 0.0, 1.0);
    let tx = clamp(fx - f32(x0), 0.0, 1.0);
    let a = grids[(y0 * p.grid_cols + x0) * GRID_STRIDE + k];
    let b = grids[(y0 * p.grid_cols + x1) * GRID_STRIDE + k];
    let c = grids[(y1 * p.grid_cols + x0) * GRID_STRIDE + k];
    let d = grids[(y1 * p.grid_cols + x1) * GRID_STRIDE + k];
    let t = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    return t + (bottom - t) * ty;
}

// Bilinear lookup in the blurred share map, active-relative position.
fn share_at(y: u32, x: u32) -> vec3<f32> {
    let fy = (f32(y) + 0.5) / f32(SHARE_BLOCK) - 0.5;
    let fx = (f32(x) + 0.5) / f32(SHARE_BLOCK) - 0.5;
    let y0 = u32(clamp(floor(fy), 0.0, f32(p.share_rows - 1u)));
    let x0 = u32(clamp(floor(fx), 0.0, f32(p.share_cols - 1u)));
    let y1 = min(y0 + 1u, p.share_rows - 1u);
    let x1 = min(x0 + 1u, p.share_cols - 1u);
    let ty = clamp(fy - f32(y0), 0.0, 1.0);
    let tx = clamp(fx - f32(x0), 0.0, 1.0);
    var result: vec3<f32>;
    for (var k = 0u; k < 3u; k++) {
        let a = share[(y0 * p.share_cols + x0) * 3u + k];
        let b = share[(y0 * p.share_cols + x1) * 3u + k];
        let c = share[(y1 * p.share_cols + x0) * 3u + k];
        let d = share[(y1 * p.share_cols + x1) * 3u + k];
        let t = a + (b - a) * tx;
        let bottom = c + (d - c) * tx;
        result[k] = t + (bottom - t) * ty;
    }
    return result;
}

fn neutral_weight(s: vec3<f32>) -> f32 {
    return smooth01(min(min(min(1.0, s.x), s.y), s.z));
}

// (pair index, sign) of `color` over `other`.
fn pair_index(color: u32, other: u32) -> vec2<f32> {
    let lo = min(color, other);
    let hi = max(color, other);
    var index = 2.0;
    if (lo == 0u && hi == 1u) {
        index = 0.0;
    } else if (lo == 0u && hi == 2u) {
        index = 1.0;
    }
    return vec2<f32>(index, select(-1.0, 1.0, color < other));
}

// (opposed estimate, neutral weight, brightest, valid)
fn estimate_parts(row: u32, col: u32, color: u32) -> vec4<f32> {
    let w = window(row, col, 2u, false);
    let mo = mean_others(w, color);
    if (mo.y == 0.0) {
        return vec4<f32>(0.0);
    }
    let floor_ = min_other(w, color);
    var opposed = max(mo.x + grid_at(row, col, color), floor_);
    let s = share_at(row - p.top, col - p.left);
    let a = (color + 1u) % 3u;
    let b = (color + 2u) % 3u;
    var other_clipped = b;
    var recorded = a;
    if (s[a] >= s[b]) {
        other_clipped = a;
        recorded = b;
    }
    let joint_weight = smooth01(s[other_clipped]) * smooth01(2.0 * (s[other_clipped] - s[recorded]));
    if (joint_weight > 0.0 && w.count[recorded] > 0u) {
        let level = w.sum[recorded] / f32(w.count[recorded]);
        let joint = level + (4.0 * grid_at(row, col, color) + 2.0 * grid_at(row, col, other_clipped)) / 3.0;
        opposed += max(joint - opposed, 0.0) * joint_weight;
    }
    if (p.ratios != 0u) {
        var total = 0.0;
        var weights = 0.0;
        for (var i = 0u; i < 2u; i++) {
            let other = select(b, a, i == 0u);
            let ps = pair_index(color, other);
            let pair = u32(ps.x);
            let unclipped = w.count[other] - w.clipped[other];
            if (unclipped == 0u || p.ratio_measured[pair] == 0u) {
                continue;
            }
            let rec = f32(unclipped) / f32(w.count[other]);
            let weight = rec * rec * rec * rec;
            let mean = w.sum_unclipped[other] / f32(unclipped);
            let log_ratio = grid_at(row, col, 3u + pair);
            total += weight * mean * exp(ps.y * log_ratio);
            weights += weight;
        }
        if (weights > 0.0) {
            let take = min(weights, 1.0) * smooth01((s[color] - RATIO_RAMP_START) / RATIO_RAMP);
            opposed = max(opposed + (total / weights - opposed) * take, floor_);
        }
    }
    var hold = 0.0;
    for (var i = 0u; i < 2u; i++) {
        let c = select(b, a, i == 0u);
        let unclipped = w.count[c] - w.clipped[c];
        var h = 0.0;
        if (unclipped != 0u) {
            let level = w.sum_unclipped[c] / f32(unclipped) / p.clip_wb[c];
            let darkness = smooth01((1.0 - level) / DARK_EDGE_RANGE);
            h = darkness * smooth01(2.0 * f32(unclipped) / f32(w.count[c]));
        }
        hold = max(hold, h);
    }
    let weight = neutral_weight(s) * (1.0 - hold);
    var brightest = 0.0;
    for (var c = 0u; c < 3u; c++) {
        if (w.count[c] > 0u) {
            brightest = max(brightest, w.sum[c] / f32(w.count[c]));
        }
    }
    return vec4<f32>(opposed, weight, brightest, 1.0);
}

// ---- clip levels (detect_clip_levels on the GPU: maxima, onset histograms)

var<workgroup> wg_max: array<atomic<u32>, 3>;
var<workgroup> wg_hist: array<atomic<u32>, 150>;

// One thread per 32 samples of an active row (the f32 input holds the raw
// sample values here).
@compute @workgroup_size(256)
fn hl_max(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if (li < 3u) {
        atomicStore(&wg_max[li], 0u);
    }
    workgroupBarrier();
    let chunks = (p.aw + 31u) / 32u;
    let id = gid.x;
    if (id < chunks * p.ah) {
        let row = p.top + id / chunks;
        let c0 = p.left + (id % chunks) * 32u;
        let c1 = min(c0 + 32u, p.left + p.aw);
        var local = vec3<f32>(0.0);
        for (var col = c0; col < c1; col++) {
            let color = color_at(row, col);
            if (color < 3u) {
                local[color] = max(local[color], data[row * p.width + col]);
            }
        }
        for (var k = 0u; k < 3u; k++) {
            atomicMax(&wg_max[k], bitcast<u32>(local[k]));
        }
    }
    workgroupBarrier();
    if (li < 3u) {
        atomicMax(&counters[C_MAX + li], atomicLoad(&wg_max[li]));
    }
}

@compute @workgroup_size(256)
fn hl_onset(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if (li < 150u) {
        atomicStore(&wg_hist[li], 0u);
    }
    workgroupBarrier();
    let chunks = (p.aw + 31u) / 32u;
    let id = gid.x;
    if (id < chunks * p.ah) {
        var lows: vec3<f32>;
        var bw: vec3<f32>;
        for (var k = 0u; k < 3u; k++) {
            let m = bitcast<f32>(atomicLoad(&counters[C_MAX + k]));
            lows[k] = m * (1.0 - p.misc.y);
            bw[k] = max(m * p.misc.y / p.misc.z, 1.17549435e-38);
        }
        let row = p.top + id / chunks;
        let c0 = p.left + (id % chunks) * 32u;
        let c1 = min(c0 + 32u, p.left + p.aw);
        for (var col = c0; col < c1; col++) {
            let color = color_at(row, col);
            if (color < 3u) {
                let v = data[row * p.width + col];
                if (v >= lows[color]) {
                    let bin = min(u32((v - lows[color]) / bw[color]), ONSET_BINS - 1u);
                    atomicAdd(&wg_hist[color * ONSET_BINS + bin], 1u);
                }
            }
        }
    }
    workgroupBarrier();
    if (li < 150u) {
        let n = atomicLoad(&wg_hist[li]);
        if (n > 0u) {
            atomicAdd(&counters[C_HIST + li], n);
        }
    }
}

// ---- reconstruction

// 1. Clipped-sample mask, one thread per 32-sample word.
@compute @workgroup_size(64, 4)
fn hl_mask(@builtin(global_invocation_id) gid: vec3<u32>) {
    let word = gid.x;
    let row = gid.y;
    if (word >= p.words || row >= p.height) {
        return;
    }
    var bits = 0u;
    var n = 0u;
    if (row >= p.top && row < p.top + p.ah) {
        let c0 = max(word * 32u, p.left);
        let c1 = min(word * 32u + 32u, p.left + p.aw);
        for (var col = c0; col < c1; col++) {
            let color = color_at(row, col);
            if (color < 3u && data[row * p.width + col] >= p.thresholds[color]) {
                bits |= 1u << (col % 32u);
                n += 1u;
            }
        }
    }
    mask[row * p.words + word] = bits;
    if (n > 0u) {
        atomicAdd(&counters[C_CLIPPED], n);
    }
}

// near_clip: clipped within RATIO_REACH along the row ...
@compute @workgroup_size(64, 4)
fn hl_near_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    let word = gid.x;
    let row = gid.y;
    if (word >= p.words || row >= p.height) {
        return;
    }
    let base = row * p.words;
    let cur = mask[base + word];
    var prev = 0u;
    var next = 0u;
    if (word > 0u) {
        prev = mask[base + word - 1u];
    }
    if (word + 1u < p.words) {
        next = mask[base + word + 1u];
    }
    var d = cur;
    for (var k = 1u; k <= RATIO_REACH; k++) {
        d |= (cur << k) | (prev >> (32u - k));
        d |= (cur >> k) | (next << (32u - k));
    }
    hnear[base + word] = d;
}

// ... and across rows.
@compute @workgroup_size(64, 4)
fn hl_near_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    let word = gid.x;
    let row = gid.y;
    if (word >= p.words || row >= p.height) {
        return;
    }
    let r0 = select(row - RATIO_REACH, 0u, row < RATIO_REACH);
    let r1 = min(row + RATIO_REACH, p.height - 1u);
    var d = 0u;
    for (var r = r0; r <= r1; r++) {
        d |= hnear[r * p.words + word];
    }
    near_bits[row * p.words + word] = d;
}

fn is_near(row: u32, col: u32) -> bool {
    return ((near_bits[row * p.words + col / 32u] >> (col % 32u)) & 1u) != 0u;
}

var<workgroup> red_f: array<array<f32, 6>, 256>;
var<workgroup> red_u: array<array<u32, 6>, 256>;

fn reduce(li: u32) {
    for (var stride = 128u; stride > 0u; stride = stride / 2u) {
        workgroupBarrier();
        if (li < stride) {
            for (var k = 0u; k < 6u; k++) {
                red_f[li][k] += red_f[li + stride][k];
                red_u[li][k] += red_u[li + stride][k];
            }
        }
    }
    workgroupBarrier();
}

// 2. Chrominance and ratio candidates, one workgroup per grid cell:
//    per cell sums[3], counts[3], ratio sums[3], ratio counts[3].
@compute @workgroup_size(256)
fn hl_cand(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let gx = wid.x;
    let gy = wid.y;
    var f = array<f32, 6>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    var u = array<u32, 6>(0u, 0u, 0u, 0u, 0u, 0u);
    let row_start = max(gy * CELL, p.top);
    let row_end = max(min((gy + 1u) * CELL, p.top + p.ah), row_start);
    let col_start = max(gx * CELL, p.left);
    let col_end = max(min((gx + 1u) * CELL, p.left + p.aw), col_start);
    for (var k = 0u; k < 16u; k++) {
        let s = li + 256u * k;
        let row = gy * CELL + s / CELL;
        let col = gx * CELL + s % CELL;
        if (row < row_start || row >= row_end || col < col_start || col >= col_end) {
            continue;
        }
        if (is_clipped(row, col)) {
            continue;
        }
        let color = color_at(row, col);
        if (color >= 3u) {
            continue;
        }
        let value = data[row * p.width + col] * p.wb[color];
        if (value < p.clip_wb[color] * CANDIDATE_FRACTION) {
            continue;
        }
        let adjacent = any_clipped_near(row, col);
        let ratio_candidate = p.ratios != 0u && !adjacent && is_near(row, col);
        if (!adjacent && !ratio_candidate) {
            continue;
        }
        let w = window(row, col, 1u, true);
        let mo = mean_others(w, color);
        if (mo.y == 0.0) {
            continue;
        }
        if (value < mo.x * CANDIDATE_MIN_RATIO) {
            continue;
        }
        if (adjacent) {
            f[color] += value - mo.x;
            u[color] += 1u;
        } else if (uniform_around(row, col, color, value, w)) {
            for (var o = 0u; o < 3u; o++) {
                if (o == color) {
                    continue;
                }
                let mean = w.sum[o] / f32(w.count[o]);
                if (mean > 0.0) {
                    let ps = pair_index(color, o);
                    let pair = u32(ps.x);
                    f[3u + pair] += ps.y * log(value / mean);
                    u[3u + pair] += 1u;
                }
            }
        }
    }
    red_f[li] = f;
    red_u[li] = u;
    reduce(li);
    if (li == 0u) {
        let cell = gy * p.grid_cols + gx;
        for (var k = 0u; k < 3u; k++) {
            cells[cell * 12u + k] = red_f[0][k];
            cells[cell * 12u + 3u + k] = f32(red_u[0][k]);
            cells[cell * 12u + 6u + k] = red_f[0][3u + k];
            cells[cell * 12u + 9u + k] = f32(red_u[0][3u + k]);
        }
    }
}

// Clipped-share blocks: samples and (partly) clipped samples per colour.
@compute @workgroup_size(16, 16)
fn hl_blocks(@builtin(global_invocation_id) gid: vec3<u32>) {
    let bx = gid.x;
    let by = gid.y;
    if (bx >= p.share_cols || by >= p.share_rows) {
        return;
    }
    var total = vec3<f32>(0.0);
    var clipped = vec3<f32>(0.0);
    let r1 = min(p.top + (by + 1u) * SHARE_BLOCK, p.top + p.ah);
    let c1 = min(p.left + (bx + 1u) * SHARE_BLOCK, p.left + p.aw);
    for (var row = p.top + by * SHARE_BLOCK; row < r1; row++) {
        for (var col = p.left + bx * SHARE_BLOCK; col < c1; col++) {
            let color = color_at(row, col);
            if (color < 3u) {
                total[color] += 1.0;
                if (is_clipped(row, col)) {
                    clipped[color] += 1.0;
                } else {
                    let x = clamp((data[row * p.width + col] / p.thresholds[color] - (1.0 - NEAR_CLIP)) / NEAR_CLIP, 0.0, 1.0);
                    clipped[color] += x * x * (3.0 - 2.0 * x);
                }
            }
        }
    }
    let b = by * p.share_cols + bx;
    for (var k = 0u; k < 3u; k++) {
        blocks[b * 6u + k] = total[k];
        blocks[b * 6u + 3u + k] = clipped[k];
    }
}

@compute @workgroup_size(16, 16)
fn hl_share(@builtin(global_invocation_id) gid: vec3<u32>) {
    let bx = i32(gid.x);
    let by = i32(gid.y);
    if (u32(bx) >= p.share_cols || u32(by) >= p.share_rows) {
        return;
    }
    var total = vec3<f32>(0.0);
    var clipped = vec3<f32>(0.0);
    for (var y = max(by - SHARE_BLUR, 0); y < min(by + SHARE_BLUR + 1, i32(p.share_rows)); y++) {
        for (var x = max(bx - SHARE_BLUR, 0); x < min(bx + SHARE_BLUR + 1, i32(p.share_cols)); x++) {
            let b = u32(y) * p.share_cols + u32(x);
            for (var k = 0u; k < 3u; k++) {
                total[k] += blocks[b * 6u + k];
                clipped[k] += blocks[b * 6u + 3u + k];
            }
        }
    }
    let b = u32(by) * p.share_cols + u32(bx);
    for (var k = 0u; k < 3u; k++) {
        share[b * 3u + k] = select(clipped[k] / total[k], 0.0, total[k] == 0.0);
    }
}

// 4. The neutral fill level per cell: sums and counts per colour.
@compute @workgroup_size(256)
fn hl_level(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let gx = wid.x;
    let gy = wid.y;
    var f = array<f32, 6>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    var u = array<u32, 6>(0u, 0u, 0u, 0u, 0u, 0u);
    let row_start = max(gy * CELL, p.top);
    let row_end = min((gy + 1u) * CELL, p.top + p.ah);
    let col_start = max(gx * CELL, p.left);
    let col_end = min((gx + 1u) * CELL, p.left + p.aw);
    for (var k = 0u; k < 16u; k++) {
        let s = li + 256u * k;
        let row = gy * CELL + s / CELL;
        let col = gx * CELL + s % CELL;
        if (row < row_start || row >= row_end || col < col_start || col >= col_end) {
            continue;
        }
        if ((row / 2u) % LEVEL_ROW_STEP != 0u) {
            continue;
        }
        let color = color_at(row, col);
        if (color >= 3u) {
            continue;
        }
        var value = 0.0;
        if (is_clipped(row, col)) {
            if (neutral_weight(share_at(row - p.top, col - p.left)) >= 0.5) {
                continue;
            }
            let e = estimate_parts(row, col, color);
            if (e.w == 0.0 || e.y >= 0.5) {
                continue;
            }
            value = e.x;
        } else {
            value = data[row * p.width + col] * p.wb[color];
            if (value < p.clip_wb[color] * CANDIDATE_FRACTION || !any_clipped_near(row, col)) {
                continue;
            }
        }
        f[color] += value;
        u[color] += 1u;
    }
    red_f[li] = f;
    red_u[li] = u;
    reduce(li);
    if (li == 0u) {
        let cell = gy * p.grid_cols + gx;
        for (var k = 0u; k < 3u; k++) {
            cells[cell * 12u + k] = red_f[0][k];
            cells[cell * 12u + 3u + k] = f32(red_u[0][k]);
        }
    }
}

// 5. Raise clipped samples (reads `data`, writes `out`, a copy of it).
@compute @workgroup_size(64, 4)
fn hl_apply(@builtin(global_invocation_id) gid: vec3<u32>) {
    let col = p.left + gid.x;
    let row = p.top + gid.y;
    if (gid.x >= p.aw || gid.y >= p.ah) {
        return;
    }
    if (!is_clipped(row, col)) {
        return;
    }
    let color = color_at(row, col);
    if (color >= 3u) {
        return;
    }
    let e = estimate_parts(row, col, color);
    if (e.w == 0.0) {
        return;
    }
    let level = max(grid_at(row, col, 6u), e.z);
    let estimate = e.x + (level - e.x) * e.y;
    let index = row * p.width + col;
    let current = data[index] * p.wb[color];
    if (estimate > current) {
        result_data[index] = estimate / p.wb[color];
        atomicAdd(&counters[C_RAISED], 1u);
    }
}
