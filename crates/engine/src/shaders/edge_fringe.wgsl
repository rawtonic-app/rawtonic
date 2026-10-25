// SPDX-License-Identifier: AGPL-3.0-or-later
// fringeVersion 5: the edge-fringe pass of version 4 on the GPU
// (edge_fringe/gpu.rs). Each entry point is the per-pixel arithmetic of the
// CPU stage it names, in the same order, over one tile of the image
// (`P.rows` rows of `P.width` pixels, RGB in xyz): the chromatic-aberration
// stage (edge_fringe/axial_reference.rs), the luminance stage (`suppress`)
// and the colour-line stage (`colour_lines`) of edge_fringe.rs. Pixels
// beyond the tile repeat its edge pixels, as pixels beyond the image repeat
// the image's; the pixels within a stage's reach of a tile edge that is not
// the image's are wrong and are not read back (the host's halo).
//
// The luminance and colour-line stages sum each window row by row, left to
// right, as the CPU's horizontal-then-vertical box sums do. Their windows'
// models are kept for `P.radius` virtual rows beyond the tile on each side
// (index m is tile row m - radius): the CPU computes those from windows of
// repeated rows centred beyond the image, and repeats the models only along
// rows (so does the tile).
//
// Whole workgroups skip work no pixel of theirs can need, decided from the
// records of the 8 × 8 blocks around them (`record`, in the tail of `g4`):
// the side search where the luminance its pixels read varies less than the
// edge test's start, and the window models and their sums in the luminance
// and colour-line stages away from windows of some weight. The skipped
// work would not change a pixel (as version 4's skips on the CPU).

struct Params {
    width: u32,
    rows: u32,
    radius: u32,
    search: u32,
    direction_radius: u32,
    side_radius: u32,
    profile_radius: u32,
    blocks_at: u32,
    out_x: u32,
    out_y: u32,
    out_width: u32,
    out_rows: u32,
    band_x: u32,
    band_width: u32,
    blocks_width: u32,
    _pad2: u32,
    noise: f32,
    push: f32,
    inv_area: f32,
    _pad1: f32,
    direction: array<vec4<f32>, 3>,
    side: array<vec4<f32>, 3>,
    profile: array<vec4<f32>, 3>,
}

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read_write> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> dst: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> g0: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> g1: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> g2: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read_write> g3: array<vec4<f32>>;
@group(0) @binding(7) var<storage, read_write> g4: array<vec4<f32>>;
// `dst` as packed RGB: the tile's own pixels to read back (`pack`).
@group(0) @binding(2) var<storage, read_write> packed: array<f32>;
// The band of rows the tile lies in, packed RGB, all columns.
@group(0) @binding(8) var<storage, read> band: array<f32>;

// edge_fringe.rs
const EPS: f32 = 4e-4;
const EDGE_LOW: f32 = 0.03;
const EDGE_HIGH: f32 = 0.08;
const LINE_EPS: f32 = 1e-6;
const TWO_COLOUR_FULL: f32 = 0.05;
const TWO_COLOUR_NONE: f32 = 0.15;
const MIX_FULL: f32 = 0.1;
const MIX_NONE: f32 = 0.25;
// edge_fringe/axial.rs
const STEP_LOW: f32 = 0.1;
const STEP_SPAN: f32 = 0.08;
const STEP_NOISE: f32 = 6.0;
const STOPS_LOW: f32 = 1.5;
const STOPS_HIGH: f32 = 2.5;
const NOISE_TOLERANCE: f32 = 3.0;
const BEYOND_FULL: f32 = 0.1;
const BEYOND_NONE: f32 = 0.25;
const HUE_SHOULDER: f32 = 15.0;
const SMALL_FULL: f32 = 1.3;
const SMALL_NONE: f32 = 2.6;
const STEPS_FULL: f32 = 0.5;
const STEPS_NONE: f32 = 0.8;
// The longest luminance profile of the two-step test: 2 · search + 3 with
// search ≤ 8.
const PROFILE_MAX: u32 = 20u;

fn at(x: i32, y: i32) -> u32 {
    return u32(y) * P.width + u32(x);
}

fn cx(x: i32) -> i32 {
    return clamp(x, 0, i32(P.width) - 1);
}

fn cy(y: i32) -> i32 {
    return clamp(y, 0, i32(P.rows) - 1);
}

fn finite(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

fn all_finite(c: vec3<f32>) -> bool {
    return finite(c.x) && finite(c.y) && finite(c.z);
}

fn enc(v: f32) -> f32 {
    return sqrt(max(v, 0.0));
}

fn lum(c: vec3<f32>) -> f32 {
    return 0.25 * (c.x + 2.0 * c.y + c.z);
}

fn ramp(v: f32, lo: f32, hi: f32) -> f32 {
    let t = clamp((v - lo) / (hi - lo), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn tap(weights: array<vec4<f32>, 3>, k: u32) -> f32 {
    return weights[k / 4u][k % 4u];
}

// The record of 8 × 8 block (bx, by): x, y = the smallest and largest
// direction luminance of its pixels (blocks of tile rows); z = whether a
// colour-line window of it has weight, w = whether a luminance window of it
// is near an edge (1; blocks of virtual rows).
fn record(bx: i32, by: i32) -> u32 {
    return P.blocks_at + u32(by) * P.blocks_width + u32(bx);
}

// The blocks over columns x0 ..= x1 and rows y0 ..= y1 of a grid over
// `rows` rows: first and last column, first and last row.
fn blocks_over(x0: i32, x1: i32, y0: i32, y1: i32, rows: u32) -> vec4<i32> {
    let columns = i32((P.width + 7u) / 8u);
    let block_rows = i32((rows + 7u) / 8u);
    return vec4<i32>(
        max(x0, 0) / 8,
        min(x1 / 8, columns - 1),
        max(y0, 0) / 8,
        min(y1 / 8, block_rows - 1),
    );
}

// Whether a window in columns x0 ..= x1 and virtual rows y0 ..= y1 lies in
// a block with `flag` (2 or 3, the record's component) set.
fn flagged(x0: i32, x1: i32, y0: i32, y1: i32, flag: u32) -> bool {
    let b = blocks_over(x0, x1, y0, y1, P.rows + 2u * P.radius);
    for (var j = b.z; j <= b.w; j++) {
        for (var i = b.x; i <= b.y; i++) {
            if g4[record(i, j)][flag] > 0.0 {
                return true;
            }
        }
    }
    return false;
}

var<workgroup> group_low: atomic<u32>;
var<workgroup> group_high: atomic<u32>;
var<workgroup> group_any: atomic<u32>;
var<workgroup> group_active: u32;
// Row sums of the window passes (8 × 8 groups): up to nine values for each
// of the 8 + 2r rows and 8 columns the group's windows span, value-major.
const ROW_STRIDE: i32 = 112;
var<workgroup> row_sums: array<f32, 1008>;

fn row_sum(k: i32, row: i32, col: i32) -> i32 {
    return k * ROW_STRIDE + row * 8 + col;
}

// Whether the group (window block `wid`) has work: `flagged` around it as
// one uniform value; every invocation of the group calls it.
fn group_has(wid: vec3<u32>, li: u32, reach: i32, flag: u32) -> bool {
    if li == 0u {
        let x0 = i32(wid.x) * 8;
        let y0 = i32(wid.y) * 8;
        group_active = u32(flagged(x0 - reach, x0 + 7 + reach, y0 - reach, y0 + 7 + reach, flag));
    }
    return workgroupUniformLoad(&group_active) != 0u;
}

// Marks the group's block (an 8 × 8 group's) with `flag` (2 or 3) where
// any invocation's `marked` is true; every invocation of the group calls it.
fn mark_block(wid: vec3<u32>, li: u32, marked: bool, flag: u32) {
    if li == 0u {
        atomicStore(&group_any, 0u);
    }
    workgroupBarrier();
    if marked {
        atomicOr(&group_any, 1u);
    }
    workgroupBarrier();
    if li == 0u {
        g4[record(i32(wid.x), i32(wid.y))][flag] = f32(atomicLoad(&group_any));
    }
}

// The tile's pixels from the band into `src`.
@compute @workgroup_size(16, 16)
fn unpack(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.width || id.y >= P.rows {
        return;
    }
    let j = 3u * (id.y * P.band_width + P.band_x + id.x);
    let c = vec3<f32>(band[j], band[j + 1u], band[j + 2u]);
    // w: the square-root luminance the chromatic-aberration stage reads.
    src[at(i32(id.x), i32(id.y))] = vec4<f32>(c, sqrt(max(lum(c), 0.0)));
}

// The tile's own pixels of `src`, packed RGB, into `dst`.
@compute @workgroup_size(16, 16)
fn pack(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.out_width || id.y >= P.out_rows {
        return;
    }
    let v = src[at(i32(id.x + P.out_x), i32(id.y + P.out_y))];
    let j = 3u * (id.y * P.out_width + id.x);
    packed[j] = v.x;
    packed[j + 1u] = v.y;
    packed[j + 2u] = v.z;
}

// ---- Chromatic-aberration stage (version 4's first) ----

fn root_at(x: i32, y: i32) -> f32 {
    return src[at(x, y)].w;
}

// Gaussians along rows: g0 = (direction, profile) of the square-root
// luminance, g1 = the side colours.
@compute @workgroup_size(16, 16)
fn axial_across(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.width || id.y >= P.rows {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    var direction = 0.0;
    let rd = i32(P.direction_radius);
    for (var k = 0; k <= 2 * rd; k++) {
        direction += tap(P.direction, u32(k)) * root_at(cx(x - rd + k), y);
    }
    var profile = 0.0;
    let rp = i32(P.profile_radius);
    for (var k = 0; k <= 2 * rp; k++) {
        profile += tap(P.profile, u32(k)) * root_at(cx(x - rp + k), y);
    }
    var colour = vec3<f32>(0.0);
    let rs = i32(P.side_radius);
    for (var k = 0; k <= 2 * rs; k++) {
        let w = tap(P.side, u32(k));
        let c = src[at(cx(x - rs + k), y)].xyz;
        colour = vec3<f32>(colour.x + w * c.x, colour.y + w * c.y, colour.z + w * c.z);
    }
    let i = at(x, y);
    g0[i] = vec4<f32>(direction, profile, 0.0, 0.0);
    g1[i] = vec4<f32>(colour, 0.0);
}

// The same along columns: g2 = (direction, profile), g3 = side colours,
// and the block's direction range.
@compute @workgroup_size(8, 8)
fn axial_down(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    if li == 0u {
        atomicStore(&group_low, 0x7f800000u);
        atomicStore(&group_high, 0u);
    }
    workgroupBarrier();
    if id.x < P.width && id.y < P.rows {
        let x = i32(id.x);
        let y = i32(id.y);
        var direction = 0.0;
        let rd = i32(P.direction_radius);
        for (var k = 0; k <= 2 * rd; k++) {
            direction += tap(P.direction, u32(k)) * g0[at(x, cy(y - rd + k))].x;
        }
        var profile = 0.0;
        let rp = i32(P.profile_radius);
        for (var k = 0; k <= 2 * rp; k++) {
            profile += tap(P.profile, u32(k)) * g0[at(x, cy(y - rp + k))].y;
        }
        var colour = vec3<f32>(0.0);
        let rs = i32(P.side_radius);
        for (var k = 0; k <= 2 * rs; k++) {
            let w = tap(P.side, u32(k));
            let c = g1[at(x, cy(y - rs + k))].xyz;
            colour = vec3<f32>(colour.x + w * c.x, colour.y + w * c.y, colour.z + w * c.z);
        }
        let i = at(x, y);
        g2[i] = vec4<f32>(direction, profile, 0.0, 0.0);
        g3[i] = vec4<f32>(colour, 0.0);
        // Non-negative: the bits order as the values (a -0 as 0).
        let bits = bitcast<u32>(abs(direction));
        atomicMin(&group_low, bits);
        atomicMax(&group_high, bits);
    }
    workgroupBarrier();
    if li == 0u {
        let b = record(i32(wid.x), i32(wid.y));
        g4[b] = vec4<f32>(
            bitcast<f32>(atomicLoad(&group_low)),
            bitcast<f32>(atomicLoad(&group_high)),
            g4[b].z,
            g4[b].w,
        );
    }
}

// A bilinear read's four buffer indices and weights (`Planes::read`): the
// weights come from the offset alone, positions clamp to the tile.
struct Bilinear {
    i00: u32,
    i10: u32,
    i01: u32,
    i11: u32,
    fx: f32,
    fy: f32,
}

fn bilinear(x: i32, y: i32, ox: f32, oy: f32) -> Bilinear {
    var b: Bilinear;
    var xa: i32;
    var xb: i32;
    var ya: i32;
    var yb: i32;
    let wx = floor(ox);
    let a = x + i32(wx);
    if a < 0 {
        xa = 0;
        xb = 0;
        b.fx = 0.0;
    } else if a >= i32(P.width) - 1 {
        xa = i32(P.width) - 1;
        xb = xa;
        b.fx = 0.0;
    } else {
        xa = a;
        xb = a + 1;
        b.fx = ox - wx;
    }
    let wy = floor(oy);
    let c = y + i32(wy);
    if c < 0 {
        ya = 0;
        yb = 0;
        b.fy = 0.0;
    } else if c >= i32(P.rows) - 1 {
        ya = i32(P.rows) - 1;
        yb = ya;
        b.fy = 0.0;
    } else {
        ya = c;
        yb = c + 1;
        b.fy = oy - wy;
    }
    b.i00 = at(xa, ya);
    b.i10 = at(xb, ya);
    b.i01 = at(xa, yb);
    b.i11 = at(xb, yb);
    return b;
}

fn mix2(v00: f32, v10: f32, v01: f32, v11: f32, b: Bilinear) -> f32 {
    let top = v00 * (1.0 - b.fx) + v10 * b.fx;
    let bottom = v01 * (1.0 - b.fx) + v11 * b.fx;
    return top * (1.0 - b.fy) + bottom * b.fy;
}

fn direction_at(x: i32, y: i32, ox: f32, oy: f32) -> f32 {
    let b = bilinear(x, y, ox, oy);
    return mix2(g2[b.i00].x, g2[b.i10].x, g2[b.i01].x, g2[b.i11].x, b);
}

fn profile_at(x: i32, y: i32, ox: f32, oy: f32) -> f32 {
    let b = bilinear(x, y, ox, oy);
    return mix2(g2[b.i00].y, g2[b.i10].y, g2[b.i01].y, g2[b.i11].y, b);
}

fn side_at(x: i32, y: i32, ox: f32, oy: f32) -> vec3<f32> {
    let b = bilinear(x, y, ox, oy);
    let c00 = g3[b.i00].xyz;
    let c10 = g3[b.i10].xyz;
    let c01 = g3[b.i01].xyz;
    let c11 = g3[b.i11].xyz;
    return vec3<f32>(
        mix2(c00.x, c10.x, c01.x, c11.x, b),
        mix2(c00.y, c10.y, c01.y, c11.y, b),
        mix2(c00.z, c10.z, c01.z, c11.z, b),
    );
}

fn cbrt(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    return pow(v, 1.0 / 3.0);
}

fn oklab(c: vec3<f32>) -> vec3<f32> {
    let l = 0.41222147 * c.x + 0.53633255 * c.y + 0.051445995 * c.z;
    let m = 0.2119035 * c.x + 0.6806995 * c.y + 0.10739696 * c.z;
    let s = 0.08830246 * c.x + 0.28171885 * c.y + 0.6299787 * c.z;
    let l3 = cbrt(max(l, 0.0));
    let m3 = cbrt(max(m, 0.0));
    let s3 = cbrt(max(s, 0.0));
    return vec3<f32>(
        0.21045426 * l3 + 0.7936178 * m3 - 0.004072047 * s3,
        1.9779985 * l3 - 2.4285922 * m3 + 0.4505937 * s3,
        0.025904037 * l3 + 0.78277177 * m3 - 0.80867577 * s3,
    );
}

fn rem_360(v: f32) -> f32 {
    let r = v - 360.0 * trunc(v / 360.0);
    if r < 0.0 {
        return r + 360.0;
    }
    return r;
}

fn in_hue(hue: f32, lo: f32, hi: f32) -> f32 {
    let half = 0.5 * (hi - lo);
    let off = rem_360(rem_360(hue - 0.5 * (lo + hi)) + 180.0) - 180.0;
    return 1.0 - ramp(abs(off), half, half + HUE_SHOULDER);
}

// The dip between two luminance steps on the path from the dark side to
// the bright side, relative to the steeper step (`two_steps`).
fn two_steps(x: i32, y: i32, dx: f32, dy: f32, to_dark: f32, to_bright: f32) -> f32 {
    let first = -i32(to_dark) - 1;
    let last = i32(to_bright) + 1;
    var steps: array<f32, PROFILE_MAX>;
    var n = 0u;
    for (var k = first; k <= last; k++) {
        let kf = f32(k);
        steps[n] = profile_at(x, y, kf * dx, kf * dy);
        n++;
    }
    var steepest = 0.0;
    for (var j = 0u; j + 1u < n; j++) {
        steepest = max(steepest, steps[j + 1u] - steps[j]);
    }
    if steepest <= 0.0 {
        return 0.0;
    }
    var after: array<f32, PROFILE_MAX>;
    for (var j = i32(n) - 2; j >= 0; j--) {
        let u = u32(j);
        if u + 2u < n {
            after[u] = max(steps[u + 2u] - steps[u + 1u], after[u + 1u]);
        } else {
            after[u] = 0.0;
        }
    }
    var before = 0.0;
    var dip = 0.0;
    for (var j = 0u; j + 1u < n; j++) {
        let rise = steps[j + 1u] - steps[j];
        dip = max(dip, min(before, after[j]) - rise);
        before = max(before, rise);
    }
    return dip / steepest;
}

// Each pixel's move (`pixel_move`): g4 = (goal, weight), g0.x = the cap,
// below 0 where the pixel does not move (`None`).
@compute @workgroup_size(16, 16)
fn axial_moves(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    if id.x >= P.width || id.y >= P.rows {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = at(x, y);
    g4[i] = vec4<f32>(0.0);
    g0[i] = vec4<f32>(-1.0, 0.0, 0.0, 0.0);
    let low = max(STEP_LOW, STEP_NOISE * P.noise);
    // Every luminance the side search reads lies within search + 1 of the
    // pixel: where the blocks there vary less than the edge test's start
    // (with a margin for the reads' rounding), no pixel of the group
    // changes.
    var darkest = 3.40282347e38;
    var brightest = 0.0;
    let reach = i32(P.search) + 2;
    let x0 = i32(wid.x) * 16;
    let y0 = i32(wid.y) * 16;
    let near = blocks_over(x0 - reach, x0 + 15 + reach, y0 - reach, y0 + 15 + reach, P.rows);
    for (var by = near.z; by <= near.w; by++) {
        for (var bx = near.x; bx <= near.y; bx++) {
            let range = g4[record(bx, by)];
            darkest = min(darkest, range.x);
            brightest = max(brightest, range.y);
        }
    }
    if brightest - darkest < low * (1.0 - 1e-4) {
        return;
    }
    let p = src[i].xyz;
    if !all_finite(p) {
        return;
    }
    let gx = 0.5 * (direction_at(x, y, 1.0, 0.0) - direction_at(x, y, -1.0, 0.0));
    let gy = 0.5 * (direction_at(x, y, 0.0, 1.0) - direction_at(x, y, 0.0, -1.0));
    let g = sqrt(gx * gx + gy * gy);
    if !(g > 1e-5) {
        return;
    }
    let dx = gx / g;
    let dy = gy / g;
    let own = direction_at(x, y, 0.0, 0.0);
    var bright = own;
    var to_bright = 0.0;
    var dark = own;
    var to_dark = 0.0;
    for (var k = 1u; k <= P.search; k++) {
        let kf = f32(k);
        let ahead = direction_at(x, y, kf * dx, kf * dy);
        if ahead > bright {
            bright = ahead;
            to_bright = kf;
        }
        let behind = direction_at(x, y, -kf * dx, -kf * dy);
        if behind < dark {
            dark = behind;
            to_dark = kf;
        }
    }
    let edge = ramp(bright - dark, low, low + STEP_SPAN);
    if edge <= 0.0 {
        return;
    }
    let kl = to_bright + P.push;
    let lit = side_at(x, y, kl * dx, kl * dy);
    let ks = -(to_dark + P.push);
    let shade = side_at(x, y, ks * dx, ks * dy);
    let l_lit = lum(lit);
    let l_shade = lum(shade);
    let l_own = lum(p);
    if !(l_lit - l_shade > 1e-6) {
        return;
    }
    let t = clamp((l_own - l_shade) / (l_lit - l_shade), 0.0, 1.0);
    let mixed = vec3<f32>(
        shade.x + t * (lit.x - shade.x),
        shade.y + t * (lit.y - shade.y),
        shade.z + t * (lit.z - shade.z),
    );
    let span = max(max(max(max(0.0, abs(lit.x - shade.x)), abs(lit.y - shade.y)), abs(lit.z - shade.z)), 1e-6);
    var beyond = 0.0;
    for (var c = 0u; c < 3u; c++) {
        let tolerance = NOISE_TOLERANCE * 2.0 * sqrt(max(p[c], 0.0)) * P.noise;
        let lo = min(lit[c], shade[c]) - tolerance;
        let hi = max(lit[c], shade[c]) + tolerance;
        beyond = max(beyond, max(max(p[c] - hi, lo - p[c]), 0.0) / span);
    }
    let between = 1.0 - ramp(beyond, BEYOND_FULL, BEYOND_NONE);
    let contrast = ramp(log2(max(l_lit, 1e-6) / max(l_shade, 1e-6)), STOPS_LOW, STOPS_HIGH);
    let l_mix = lum(mixed);
    var colour = vec3<f32>(mixed.x - l_mix, mixed.y - l_mix, mixed.z - l_mix);
    let d = vec3<f32>(p.x - l_own, p.y - l_own, p.z - l_own);
    let had = sqrt(d.x * d.x + d.y * d.y + d.z * d.z);
    let gets = sqrt(colour.x * colour.x + colour.y * colour.y + colour.z * colour.z);
    if gets > had && gets > 0.0 {
        colour = vec3<f32>(colour.x * (had / gets), colour.y * (had / gets), colour.z * (had / gets));
    }
    let goal = vec3<f32>(l_own + colour.x, l_own + colour.y, l_own + colour.z);
    let a = oklab(p);
    let b = oklab(goal);
    let da = a.y - b.y;
    let db = a.z - b.z;
    let hue = rem_360(degrees(atan2(db, da)));
    var small = 0.0;
    if P.noise > 0.0 {
        small = 1.0 - ramp(sqrt(da * da + db * db), SMALL_FULL * P.noise, SMALL_NONE * P.noise);
    }
    let family = max(max(in_hue(hue, 255.0, 355.0), in_hue(hue, 95.0, 200.0)), small);
    let weight = edge * between * contrast * family;
    let cap = edge * contrast * family;
    var single = 1.0;
    if cap > 0.0 {
        single = 1.0 - ramp(two_steps(x, y, dx, dy, to_dark, to_bright), STEPS_FULL, STEPS_NONE);
    }
    g4[i] = vec4<f32>(goal, weight * single);
    g0[i] = vec4<f32>(cap * single, 0.0, 0.0, 0.0);
}

// The moves applied (`tile_changes`): a pixel left out amid moved
// neighbours moves with their median weight up to its cap.
@compute @workgroup_size(16, 16)
fn axial_apply(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.width || id.y >= P.rows {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = at(x, y);
    let px = src[i];
    dst[i] = px;
    let cap = g0[i].x;
    if cap < 0.0 {
        return;
    }
    let own = g4[i];
    var near: array<f32, 9>;
    var n = 0u;
    for (var ny = max(y - 1, 0); ny < min(y + 2, i32(P.rows)); ny++) {
        for (var nx = max(x - 1, 0); nx < min(x + 2, i32(P.width)); nx++) {
            let j = at(nx, ny);
            var w = 0.0;
            if g0[j].x >= 0.0 {
                w = g4[j].w;
            }
            // Insertion sort.
            var k = n;
            while k > 0u && near[k - 1u] > w {
                near[k] = near[k - 1u];
                k--;
            }
            near[k] = w;
            n++;
        }
    }
    let weight = max(own.w, min(near[n / 2u], cap));
    if weight <= 0.0 {
        return;
    }
    let p = px.xyz;
    dst[i] = vec4<f32>(
        p.x + weight * (own.x - p.x),
        p.y + weight * (own.y - p.y),
        p.z + weight * (own.z - p.z),
        px.w,
    );
}

// ---- Luminance stage (`suppress`) ----

// The edge weight of each window at virtual row id.y (tile row id.y -
// radius) into g1.x, and its block's record. Each window sums the row sums
// of its 2r + 1 rows, as the CPU's box sums do.
@compute @workgroup_size(8, 8)
fn suppress_edges(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let r = i32(P.radius);
    let first = i32(wid.y) * 8 - 2 * r;
    for (var e = i32(li); e < (8 + 2 * r) * 8; e += 64) {
        let row = e / 8;
        let col = e % 8;
        let x = i32(wid.x) * 8 + col;
        let yy = cy(first + row);
        var ry = 0.0;
        var ryy = 0.0;
        for (var k = -r; k <= r; k++) {
            let px = src[at(cx(x + k), yy)].xyz;
            let l = enc(0.25 * (px.x + 2.0 * px.y + px.z));
            ry += l;
            ryy += l * l;
        }
        row_sums[row_sum(0, row, col)] = ry;
        row_sums[row_sum(1, row, col)] = ryy;
    }
    workgroupBarrier();
    var edge = 0.0;
    if id.x < P.width && id.y < P.rows + 2u * P.radius {
        var sy = 0.0;
        var syy = 0.0;
        for (var j = 0; j <= 2 * r; j++) {
            sy += row_sums[row_sum(0, i32(lid.y) + j, i32(lid.x))];
            syy += row_sums[row_sum(1, i32(lid.y) + j, i32(lid.x))];
        }
        let ia = P.inv_area;
        let my = sy * ia;
        let var_y = max(syy * ia - my * my, 0.0);
        edge = clamp((sqrt(var_y) - EDGE_LOW) / (EDGE_HIGH - EDGE_LOW), 0.0, 1.0);
        g1[at(i32(id.x), i32(id.y))] = vec4<f32>(edge, 0.0, 0.0, 0.0);
    }
    mark_block(wid, li, edge > 0.0, 3u);
}

// The window model at virtual row id.y: g0 = the slopes and offsets of u
// and v on Y. Only windows within 2r of an edge window are read
// (`suppress_apply`).
@compute @workgroup_size(8, 8)
fn suppress_models(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let r = i32(P.radius);
    if !group_has(wid, li, 2 * r, 3u) {
        return;
    }
    let first = i32(wid.y) * 8 - 2 * r;
    for (var e = i32(li); e < (8 + 2 * r) * 8; e += 64) {
        let row = e / 8;
        let col = e % 8;
        let x = i32(wid.x) * 8 + col;
        let yy = cy(first + row);
        var r0 = vec4<f32>(0.0);
        var r1 = vec2<f32>(0.0);
        for (var k = -r; k <= r; k++) {
            let px = src[at(cx(x + k), yy)].xyz;
            let g = enc(px.y);
            let l = enc(0.25 * (px.x + 2.0 * px.y + px.z));
            let u = enc(px.x) - g;
            let v = enc(px.z) - g;
            r0 = vec4<f32>(r0.x + l, r0.y + u, r0.z + v, r0.w + l * l);
            r1 = vec2<f32>(r1.x + l * u, r1.y + l * v);
        }
        row_sums[row_sum(0, row, col)] = r0.x;
        row_sums[row_sum(1, row, col)] = r0.y;
        row_sums[row_sum(2, row, col)] = r0.z;
        row_sums[row_sum(3, row, col)] = r0.w;
        row_sums[row_sum(4, row, col)] = r1.x;
        row_sums[row_sum(5, row, col)] = r1.y;
    }
    workgroupBarrier();
    if id.x >= P.width || id.y >= P.rows + 2u * P.radius {
        return;
    }
    var s0 = vec4<f32>(0.0);
    var s1 = vec2<f32>(0.0);
    for (var j = 0; j <= 2 * r; j++) {
        let row = i32(lid.y) + j;
        let col = i32(lid.x);
        s0 = vec4<f32>(
            s0.x + row_sums[row_sum(0, row, col)],
            s0.y + row_sums[row_sum(1, row, col)],
            s0.z + row_sums[row_sum(2, row, col)],
            s0.w + row_sums[row_sum(3, row, col)],
        );
        s1 = vec2<f32>(s1.x + row_sums[row_sum(4, row, col)], s1.y + row_sums[row_sum(5, row, col)]);
    }
    let ia = P.inv_area;
    let my = s0.x * ia;
    let mu = s0.y * ia;
    let mv = s0.z * ia;
    let var_y = max(s0.w * ia - my * my, 0.0);
    let au = (s1.x * ia - my * mu) / (var_y + EPS);
    let av = (s1.y * ia - my * mv) / (var_y + EPS);
    g0[at(i32(id.x), i32(id.y))] = vec4<f32>(au, mu - au * my, av, mv - av * my);
}

@compute @workgroup_size(16, 16)
fn suppress_apply(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    if id.x >= P.width || id.y >= P.rows {
        return;
    }
    let r = i32(P.radius);
    let x = i32(id.x);
    let y = i32(id.y);
    let i = at(x, y);
    let px = src[i];
    dst[i] = px;
    // The group's pixels read windows r columns beside them and up to 2r
    // virtual rows below.
    let gx = i32(wid.x) * 16;
    let gy = i32(wid.y) * 16;
    if !flagged(gx - r, gx + 15 + r, gy, gy + 15 + 2 * r, 3u) {
        return;
    }
    // The windows of rows y - r ..= y + r: virtual rows y ..= y + 2r. Their
    // edge weights first: most pixels lie away from strong edges.
    var m1 = 0.0;
    for (var j = 0; j <= 2 * r; j++) {
        var r1 = 0.0;
        for (var k = -r; k <= r; k++) {
            r1 += g1[at(cx(x + k), y + j)].x;
        }
        m1 += r1;
    }
    let ia = P.inv_area;
    let weight = min(2.0 * m1 * ia, 1.0);
    if weight <= 0.0 {
        return;
    }
    let p = px.xyz;
    if !all_finite(p) || p.x < 0.0 || p.y < 0.0 || p.z < 0.0 {
        return;
    }
    var m0 = vec4<f32>(0.0);
    for (var j = 0; j <= 2 * r; j++) {
        var r0 = vec4<f32>(0.0);
        for (var k = -r; k <= r; k++) {
            let c = g0[at(cx(x + k), y + j)];
            r0 = vec4<f32>(r0.x + c.x, r0.y + c.y, r0.z + c.z, r0.w + c.w);
        }
        m0 = vec4<f32>(m0.x + r0.x, m0.y + r0.y, m0.z + r0.z, m0.w + r0.w);
    }
    let g = enc(p.y);
    let l = enc(0.25 * (p.x + 2.0 * p.y + p.z));
    let u0 = enc(p.x) - g;
    let v0 = enc(p.z) - g;
    var u1 = (m0.x * l + m0.y) * ia;
    var v1 = (m0.z * l + m0.w) * ia;
    let c0 = sqrt(u0 * u0 + v0 * v0);
    let c1 = sqrt(u1 * u1 + v1 * v1);
    if c1 > c0 {
        var k = 0.0;
        if c1 > 0.0 {
            k = c0 / c1;
        }
        u1 *= k;
        v1 *= k;
    }
    let red = max(g + u0 + weight * (u1 - u0), 0.0);
    let blue = max(g + v0 + weight * (v1 - v0), 0.0);
    dst[i] = vec4<f32>(red * red, p.y, blue * blue, px.w);
}

// ---- Colour-line stage (`colour_lines`) ----

struct Principal {
    axis: vec3<f32>,
    variance: f32,
}

fn mul_cov(c: array<f32, 6>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        c[0] * v.x + c[1] * v.y + c[2] * v.z,
        c[1] * v.x + c[3] * v.y + c[4] * v.z,
        c[2] * v.x + c[4] * v.y + c[5] * v.z,
    );
}

// `principal_of`: power iteration from the covariance's largest column.
fn principal_of(c: array<f32, 6>) -> Principal {
    let a = c[0];
    let b = c[1];
    let cc = c[2];
    let d = c[3];
    let e = c[4];
    let f = c[5];
    let n0 = a * a + b * b + cc * cc;
    let n1 = b * b + d * d + e * e;
    let n2 = cc * cc + e * e + f * f;
    var v: vec3<f32>;
    if n0 >= n1 && n0 >= n2 {
        v = vec3<f32>(a, b, cc);
    } else if n1 >= n2 {
        v = vec3<f32>(b, d, e);
    } else {
        v = vec3<f32>(cc, e, f);
    }
    for (var k = 0; k < 3; k++) {
        let inv = 1.0 / sqrt(v.x * v.x + v.y * v.y + v.z * v.z);
        v = mul_cov(c, vec3<f32>(v.x * inv, v.y * inv, v.z * inv));
    }
    let n = sqrt(v.x * v.x + v.y * v.y + v.z * v.z);
    var out: Principal;
    if !(n > 1e-12 && finite(n)) {
        out.axis = vec3<f32>(0.0);
        out.variance = 0.0;
        return out;
    }
    v = vec3<f32>(v.x / n, v.y / n, v.z / n);
    let cv = mul_cov(c, v);
    out.axis = v;
    out.variance = max(v.x * cv.x + v.y * cv.y + v.z * cv.z, 0.0);
    return out;
}

// Means and covariances (`moments`) from sums of the three channels and
// their six products.
fn covariance(s: array<f32, 9>, ia: f32) -> array<f32, 6> {
    let m = vec3<f32>(s[0] * ia, s[1] * ia, s[2] * ia);
    return array<f32, 6>(
        max(s[3] * ia - m.x * m.x, 0.0),
        s[4] * ia - m.x * m.y,
        s[5] * ia - m.x * m.z,
        max(s[6] * ia - m.y * m.y, 0.0),
        s[7] * ia - m.y * m.z,
        max(s[8] * ia - m.z * m.z, 0.0),
    );
}

// `line_weights` of one window.
fn store_row_sums(v: array<f32, 9>, row: i32, col: i32) {
    row_sums[row_sum(0, row, col)] = v[0];
    row_sums[row_sum(1, row, col)] = v[1];
    row_sums[row_sum(2, row, col)] = v[2];
    row_sums[row_sum(3, row, col)] = v[3];
    row_sums[row_sum(4, row, col)] = v[4];
    row_sums[row_sum(5, row, col)] = v[5];
    row_sums[row_sum(6, row, col)] = v[6];
    row_sums[row_sum(7, row, col)] = v[7];
    row_sums[row_sum(8, row, col)] = v[8];
}

// The nine window sums of the window in group column `col` and row `row`:
// its 2r + 1 row sums added top to bottom.
fn window_sums(col: i32, row: i32, r: i32) -> array<f32, 9> {
    var out = array<f32, 9>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (var j = 0; j <= 2 * r; j++) {
        out[0] += row_sums[row_sum(0, row + j, col)];
        out[1] += row_sums[row_sum(1, row + j, col)];
        out[2] += row_sums[row_sum(2, row + j, col)];
        out[3] += row_sums[row_sum(3, row + j, col)];
        out[4] += row_sums[row_sum(4, row + j, col)];
        out[5] += row_sums[row_sum(5, row + j, col)];
        out[6] += row_sums[row_sum(6, row + j, col)];
        out[7] += row_sums[row_sum(7, row + j, col)];
        out[8] += row_sums[row_sum(8, row + j, col)];
    }
    return out;
}

fn line_weight(c: array<f32, 6>) -> f32 {
    let total = c[0] + c[3] + c[5];
    let off = c[1] * c[1] + c[2] * c[2] + c[4] * c[4];
    let frobenius = sqrt(c[0] * c[0] + c[3] * c[3] + c[5] * c[5] + 2.0 * off);
    let bound = min(total, frobenius);
    let passes = bound > 3.0 * EDGE_LOW * EDGE_LOW && total - bound < TWO_COLOUR_NONE * bound;
    if !passes {
        return 0.0;
    }
    let variance = principal_of(c).variance;
    if !(variance > 0.0) {
        return 0.0;
    }
    let off_line = max((total - variance) / max(variance, 1e-20), 0.0);
    let edge = clamp((sqrt(variance / 3.0) - EDGE_LOW) / (EDGE_HIGH - EDGE_LOW), 0.0, 1.0);
    let two = clamp((TWO_COLOUR_NONE - off_line) / (TWO_COLOUR_NONE - TWO_COLOUR_FULL), 0.0, 1.0);
    return edge * two;
}

// The colour-line weight of each window at virtual row id.y into g2.y, and
// its block's record.
@compute @workgroup_size(8, 8)
fn line_weights(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let r = i32(P.radius);
    let first = i32(wid.y) * 8 - 2 * r;
    for (var e = i32(li); e < (8 + 2 * r) * 8; e += 64) {
        let row = e / 8;
        let col = e % 8;
        let x = i32(wid.x) * 8 + col;
        let yy = cy(first + row);
        var rr = array<f32, 9>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for (var k = -r; k <= r; k++) {
            let px = src[at(cx(x + k), yy)].xyz;
            let q = vec3<f32>(enc(px.x), enc(px.y), enc(px.z));
            rr[0] += q.x;
            rr[1] += q.y;
            rr[2] += q.z;
            rr[3] += q.x * q.x;
            rr[4] += q.x * q.y;
            rr[5] += q.x * q.z;
            rr[6] += q.y * q.y;
            rr[7] += q.y * q.z;
            rr[8] += q.z * q.z;
        }
        store_row_sums(rr, row, col);
    }
    workgroupBarrier();
    var weight = 0.0;
    if id.x < P.width && id.y < P.rows + 2u * P.radius {
        weight = line_weight(covariance(window_sums(i32(lid.x), i32(lid.y), r), P.inv_area));
        g2[at(i32(id.x), i32(id.y))] = vec4<f32>(0.0, weight, 0.0, 0.0);
    }
    mark_block(wid, li, weight > 0.0, 2u);
}

// The window at virtual row id.y: its projection onto its colour line as
// ten map entries (g0 = offsets, slopes 0; g1 = slopes 1–4; g2 = slope 5,
// weight) and its mean (g3) and axis (g4). Only windows within 2r of a
// window of some weight are read (`line_apply`).
@compute @workgroup_size(8, 8)
fn line_maps(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let r = i32(P.radius);
    if !group_has(wid, li, 2 * r, 2u) {
        return;
    }
    let first = i32(wid.y) * 8 - 2 * r;
    for (var e = i32(li); e < (8 + 2 * r) * 8; e += 64) {
        let row = e / 8;
        let col = e % 8;
        let x = i32(wid.x) * 8 + col;
        let yy = cy(first + row);
        var rl = array<f32, 9>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for (var k = -r; k <= r; k++) {
            let px = src[at(cx(x + k), yy)].xyz;
            let l = vec3<f32>(max(px.x, 0.0), max(px.y, 0.0), max(px.z, 0.0));
            rl[0] += l.x;
            rl[1] += l.y;
            rl[2] += l.z;
            rl[3] += l.x * l.x;
            rl[4] += l.x * l.y;
            rl[5] += l.x * l.z;
            rl[6] += l.y * l.y;
            rl[7] += l.y * l.z;
            rl[8] += l.z * l.z;
        }
        store_row_sums(rl, row, col);
    }
    workgroupBarrier();
    if id.x >= P.width || id.y >= P.rows + 2u * P.radius {
        return;
    }
    let x = i32(id.x);
    let lin = window_sums(i32(lid.x), i32(lid.y), r);
    let ia = P.inv_area;
    let i = at(x, i32(id.y));
    let weight = g2[i].y;
    let mean = vec3<f32>(lin[0] * ia, lin[1] * ia, lin[2] * ia);
    let principal = principal_of(covariance(lin, ia));
    let e = principal.axis;
    let variance = principal.variance;
    // A window of one colour (a clipped patch) maps to its mean.
    let shrink = variance / (variance + LINE_EPS);
    let t = shrink * (e.x * mean.x + e.y * mean.y + e.z * mean.z);
    var w = 0.0;
    if variance > 0.0 {
        w = weight;
    }
    g0[i] = vec4<f32>(mean.x - t * e.x, mean.y - t * e.y, mean.z - t * e.z, shrink * e.x * e.x);
    g1[i] = vec4<f32>(shrink * e.x * e.y, shrink * e.x * e.z, shrink * e.y * e.y, shrink * e.y * e.z);
    g2[i] = vec4<f32>(shrink * e.z * e.z, w, 0.0, 0.0);
    g3[i] = vec4<f32>(mean, 0.0);
    g4[i] = vec4<f32>(e, 0.0);
}

fn along(axis: vec3<f32>, mean: vec3<f32>, q: vec3<f32>) -> f32 {
    return axis.x * (q.x - mean.x) + axis.y * (q.y - mean.y) + axis.z * (q.z - mean.z);
}

@compute @workgroup_size(16, 16)
fn line_apply(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    if id.x >= P.width || id.y >= P.rows {
        return;
    }
    let r = i32(P.radius);
    let x = i32(id.x);
    let y = i32(id.y);
    let i = at(x, y);
    let px = src[i];
    dst[i] = px;
    let gx = i32(wid.x) * 16;
    let gy = i32(wid.y) * 16;
    if !flagged(gx - r, gx + 15 + r, gy, gy + 15 + 2 * r, 2u) {
        return;
    }
    // The windows' weights first, as in `suppress_apply`.
    var weights = 0.0;
    for (var j = 0; j <= 2 * r; j++) {
        var r2 = 0.0;
        for (var k = -r; k <= r; k++) {
            r2 += g2[at(cx(x + k), y + j)].y;
        }
        weights += r2;
    }
    let ia = P.inv_area;
    var weight = min(2.0 * weights * ia, 1.0);
    if weight <= 0.0 {
        return;
    }
    let p = px.xyz;
    if !all_finite(p) || p.x < 0.0 || p.y < 0.0 || p.z < 0.0 {
        return;
    }
    var m0 = vec4<f32>(0.0);
    var m1 = vec4<f32>(0.0);
    var m2 = 0.0;
    for (var j = 0; j <= 2 * r; j++) {
        var r0 = vec4<f32>(0.0);
        var r1 = vec4<f32>(0.0);
        var r2 = 0.0;
        for (var k = -r; k <= r; k++) {
            let w = at(cx(x + k), y + j);
            let a = g0[w];
            let b = g1[w];
            r0 = vec4<f32>(r0.x + a.x, r0.y + a.y, r0.z + a.z, r0.w + a.w);
            r1 = vec4<f32>(r1.x + b.x, r1.y + b.y, r1.z + b.z, r1.w + b.w);
            r2 += g2[w].x;
        }
        m0 = vec4<f32>(m0.x + r0.x, m0.y + r0.y, m0.z + r0.z, m0.w + r0.w);
        m1 = vec4<f32>(m1.x + r1.x, m1.y + r1.y, m1.z + r1.z, m1.w + r1.w);
        m2 += r2;
    }
    // Misregistration keeps every channel between the two colours: how far
    // the pixel may move (`within_the_mixes`, the window centred on it).
    let own = at(x, y + r);
    let mean = g3[own].xyz;
    let axis = g4[own].xyz;
    var low = 3.40282347e38;
    var high = -3.40282347e38;
    for (var j = -r; j <= r; j++) {
        let yy = cy(y + j);
        for (var k = -r; k <= r; k++) {
            let t = along(axis, mean, src[at(cx(x + k), yy)].xyz);
            low = min(low, t);
            high = max(high, t);
        }
    }
    let extent = high - low;
    var share = 1.0;
    if !(axis.x == 0.0 && axis.y == 0.0 && axis.z == 0.0) && extent > 0.0 {
        var beyond = 0.0;
        for (var c = 0u; c < 3u; c++) {
            let a = mean[c] + low * axis[c];
            let b = mean[c] + high * axis[c];
            beyond = max(beyond, max(max(min(a, b) - p[c], p[c] - max(a, b)), 0.0));
        }
        beyond = beyond / extent;
        share = clamp((MIX_NONE - beyond) / (MIX_NONE - MIX_FULL), 0.0, 1.0);
    }
    weight *= share;
    if weight <= 0.0 {
        return;
    }
    // Slope matrix entries (`PAIRS` order): m0.w, m1.xyzw, m2.x.
    let s00 = m0.w;
    let s01 = m1.x;
    let s02 = m1.y;
    let s11 = m1.z;
    let s12 = m1.w;
    let s22 = m2;
    let projected = vec3<f32>(
        (m0.x + s00 * p.x + s01 * p.y + s02 * p.z) * ia,
        (m0.y + s01 * p.x + s11 * p.y + s12 * p.z) * ia,
        (m0.z + s02 * p.x + s12 * p.y + s22 * p.z) * ia,
    );
    dst[i] = vec4<f32>(
        max(p.x + weight * (projected.x - p.x), 0.0),
        max(p.y + weight * (projected.y - p.y), 0.0),
        max(p.z + weight * (projected.z - p.z), 0.0),
        px.w,
    );
}
