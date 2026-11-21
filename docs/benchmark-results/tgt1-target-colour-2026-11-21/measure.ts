// TGT1 measurement: solve target colours with the app's solver, render with
// the CLI, sample the same block, ΔE00 (CIELAB D50) to the target.
import { execFileSync } from 'node:child_process';
import { mkdirSync, unlinkSync, writeFileSync } from 'node:fs';
// RAWTONIC_REPO: the Rawtonic checkout; TGT1_SCRATCH: folder with src/ (the
// RAW copies) and out/<id>_base.tif (default-recipe exports). Each run
// writes only into a new folder runs/<time>/ (recipe, renders while they are
// sampled, results.json) and deletes only the renders it made there.
const REPO = process.env.RAWTONIC_REPO!;
const S = process.env.TGT1_SCRATCH!;
const {
  deltaE00,
  hexOf,
  labToLch,
  labToLinear,
  lchToLab,
  linearToLab,
  linearToOklab,
  linearToSrgb8,
  parseHex,
  solveColorTarget,
  srgb8ToLinear,
  insideSrgb
} = await import(REPO + '/src/utils/colorTarget.ts');

const CLI = process.env.RAWTONIC_CLI ?? S + '/target/debug/rawtonic';
type V = [number, number, number];

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
// Relative targets in CIELAB LCh (fixed before any measurement).
const offsets: { id: string; dL: number; kC: number; dh: number }[] = [
  { id: 'h+12 C×1.15', dL: 0, kC: 1.15, dh: 12 },
  { id: 'h−20 C×0.8 L−6', dL: -6, kC: 0.8, dh: -20 },
  { id: 'h+5 C×1.3 L+5', dL: 5, kC: 1.3, dh: 5 },
  { id: 'h+80 (out of reach)', dL: 0, kC: 1, dh: 80 }
];

function size(tif: string): [number, number] {
  const out = execFileSync('magick', ['identify', '-format', '%w %h', tif]).toString().trim().split(' ');
  return [Number(out[0]), Number(out[1])];
}
function sample(tif: string, x: number, y: number): V {
  const [w, h] = size(tif);
  const b = Math.max(11, Math.round(w / 200));
  const cx = Math.round(x * w - b / 2);
  const cy = Math.round(y * h - b / 2);
  const txt = execFileSync('magick', [
    tif,
    '-crop',
    `${b}x${b}+${cx}+${cy}`,
    '+repage',
    '-scale',
    '1x1!',
    '-depth',
    '16',
    'txt:-'
  ]).toString();
  const m = txt.match(/\((\d+(?:\.\d+)?),(\d+(?:\.\d+)?),(\d+(?:\.\d+)?)/);
  if (!m) throw new Error('no sample: ' + txt);
  return [Number(m[1]), Number(m[2]), Number(m[3])].map((v) => (v / 65535) * 255) as V;
}
const RUN = `${S}/runs/${new Date().toISOString().replace(/[:.]/g, '-')}`;
mkdirSync(`${S}/runs`, { recursive: true });
mkdirSync(RUN);
let serial = 0;
/** Renders into a new file of this run (the CLI never overwrites); returns its path. */
function render(src: string, recipe: object): string {
  serial += 1;
  const json = `${RUN}/recipe-${serial}.json`;
  const out = `${RUN}/render-${serial}.tif`;
  writeFileSync(json, JSON.stringify(recipe), { flag: 'wx' });
  execFileSync(
    CLI,
    ['export', src, '--output', out, '--adjustments', json, '--lensfun-db', REPO + '/src-tauri/lensfun_db'],
    { stdio: 'ignore' }
  );
  return out;
}

const rows: object[] = [];
for (const p of picks) {
  const base = `${S}/out/${p.file.split('.')[0]}_base.tif`;
  const src = sample(base, p.x, p.y);
  const ok = linearToOklab(srgb8ToLinear(src));
  const chroma = Math.hypot(ok[1], ok[2]);
  const range = {
    id: 'r1',
    hue: ((Math.atan2(ok[2], ok[1]) * 180) / Math.PI + 360) % 360,
    chroma,
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
    lightnessUniformity: 0
  };
  const srcLab = linearToLab(srgb8ToLinear(src));
  const [L, C, h] = labToLch(srcLab);
  const targets: { id: string; lab: V }[] = offsets.map((o) => ({
    id: o.id,
    lab: lchToLab([L + o.dL, C * o.kC, h + o.dh])
  }));
  if (p.hex) targets.push({ id: 'hex ' + p.hex, lab: linearToLab(srgb8ToLinear(parseHex(p.hex)!)) });
  for (const t of targets) {
    // Pass 0: the app's solve; passes 1–2: closed loop on the rendered sample
    // (an experiment). The row's fields are pass 0's; `loop` lists every
    // pass's ΔE00.
    let aim = t.lab;
    const loop: number[] = [];
    let first: { solve: any; got: V; gotLab: V; ms: number } | null = null;
    for (let k = 0; k < 3; k++) {
      const solve = solveColorTarget([range as never], 'r1', aim)!;
      const t0 = Date.now();
      const out = render(`${S}/src/${p.file}`, { colorRanges: [{ ...range, ...solve.shifts }] });
      const ms = Date.now() - t0;
      const got = sample(out, p.x, p.y);
      unlinkSync(out);
      const gotLab = linearToLab(srgb8ToLinear(got));
      if (k === 0) first = { solve, got, gotLab, ms };
      loop.push(+deltaE00(t.lab, gotLab).toFixed(2));
      aim = aim.map((v: number, i: number) => v + (t.lab[i] - gotLab[i])) as V;
    }
    const { solve, got, gotLab, ms } = first!;
    const row = {
      pick: p.id,
      kind: p.kind,
      file: p.file,
      target: t.id,
      source: hexOf(src),
      targetHex: hexOf(linearToSrgb8(labToLinear(t.lab))),
      targetInSrgb: insideSrgb(labToLinear(t.lab)),
      shifts: solve.shifts,
      limited: solve.limited,
      predictedDE: +solve.deltaE.toFixed(2),
      measuredDE: +deltaE00(t.lab, gotLab).toFixed(2),
      predVsMeasured: +deltaE00(solve.resultLab, gotLab).toFixed(2),
      sourceToTargetDE: +deltaE00(srcLab, t.lab).toFixed(2),
      got: hexOf(got),
      renderMs: ms,
      loop
    };
    rows.push(row);
    console.log(JSON.stringify(row));
  }
}
writeFileSync(`${RUN}/results.json`, JSON.stringify(rows, null, 1), { flag: 'wx' });
console.error('results: ' + RUN + '/results.json');
