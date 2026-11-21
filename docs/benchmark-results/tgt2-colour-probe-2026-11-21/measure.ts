// TGT2 measurement: the colour editor's target solve fed with the colour
// entering the colour editor (unclipped working colour) and the mask
// influence at the pick, as the renderer's probe returns them
// (`sample_color_range_input`). The probe is stood in for by exports of
// the CLI: the colour by a ProPhoto 16-bit export of the recipe without
// ranges (the working gamut is the ProPhoto cube and the ProPhoto output
// keeps it unchanged), the influence by the ratio of a layer range's change
// to the same range's change on the whole photo. Solve with the app's
// solver → export → sample the same block → ΔE00 (CIELAB D50) to the target.
//
// RAWTONIC_REPO: the checkout; TGT2_SCRATCH: folder with src/ (the RAW
// copies); RAWTONIC_CLI: the CLI binary. Each run writes only into a new
// folder runs/<time>/ and deletes only the renders it made there.
import { execFileSync } from 'node:child_process';
import { mkdirSync, unlinkSync, writeFileSync } from 'node:fs';
import type { RangeInput, Vec3 } from '../../../src/utils/colorTarget';

const REPO = process.env.RAWTONIC_REPO!;
const S = process.env.TGT2_SCRATCH!;
const CLI = process.env.RAWTONIC_CLI!;
const {
  deltaE00,
  hexOf,
  insideSrgb,
  labToLch,
  labToLinear,
  lchToLab,
  linearToLab,
  linearToOklab,
  linearToSrgb8,
  meanOklab,
  oklabToLinear,
  parseHex,
  prophotoToLinear,
  renderedLab,
  solveColorTarget,
  srgb8ToLinear
} = await import(REPO + '/src/utils/colorTarget.ts');

const picks: { id: string; file: string; kind: string; x: number; y: number; hex?: string }[] = [
  { id: 'skin-face', file: 'c0108.cr3', kind: 'skin', x: 0.548, y: 0.39, hex: '#E3A98C' },
  { id: 'red-sash', file: 'c0108.cr3', kind: 'product', x: 0.5, y: 0.79, hex: '#C8102E' },
  { id: 'red-can', file: 'c0398.nef', kind: 'product', x: 0.393, y: 0.79, hex: '#E4002B' },
  { id: 'terracotta', file: 'c0398.nef', kind: 'product', x: 0.7, y: 0.815, hex: '#C0603A' },
  { id: 'orange-robot', file: 'c1182.arw', kind: 'product', x: 0.7345, y: 0.29, hex: '#FF6A13' },
  { id: 'blue-robot', file: 'c1182.arw', kind: 'product', x: 0.147, y: 0.47, hex: '#0057B8' },
  { id: 'teal-bead', file: 'c0402.nef', kind: 'product', x: 0.693, y: 0.52, hex: '#007C80' },
  { id: 'sky-hills', file: 'c0428.nef', kind: 'sky', x: 0.48, y: 0.115, hex: '#6CA0DC' },
  { id: 'sky-trees', file: 'c0519.nef', kind: 'sky', x: 0.383, y: 0.14, hex: '#87CEEB' },
  { id: 'sky-ferry', file: 'c0320.raf', kind: 'sky', x: 0.743, y: 0.14, hex: '#3D85C6' },
  { id: 'sky-city', file: 'c0326.rwl', kind: 'sky', x: 0.147, y: 0.14, hex: '#4A90D9' },
  { id: 'sky-venice', file: 'c1015.rw2', kind: 'sky', x: 0.8, y: 0.065, hex: '#A7C7E7' }
];
// Relative targets in CIELAB LCh from the pick's colour (as TGT1).
const offsets: { id: string; dL: number; kC: number; dh: number }[] = [
  { id: 'h+12 C×1.15', dL: 0, kC: 1.15, dh: 12 },
  { id: 'h−20 C×0.8 L−6', dL: -6, kC: 0.8, dh: -20 },
  { id: 'h+5 C×1.3 L+5', dL: 5, kC: 1.3, dh: 5 },
  { id: 'h+80 (out of reach)', dL: 0, kC: 1, dh: 80 }
];
// Feathered layers: a linear gradient through the pick, its 50 % line
// `offset` × range above it (influence about 0.5 and 0.25 at the pick).
const feathers: { pick: string; offset: number }[] = [
  { pick: 'red-sash', offset: 0 },
  { pick: 'terracotta', offset: 0.5 },
  { pick: 'blue-robot', offset: 0 },
  { pick: 'sky-hills', offset: 0.5 },
  { pick: 'skin-face', offset: 0 }
];

const CELLS = 11; // the probe's block: 11 × 11 preview pixels
function size(tif: string): [number, number] {
  const out = execFileSync('magick', ['identify', '-format', '%w %h', tif]).toString().trim().split(' ');
  return [Number(out[0]), Number(out[1])];
}
/** The block at a pick: max(11, width/200) px, averaged into 11 × 11 cells (16-bit codes / 65535). */
function cells(tif: string, x: number, y: number): Vec3[] {
  const [w, h] = size(tif);
  const b = Math.max(CELLS, Math.round(w / 200));
  const cx = Math.round(x * w - b / 2);
  const cy = Math.round(y * h - b / 2);
  const txt = execFileSync('magick', [
    tif,
    '-crop',
    `${b}x${b}+${cx}+${cy}`,
    '+repage',
    '-scale',
    `${CELLS}x${CELLS}!`,
    '-depth',
    '16',
    'txt:-'
  ]).toString();
  const out: Vec3[] = [];
  for (const m of txt.matchAll(/\((\d+(?:\.\d+)?),(\d+(?:\.\d+)?),(\d+(?:\.\d+)?)/g))
    out.push([Number(m[1]), Number(m[2]), Number(m[3])].map((v) => v / 65535) as Vec3);
  if (out.length !== CELLS * CELLS) throw new Error(`${out.length} cells: ${tif}`);
  return out;
}
/** Oklab of ProPhoto 16-bit cells (gamma 461/256). */
const prophotoOklab = (c: Vec3[]) =>
  c.map((p) => linearToOklab(prophotoToLinear(p.map((v) => v ** (461 / 256)) as Vec3)));
/** Oklab of sRGB 16-bit cells. */
const srgbOklab = (c: Vec3[]) => c.map((p) => linearToOklab(srgb8ToLinear(p.map((v) => v * 255) as Vec3)));
const labOf = (oklab: Vec3[]) => linearToLab(oklabToLinear(meanOklab(oklab)));

const RUN = `${S}/runs/${new Date().toISOString().replace(/[:.]/g, '-')}`;
mkdirSync(`${S}/runs`, { recursive: true });
mkdirSync(RUN);
let serial = 0;
/** Renders into a new file of this run (the CLI never overwrites), samples it, deletes it. */
function render(src: string, recipe: object, space: 'srgb' | 'prophoto', x: number, y: number): Vec3[] {
  serial += 1;
  const json = `${RUN}/recipe-${serial}.json`;
  const out = `${RUN}/render-${serial}.tif`;
  writeFileSync(json, JSON.stringify(recipe), { flag: 'wx' });
  execFileSync(
    CLI,
    [
      'export',
      src,
      '--output',
      out,
      '--adjustments',
      json,
      '--color-space',
      space,
      '--lensfun-db',
      REPO + '/src-tauri/lensfun_db'
    ],
    { stdio: 'ignore' }
  );
  const c = cells(out, x, y);
  unlinkSync(out);
  return space === 'prophoto' ? prophotoOklab(c) : srgbOklab(c);
}

const rangeAt = (ok: Vec3, patch: object = {}) => ({
  id: 'r1',
  hue: ((Math.atan2(ok[2], ok[1]) * 180) / Math.PI + 360) % 360,
  chroma: Math.hypot(ok[1], ok[2]),
  lightness: ok[0],
  hueRange: 25,
  chromaRange: 70,
  lightnessRange: 100,
  smoothness: 50,
  hueShift: 0,
  saturation: 0,
  lightnessShift: 0,
  hueUniformity: 0,
  saturationUniformity: 0,
  lightnessUniformity: 0,
  ...patch
});
const layerOf = (ranges: object[], p: { x: number; y: number }, wh: [number, number], offset: number) => {
  const range = 0.08 * wh[1];
  const y = p.y * wh[1] + offset * range;
  return {
    id: 'L',
    name: 'L',
    visible: true,
    invert: false,
    opacity: 100,
    adjustments: { colorRanges: ranges },
    subMasks: [
      {
        id: 'g',
        type: 'linear',
        visible: true,
        mode: 'additive',
        parameters: { startX: 0, startY: y, endX: wh[0], endY: y, range }
      }
    ]
  };
};

const rows: object[] = [];
const log = (row: object) => {
  rows.push(row);
  console.log(JSON.stringify(row));
};
const baseCache = new Map<string, Vec3[]>();
const inputOf = (p: (typeof picks)[number]) => {
  const key = `${p.file}@${p.x},${p.y}`;
  // With a range that changes nothing: the colour editor clips what it
  // receives to the working gamut (`gamut_unit`) as the probe does, while
  // an export without ranges maps colours beyond it at output instead
  // (the blue robot's pixels lie beyond ProPhoto).
  const identity = { colorRanges: [rangeAt([0.5, 0.1, 0])] };
  if (!baseCache.has(key)) baseCache.set(key, render(`${S}/src/${p.file}`, identity, 'prophoto', p.x, p.y));
  return baseCache.get(key)!;
};
const targetsOf = (srcLab: Vec3, hex?: string) => {
  const [L, C, h] = labToLch(srcLab);
  const t: { id: string; lab: Vec3 }[] = offsets.map((o) => ({
    id: o.id,
    lab: lchToLab([L + o.dL, C * o.kC, h + o.dh])
  }));
  if (hex) t.push({ id: 'hex ' + hex, lab: linearToLab(srgb8ToLinear(parseHex(hex)!)) });
  return t;
};

// 1. Global ranges: the pick's colour from the probe (working colour).
for (const p of picks) {
  const pixels = inputOf(p);
  const input: RangeInput = { pixels, influence: pixels.map(() => 1) };
  const centre = meanOklab(pixels);
  const range = rangeAt(centre);
  const srcLab = labOf(pixels);
  for (const t of targetsOf(srcLab, p.hex)) {
    const solve = solveColorTarget([range as never], 'r1', t.lab, input)!;
    const recipe = { colorRanges: [{ ...range, ...solve.shifts }] };
    const t0 = Date.now();
    const working = labOf(render(`${S}/src/${p.file}`, recipe, 'prophoto', p.x, p.y));
    const ms = Date.now() - t0;
    const shown = labOf(render(`${S}/src/${p.file}`, recipe, 'srgb', p.x, p.y));
    log({
      part: 'global',
      pick: p.id,
      kind: p.kind,
      file: p.file,
      target: t.id,
      source: hexOf(linearToSrgb8(labToLinear(srcLab))),
      sourceInSrgb: insideSrgb(labToLinear(srcLab)),
      targetHex: hexOf(linearToSrgb8(labToLinear(t.lab))),
      targetInSrgb: insideSrgb(labToLinear(t.lab)),
      shifts: solve.shifts,
      limited: solve.limited,
      predictedDE: +solve.deltaE.toFixed(2),
      measuredDE: +deltaE00(t.lab, working).toFixed(2),
      srgbDE: +deltaE00(t.lab, shown).toFixed(2),
      predVsMeasured: +deltaE00(solve.resultLab, working).toFixed(2),
      renderMs: ms
    });
  }
}

// 2. Feathered mask layers: the solve with the influence at the pick
// against the solve at full strength (TGT1's prediction).
for (const f of feathers) {
  const p = picks.find((q) => q.id === f.pick)!;
  const src = `${S}/src/${p.file}`;
  const pixels = inputOf(p);
  const centre = meanOklab(pixels);
  const range = rangeAt(centre);
  // The photo's pixel size, for the gradient's coordinates (full resolution).
  const probe = `${RUN}/size-${f.pick}.json`;
  writeFileSync(probe, '{}', { flag: 'wx' });
  const sizeTif = `${RUN}/size-${f.pick}.tif`;
  execFileSync(
    CLI,
    ['export', src, '--output', sizeTif, '--adjustments', probe, '--lensfun-db', REPO + '/src-tauri/lensfun_db'],
    {
      stdio: 'ignore'
    }
  );
  const wh = size(sizeTif);
  unlinkSync(sizeTif);
  // Influence per cell: a layer range's change over the same range's change on the photo.
  const strong = { ...range, hueShift: 100 };
  const before = pixels.map((c) => oklabToLinear(c));
  const full = render(src, { colorRanges: [strong] }, 'prophoto', p.x, p.y).map((c) => oklabToLinear(c));
  const layered = render(src, { masks: [layerOf([strong], p, wh, f.offset)] }, 'prophoto', p.x, p.y).map((c) =>
    oklabToLinear(c)
  );
  const enc = (v: number) =>
    Math.sign(v) * (Math.abs(v) <= 0.0031308 ? 12.92 * Math.abs(v) : 1.055 * Math.abs(v) ** (1 / 2.4) - 0.055);
  const influence = before.map((b, i) => {
    let k = 0;
    for (let j = 1; j < 3; j += 1)
      if (Math.abs(enc(full[i][j]) - enc(b[j])) > Math.abs(enc(full[i][k]) - enc(b[k]))) k = j;
    const span = enc(full[i][k]) - enc(b[k]);
    return Math.abs(span) < 1e-3 ? 0 : Math.min(1, Math.max(0, (enc(layered[i][k]) - enc(b[k])) / span));
  });
  const meanInfluence = influence.reduce((a, b) => a + b, 0) / influence.length;
  const input: RangeInput = { pixels, influence };
  const srcLab = labOf(pixels);
  for (const t of targetsOf(srcLab, p.hex).filter((t) => !t.id.includes('out of reach'))) {
    for (const mode of ['influence', 'full strength'] as const) {
      const solve = solveColorTarget([range as never], 'r1', t.lab, mode === 'influence' ? input : null)!;
      const recipe = { masks: [layerOf([{ ...range, ...solve.shifts }], p, wh, f.offset)] };
      const working = labOf(render(src, recipe, 'prophoto', p.x, p.y));
      log({
        part: 'layer',
        mode,
        pick: p.id,
        kind: p.kind,
        target: t.id,
        influence: +meanInfluence.toFixed(3),
        targetInSrgb: insideSrgb(labToLinear(t.lab)),
        shifts: solve.shifts,
        limited: solve.limited,
        predictedDE: +solve.deltaE.toFixed(2),
        measuredDE: +deltaE00(t.lab, working).toFixed(2),
        predVsMeasured: +deltaE00(
          mode === 'influence' ? solve.resultLab : renderedLab([{ ...range, ...solve.shifts } as never], input),
          working
        ).toFixed(2)
      });
    }
  }
}
writeFileSync(`${RUN}/results.json`, JSON.stringify(rows, null, 1), { flag: 'wx' });
console.error('results: ' + RUN + '/results.json');
