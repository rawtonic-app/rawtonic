// SCRATCH ONLY (task GD, G43 ceiling): the CPU RAW develop stages before the
// GPU main pass, as wgpu compute on the uploaded mosaic. Bayer only.
// demosaic passes d1-d4 are a direct port of demosaic.rs `bayer_tile`
// (our own algorithm, same expressions and order); hot pixels port
// hot_pixels.rs `suppress`; shadow stats port black_bias.rs
// `ShadowMeans::measure` (f32 sums instead of f64); the highlight passes
// are a cost proxy only (not the algorithm).

struct P {
    w: u32, h: u32, raw_w: u32, rx: u32,
    ry: u32, bw: u32, bh: u32, hl_bins: u32,
    cfa: vec4<u32>,
    black: vec4<f32>,
    scale: vec4<f32>,
    wb: vec4<f32>,
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    misc: vec4<f32>, // x: rescale, y: highlight threshold
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> raw: array<u32>;
@group(0) @binding(2) var<storage, read_write> lev: array<f32>;
@group(0) @binding(3) var<storage, read_write> enc: array<f32>;
@group(0) @binding(4) var<storage, read_write> dir: array<vec2<f32>>;
@group(0) @binding(5) var<storage, read_write> pr: array<f32>;
@group(0) @binding(6) var<storage, read_write> pg: array<f32>;
@group(0) @binding(7) var<storage, read_write> pb: array<f32>;
@group(0) @binding(8) var<storage, read_write> stats: array<f32>;
@group(0) @binding(9) var<storage, read_write> outp: array<vec2<u32>>;
@group(0) @binding(10) var<storage, read_write> counters: array<atomic<u32>>;
@group(0) @binding(11) var<storage, read> gains: array<vec4<f32>>;

const EPS: f32 = 1e-5;
const EPS_SQ: f32 = 1e-10;

fn colour(x: i32, y: i32) -> u32 {
    return p.cfa[u32(y & 1) * 2u + u32(x & 1)];
}

// Nearest in-image coordinate of the same CFA parity.
fn cx(x: i32) -> i32 {
    let w = i32(p.w);
    if x < 0 { return x & 1; }
    if x >= w { return w - 2 + ((x - w) & 1); }
    return x;
}
fn cy(y: i32) -> i32 {
    let h = i32(p.h);
    if y < 0 { return y & 1; }
    if y >= h { return h - 2 + ((y - h) & 1); }
    return y;
}
fn idx(x: i32, y: i32) -> u32 { return u32(cy(y)) * p.w + u32(cx(x)); }
fn e(x: i32, y: i32) -> f32 { return enc[idx(x, y)]; }

// ---- 1. unpack u16 + black subtract + scale (sensor black 0, white 1) ----
@compute @workgroup_size(16, 16)
fn unpack(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let ax = p.rx + id.x;
    let ay = p.ry + id.y;
    let k = ay * p.raw_w + ax;
    let v = f32((raw[k >> 1u] >> ((k & 1u) * 16u)) & 0xffffu);
    let pos = (ay & 1u) * 2u + (ax & 1u);
    pr[id.y * p.w + id.x] = (v - p.black[pos]) * p.scale[pos]; // unfixed level
}

// ---- 2. hot/dead pixels (hot_pixels::suppress, DeadTest::Adjacent) -> wb mosaic, sqrt-encoded ----
@compute @workgroup_size(16, 16)
fn hot(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * p.w + id.x;
    let c = colour(x, y);
    var v = pr[i];
    if x >= 2 && y >= 2 && x < i32(p.w) - 2 && y < i32(p.h) - 2 {
        var vals: array<f32, 24>;
        var n = 0u;
        var lo = 1e30;
        var hi = -1e30;
        var adj_hi = 0.0;
        var adj_lo = 1e30;
        for (var dy = -2; dy <= 2; dy++) {
            for (var dx = -2; dx <= 2; dx++) {
                if dx == 0 && dy == 0 { continue; }
                let o = colour(x + dx, y + dy);
                let s = pr[u32(y + dy) * p.w + u32(x + dx)];
                if o == c {
                    vals[n] = s;
                    n++;
                    lo = min(lo, s);
                    hi = max(hi, s);
                } else if abs(dx) <= 1 && abs(dy) <= 1 {
                    let a = s * p.wb[o] / p.wb[c];
                    adj_hi = max(adj_hi, a);
                    adj_lo = min(adj_lo, a);
                }
            }
        }
        let is_hot = v > 2.0 * max(hi, 0.0) + 0.01 && v > 1.6 * adj_hi + 0.01;
        let is_dead = !is_hot && lo > 0.02 && v < 0.4 * lo - 0.005 && v < 0.6 * adj_lo - 0.005;
        if is_hot || is_dead {
            // insertion sort, median
            for (var a = 1u; a < n; a++) {
                let t = vals[a];
                var b = a;
                while b > 0u && vals[b - 1u] > t {
                    vals[b] = vals[b - 1u];
                    b--;
                }
                vals[b] = t;
            }
            v = 0.5 * (vals[(n - 1u) / 2u] + vals[n / 2u]);
            atomicAdd(&counters[0], 1u);
        }
    }
    // mosaic in sensor-white units, white balanced (develop_rgb_mosaic)
    let m = v * p.wb[c];
    enc[i] = sqrt(max(m, 0.0));
    lev[i] = v; // fixed level (unfixed input is in pr), for the CPU comparison and the shadow stats
}

// ---- 3. black bias: per 12 x 12 block and colour [n, sum, pairs, sqdiff] ----
@compute @workgroup_size(64)
fn shadow_stats(@builtin(global_invocation_id) id: vec3<u32>) {
    let nb = p.bw * p.bh;
    if id.x >= nb { return; }
    let bx = id.x % p.bw;
    let by = id.x / p.bw;
    var s: array<vec4<f32>, 3>;
    for (var r = by * 12u; r < min((by + 1u) * 12u, p.h); r++) {
        for (var c = bx * 12u; c < min((bx + 1u) * 12u, p.w); c++) {
            let v = lev[r * p.w + c];
            let k = colour(i32(c), i32(r));
            s[k].x += 1.0;
            s[k].y += v;
            if c >= 2u {
                let d = v - lev[r * p.w + c - 2u];
                s[k].z += 1.0;
                s[k].w += d * d;
            }
        }
    }
    for (var k = 0u; k < 3u; k++) {
        let o = (id.x * 3u + k) * 4u;
        stats[o] = s[k].x; stats[o + 1u] = s[k].y; stats[o + 2u] = s[k].z; stats[o + 3u] = s[k].w;
    }
}

// ---- demosaic d1: direction weights vh, pq (high-passes recomputed inline) ----
fn hpv(x: i32, y: i32) -> f32 {
    let v = e(x, y - 3) - e(x, y - 1) - e(x, y + 1) + e(x, y + 3)
        - 3.0 * (e(x, y - 2) + e(x, y + 2)) + 6.0 * e(x, y);
    return v * v;
}
fn hph(x: i32, y: i32) -> f32 {
    let v = e(x - 3, y) - e(x - 1, y) - e(x + 1, y) + e(x + 3, y)
        - 3.0 * (e(x - 2, y) + e(x + 2, y)) + 6.0 * e(x, y);
    return v * v;
}
fn hpp(x: i32, y: i32) -> f32 {
    let v = e(x - 3, y - 3) - e(x - 1, y - 1) - e(x + 1, y + 1) + e(x + 3, y + 3)
        - 3.0 * (e(x - 2, y - 2) + e(x + 2, y + 2)) + 6.0 * e(x, y);
    return v * v;
}
fn hpq(x: i32, y: i32) -> f32 {
    let v = e(x + 3, y - 3) - e(x + 1, y - 1) - e(x - 1, y + 1) + e(x - 3, y + 3)
        - 3.0 * (e(x + 2, y - 2) + e(x - 2, y + 2)) + 6.0 * e(x, y);
    return v * v;
}

@compute @workgroup_size(16, 16)
fn d1(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let x = i32(id.x);
    let y = i32(id.y);
    let v = EPS_SQ + hpv(x, y - 1) + hpv(x, y) + hpv(x, y + 1);
    let hz = EPS_SQ + hph(x - 1, y) + hph(x, y) + hph(x + 1, y);
    let pp = EPS_SQ + hpp(x - 1, y - 1) + hpp(x, y) + hpp(x + 1, y + 1);
    let qq = EPS_SQ + hpq(x + 1, y - 1) + hpq(x, y) + hpq(x - 1, y + 1);
    dir[id.y * p.w + id.x] = vec2<f32>(v / (v + hz), pp / (pp + qq));
}

fn dvh(x: i32, y: i32) -> f32 { return dir[idx(x, y)].x; }
fn dpq(x: i32, y: i32) -> f32 { return dir[idx(x, y)].y; }

// ---- d2: green at red and blue sites ----
@compute @workgroup_size(16, 16)
fn d2(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * p.w + id.x;
    let c = colour(x, y);
    let ci = enc[i];
    if c == 1u {
        pg[i] = ci;
        return;
    }
    if c == 0u { pr[i] = ci; } else { pb[i] = ci; }
    let n_grad = EPS + abs(e(x, y - 1) - e(x, y + 1)) + abs(ci - e(x, y - 2))
        + abs(e(x, y - 1) - e(x, y - 3)) + abs(e(x, y - 2) - e(x, y - 4));
    let s_grad = EPS + abs(e(x, y + 1) - e(x, y - 1)) + abs(ci - e(x, y + 2))
        + abs(e(x, y + 1) - e(x, y + 3)) + abs(e(x, y + 2) - e(x, y + 4));
    let w_grad = EPS + abs(e(x - 1, y) - e(x + 1, y)) + abs(ci - e(x - 2, y))
        + abs(e(x - 1, y) - e(x - 3, y)) + abs(e(x - 2, y) - e(x - 4, y));
    let e_grad = EPS + abs(e(x + 1, y) - e(x - 1, y)) + abs(ci - e(x + 2, y))
        + abs(e(x + 1, y) - e(x + 3, y)) + abs(e(x + 2, y) - e(x + 4, y));
    let n_est = e(x, y - 1) + 0.5 * (ci - e(x, y - 2));
    let s_est = e(x, y + 1) + 0.5 * (ci - e(x, y + 2));
    let w_est = e(x - 1, y) + 0.5 * (ci - e(x - 2, y));
    let e_est = e(x + 1, y) + 0.5 * (ci - e(x + 2, y));
    let v_est = (s_grad * n_est + n_grad * s_est) / (n_grad + s_grad);
    let h_est = (w_grad * e_est + e_grad * w_est) / (e_grad + w_grad);
    let central = dvh(x, y);
    let around = 0.25 * (dvh(x - 1, y - 1) + dvh(x + 1, y - 1) + dvh(x - 1, y + 1) + dvh(x + 1, y + 1));
    var disc = central;
    if abs(0.5 - central) < abs(0.5 - around) { disc = around; }
    pg[i] = max(h_est * disc + v_est * (1.0 - disc), 0.0);
}

fn g(x: i32, y: i32) -> f32 { return pg[idx(x, y)]; }
fn vx(k: u32, x: i32, y: i32) -> f32 {
    if k == 0u { return pr[idx(x, y)]; }
    return pb[idx(x, y)];
}

// ---- d3: red at blue sites and blue at red sites, along the diagonals ----
@compute @workgroup_size(16, 16)
fn d3(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * p.w + id.x;
    let own = colour(x, y);
    if own == 1u { return; }
    let k = 2u - own;
    let gi = pg[i];
    let nw = vx(k, x - 1, y - 1);
    let ne = vx(k, x + 1, y - 1);
    let sw = vx(k, x - 1, y + 1);
    let se = vx(k, x + 1, y + 1);
    let nw_grad = EPS + abs(nw - se) + abs(nw - vx(k, x - 3, y - 3)) + abs(gi - g(x - 2, y - 2));
    let ne_grad = EPS + abs(ne - sw) + abs(ne - vx(k, x + 3, y - 3)) + abs(gi - g(x + 2, y - 2));
    let sw_grad = EPS + abs(sw - ne) + abs(sw - vx(k, x - 3, y + 3)) + abs(gi - g(x - 2, y + 2));
    let se_grad = EPS + abs(se - nw) + abs(se - vx(k, x + 3, y + 3)) + abs(gi - g(x + 2, y + 2));
    let p_est = (nw_grad * (se - g(x + 1, y + 1)) + se_grad * (nw - g(x - 1, y - 1))) / (nw_grad + se_grad);
    let q_est = (ne_grad * (sw - g(x - 1, y + 1)) + sw_grad * (ne - g(x + 1, y - 1))) / (ne_grad + sw_grad);
    let central = dpq(x, y);
    let around = 0.25 * (dpq(x - 1, y - 1) + dpq(x + 1, y - 1) + dpq(x - 1, y + 1) + dpq(x + 1, y + 1));
    var disc = central;
    if abs(0.5 - central) < abs(0.5 - around) { disc = around; }
    let val = max(gi + (1.0 - disc) * p_est + disc * q_est, 0.0);
    if k == 0u { pr[i] = val; } else { pb[i] = val; }
}

// ---- d4: red and blue at green sites, along the axes ----
@compute @workgroup_size(16, 16)
fn d4(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * p.w + id.x;
    if colour(x, y) != 1u { return; }
    let central = dvh(x, y);
    let around = 0.25 * (dvh(x - 1, y - 1) + dvh(x + 1, y - 1) + dvh(x - 1, y + 1) + dvh(x + 1, y + 1));
    var disc = central;
    if abs(0.5 - central) < abs(0.5 - around) { disc = around; }
    let gi = pg[i];
    for (var k = 0u; k <= 2u; k += 2u) {
        let n_grad = EPS + abs(gi - g(x, y - 2)) + abs(vx(k, x, y - 1) - vx(k, x, y + 1)) + abs(vx(k, x, y - 1) - vx(k, x, y - 3));
        let s_grad = EPS + abs(gi - g(x, y + 2)) + abs(vx(k, x, y + 1) - vx(k, x, y - 1)) + abs(vx(k, x, y + 1) - vx(k, x, y + 3));
        let w_grad = EPS + abs(gi - g(x - 2, y)) + abs(vx(k, x - 1, y) - vx(k, x + 1, y)) + abs(vx(k, x - 1, y) - vx(k, x - 3, y));
        let e_grad = EPS + abs(gi - g(x + 2, y)) + abs(vx(k, x + 1, y) - vx(k, x - 1, y)) + abs(vx(k, x + 1, y) - vx(k, x + 3, y));
        let n_est = vx(k, x, y - 1) - g(x, y - 1);
        let s_est = vx(k, x, y + 1) - g(x, y + 1);
        let w_est = vx(k, x - 1, y) - g(x - 1, y);
        let e_est = vx(k, x + 1, y) - g(x + 1, y);
        let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
        let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);
        let val = max(gi + (1.0 - disc) * v_est + disc * h_est, 0.0);
        if k == 0u { pr[i] = val; } else { pb[i] = val; }
    }
}

// ---- d5: decode (square) in place, so the planes are linear (comparison point) ----
@compute @workgroup_size(16, 16)
fn d5(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let i = id.y * p.w + id.x;
    let r = max(pr[i], 0.0);
    let gg = max(pg[i], 0.0);
    let b = max(pb[i], 0.0);
    pr[i] = r * r;
    pg[i] = gg * gg;
    pb[i] = b * b;
}

// ---- black bias restore: block sums of the demosaiced channels ----
@compute @workgroup_size(64)
fn restore_sums(@builtin(global_invocation_id) id: vec3<u32>) {
    let nb = p.bw * p.bh;
    if id.x >= nb { return; }
    let bx = id.x % p.bw;
    let by = id.x / p.bw;
    var s = vec3<f32>(0.0);
    for (var r = by * 12u; r < min((by + 1u) * 12u, p.h); r++) {
        for (var c = bx * 12u; c < min((bx + 1u) * 12u, p.w); c++) {
            let i = r * p.w + c;
            s += vec3<f32>(pr[i], pg[i], pb[i]);
        }
    }
    let o = nb * 12u + id.x * 3u;
    stats[o] = s.x; stats[o + 1u] = s.y; stats[o + 2u] = s.z;
}

// ---- apply: bilinear block gains, camera -> linear sRGB, rescale, rgba16f out ----
fn gain_at(bx: i32, by: i32) -> vec4<f32> {
    let x = u32(clamp(bx, 0, i32(p.bw) - 1));
    let y = u32(clamp(by, 0, i32(p.bh) - 1));
    return gains[y * p.bw + x];
}
@compute @workgroup_size(16, 16)
fn apply(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let i = id.y * p.w + id.x;
    let fx = max((f32(id.x) + 0.5) / 12.0 - 0.5, 0.0);
    let fy = max((f32(id.y) + 0.5) / 12.0 - 0.5, 0.0);
    let bx = i32(floor(fx));
    let by = i32(floor(fy));
    let tx = fract(fx);
    let ty = fract(fy);
    let top = mix(gain_at(bx, by), gain_at(bx + 1, by), tx);
    let bottom = mix(gain_at(bx, by + 1), gain_at(bx + 1, by + 1), tx);
    let gn = mix(top, bottom, ty);
    let c = vec3<f32>(pr[i], pg[i], pb[i]) * gn.xyz;
    let o = vec3<f32>(dot(p.m0.xyz, c), dot(p.m1.xyz, c), dot(p.m2.xyz, c));
    // finish(): rescale (unscale * rescale = 1) and the ProPhoto floor stand-in
    let q = clamp(o, vec3<f32>(-1000.0), vec3<f32>(1000.0));
    outp[i] = vec2<u32>(pack2x16float(q.xy), pack2x16float(vec2<f32>(q.z, 1.0)));
}

// ---- highlight proxy 1: clip-level histogram of the top 6 % per colour ----
@compute @workgroup_size(16, 16)
fn hl_hist(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let v = lev[id.y * p.w + id.x];
    if v > 0.94 {
        let b = min(u32((v - 0.94) / 0.06 * f32(p.hl_bins)), p.hl_bins - 1u);
        let c = colour(i32(id.x), i32(id.y));
        atomicAdd(&counters[16u + c * p.hl_bins + b], 1u);
    }
}

// ---- highlight proxy 2: 5 x 5 neighbourhood means of every colour per sample ----
// (the reconstruction reads these for clipped samples; computed here for all
// samples as a worst-case cost; written only where clipped)
@compute @workgroup_size(16, 16)
fn hl_rebuild(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let x = i32(id.x);
    let y = i32(id.y);
    var sum = vec3<f32>(0.0);
    var cnt = vec3<f32>(0.0);
    for (var dy = -2; dy <= 2; dy++) {
        for (var dx = -2; dx <= 2; dx++) {
            let c = colour(x + dx, y + dy);
            let m = enc[idx(x + dx, y + dy)];
            let one = vec3<f32>(f32(c == 0u), f32(c == 1u), f32(c == 2u));
            sum += one * m;
            cnt += one;
        }
    }
    let mean = sum / max(cnt, vec3<f32>(1.0));
    let i = id.y * p.w + id.x;
    let c = colour(x, y);
    if lev[i] > p.misc.y {
        enc[i] = 0.5 * (mean[(c + 1u) % 3u] + mean[(c + 2u) % 3u]) + mean[c] * 1e-6;
    }
}
