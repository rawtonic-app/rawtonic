// @ts-expect-error node:fs is untyped in the frontend tsconfig
import { readFileSync } from 'node:fs';
import { expect, it } from 'vitest';
import { srgbToOklch } from './oklab';
import {
  VIEWER_BACKGROUNDS,
  VIEWER_BACKGROUND_LEVELS,
  viewerBackgroundCss,
  viewerBackgroundRgba,
  type ViewerBackground
} from './viewerDisplay';

// The interface shades' canvas colour (`--app-bg-primary`), read from the same
// stylesheet the test of the tokens reads.
const css = readFileSync('src/styles.css', 'utf8');
function canvasCode(selector: string): number {
  const body = css.slice(css.indexOf(selector) + selector.length).split('}')[0];
  const value = body.match(/--app-bg-primary:\s*#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})/i);
  if (!value) throw new Error(`no canvas colour for ${selector}`);
  const [r, g, b] = [value[1], value[2], value[3]].map((h) => parseInt(h, 16));
  expect([g, b]).toEqual([r, r]);
  return r;
}
const CANVAS = {
  darkest: canvasCode(":root[data-interface-shade='darkest'] {"),
  'dark-grey': canvasCode(":root[data-interface-shade='dark-grey'] {"),
  'medium-grey': canvasCode(":root[data-interface-shade='medium-grey'] {")
};
const FIXED = VIEWER_BACKGROUNDS.filter((background) => background !== 'default');

/** OKLab lightness of a 0–255 sRGB grey (oklab.ts holds the colour math). */
const oklabLightness = (level: number) => srgbToOklch([level, level, level]).lightness;

const nativeLevel = (background: ViewerBackground): number => {
  const [r, g, b, a] = viewerBackgroundRgba(background);
  expect(r).toBe(g);
  expect(g).toBe(b);
  expect(a).toBe(1);
  return r * 255;
};

it('gives the seven backgrounds a colour of their own in every interface shade', () => {
  expect(VIEWER_BACKGROUNDS).toHaveLength(7);
  // Each fixed entry has its own code, not only its own rounded rendering.
  expect(new Set(FIXED.map((b) => VIEWER_BACKGROUND_LEVELS[b])).size).toBe(FIXED.length);
  for (const [shade, canvas] of Object.entries(CANVAS)) {
    const levels = FIXED.map((b) => nativeLevel(b));
    expect(new Set(levels).size, shade).toBe(FIXED.length);
    // Default is exactly the canvas (one colour, not a lookalike) and differs
    // from each fixed entry by at least one code.
    const root = document.documentElement;
    root.style.setProperty('--app-bg-primary', `rgb(${canvas}, ${canvas}, ${canvas})`);
    try {
      expect(nativeLevel('default'), shade).toBeCloseTo(canvas);
      for (const level of levels) expect(Math.abs(level - canvas), shade).toBeGreaterThanOrEqual(1);
      expect(viewerBackgroundRgba('default'), shade).toEqual([canvas / 255, canvas / 255, canvas / 255, 1]);
    } finally {
      root.style.removeProperty('--app-bg-primary');
    }
  }
});

it('keeps the closest pair, Dark Gray and the Medium Grey canvas, apart in lightness', () => {
  // User check (W1 DIM1, Linux): Medium Grey's viewer against Dark Gray (80)
  // side by side in the same viewer; VISCALM1 moved that viewer to 90.
  // Neither this nor the pixel read-out below is a desktop app run.
  expect(Math.abs(oklabLightness(80) - oklabLightness(CANVAS['medium-grey']))).toBeGreaterThan(0.01);
  // Every fixed step keeps at least this lightness step from the nearest
  // canvas, so Dark Gray never becomes a second Default.
  for (const canvas of Object.values(CANVAS)) {
    const nearest = Math.min(
      ...FIXED.map((b) => Math.abs(oklabLightness(VIEWER_BACKGROUND_LEVELS[b]) - oklabLightness(canvas)))
    );
    expect(nearest).toBeGreaterThan(0.01);
  }
});

it('lists the fixed backgrounds from lightest to darkest', () => {
  const levels = FIXED.map((background) => VIEWER_BACKGROUND_LEVELS[background]);
  expect(levels).toEqual([...levels].sort((a, b) => b - a));
  // Dark is a step of its own between Medium and Darker.
  expect(VIEWER_BACKGROUND_LEVELS.dark).toBeGreaterThan(VIEWER_BACKGROUND_LEVELS.veryDark);
  expect(VIEWER_BACKGROUND_LEVELS.dark).toBeLessThan(VIEWER_BACKGROUND_LEVELS.medium);
});

it('draws Default as the canvas token, which the stylesheet fills in', () => {
  expect(viewerBackgroundCss('default')).toBe(
    `var(--app-bg-primary, rgb(${CANVAS.darkest}, ${CANVAS.darkest}, ${CANVAS.darkest}))`
  );
  // The native display cannot read CSS variables: it reads the live token.
  const root = document.documentElement;
  root.style.setProperty('--app-bg-primary', 'rgb(76, 76, 76)');
  try {
    expect(nativeLevel('default')).toBeCloseTo(76);
  } finally {
    root.style.removeProperty('--app-bg-primary');
  }
});

it("reads the built stylesheet's hex canvas token like the source rgb() one", () => {
  // The production build writes `--app-bg-primary:#181818` etc. (no spaces).
  const root = document.documentElement;
  for (const [hex, code] of [
    ['#181818', 24],
    ['#262626', 38],
    ['#4c4c4c', 76]
  ] as const) {
    root.style.setProperty('--app-bg-primary', hex);
    try {
      expect(nativeLevel('default'), hex).toBeCloseTo(code);
      for (const level of FIXED.map((b) => nativeLevel(b)))
        expect(Math.abs(level - code), hex).toBeGreaterThanOrEqual(1);
    } finally {
      root.style.removeProperty('--app-bg-primary');
    }
  }
});

it('falls back like the CSS path, never to black, when no canvas token answers', () => {
  const root = document.documentElement;
  for (const value of ['', 'currentcolor', 'var(--nope)', 'rgb(1, 2)']) {
    root.style.setProperty('--app-bg-primary', value);
    try {
      expect(nativeLevel('default'), value).toBeCloseTo(CANVAS.darkest);
    } finally {
      root.style.removeProperty('--app-bg-primary');
    }
  }
  expect(nativeLevel('default')).toBeCloseTo(CANVAS.darkest);
});

it('dims a fixed background to the share of its grey the dimmer leaves', () => {
  expect(viewerBackgroundCss('dark', 0.2)).toBe('rgb(16, 16, 16)');
  expect(viewerBackgroundCss('default', 0.2)).toBe(
    `color-mix(in srgb, var(--app-bg-primary, rgb(${CANVAS.darkest}, ${CANVAS.darkest}, ${CANVAS.darkest})) 20%, black)`
  );
  const [r] = viewerBackgroundRgba('dark', 0.2);
  expect(r).toBeCloseTo((80 / 255) * 0.2);
});
