import { expect, it } from 'vitest';
import {
  VIEWER_BACKGROUNDS,
  VIEWER_BACKGROUND_LEVELS,
  viewerBackgroundCss,
  viewerBackgroundRgba,
  type ViewerBackground
} from './viewerDisplay';

/** CIE L* of a 0–255 sRGB grey. */
function lightness(level: number): number {
  const c = level / 255;
  const y = c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  return y > (6 / 29) ** 3 ? 116 * y ** (1 / 3) - 16 : y * (29 / 3) ** 3;
}

const nativeLevel = (background: ViewerBackground): number => {
  const [r, g, b, a] = viewerBackgroundRgba(background);
  expect(r).toBe(g);
  expect(g).toBe(b);
  expect(a).toBe(1);
  return r * 255;
};

it('renders the seven backgrounds as seven distinct greys', () => {
  expect(VIEWER_BACKGROUNDS).toHaveLength(7);
  const levels = VIEWER_BACKGROUNDS.map((background) => Math.round(nativeLevel(background)));
  expect(new Set(levels).size).toBe(VIEWER_BACKGROUNDS.length);
  // Each entry has its own code, not only its own rounded rendering.
  expect(new Set(Object.values(VIEWER_BACKGROUND_LEVELS)).size).toBe(VIEWER_BACKGROUNDS.length);
  // Dark sits between Medium and Darker, about half way in perceptual lightness.
  expect(VIEWER_BACKGROUND_LEVELS.dark).toBeGreaterThan(VIEWER_BACKGROUND_LEVELS.veryDark);
  expect(VIEWER_BACKGROUND_LEVELS.dark).toBeLessThan(VIEWER_BACKGROUND_LEVELS.medium);
  // Half way in perceptual lightness. Lightness is not linear in the code, so
  // this is Dark's 80 against Black's 0, not Medium's and Darker's average (57).
  const between = (lightness(VIEWER_BACKGROUND_LEVELS.medium) + lightness(VIEWER_BACKGROUND_LEVELS.veryDark)) / 2;
  expect(lightness(VIEWER_BACKGROUND_LEVELS.dark)).toBeGreaterThan(between);
  expect(lightness(VIEWER_BACKGROUND_LEVELS.dark)).toBeGreaterThan(30);
  expect(lightness(VIEWER_BACKGROUND_LEVELS.dark)).toBeLessThan(40);
});

it('lists the fixed backgrounds from lightest to darkest', () => {
  const levels = VIEWER_BACKGROUNDS.filter((b) => b !== 'default').map((b) => VIEWER_BACKGROUND_LEVELS[b]);
  expect(levels).toEqual([...levels].sort((a, b) => b - a));
});

it('draws Default in the app canvas colour, so it follows the interface shade', () => {
  expect(viewerBackgroundCss('default')).toBe('var(--app-bg-primary, rgb(38, 38, 38))');
  // The native display cannot read CSS variables: it reads the live token.
  const shade = (value: string) => {
    document.documentElement.style.setProperty('--app-bg-primary', value);
    return value;
  };
  try {
    shade('rgb(38, 38, 38)');
    expect(nativeLevel('default')).toBeCloseTo(38);
    shade('rgb(24, 24, 24)');
    expect(nativeLevel('default')).toBeCloseTo(24);
    shade('rgb(76, 76, 76)');
    expect(nativeLevel('default')).toBeCloseTo(76);
    // Still distinct from every fixed entry in the shades whose canvas is dark.
    for (const value of ['rgb(38, 38, 38)', 'rgb(24, 24, 24)']) {
      shade(value);
      const others = VIEWER_BACKGROUNDS.filter((b) => b !== 'default').map(nativeLevel);
      expect(others.every((level) => Math.abs(level - nativeLevel('default')) >= 1)).toBe(true);
    }
  } finally {
    document.documentElement.style.removeProperty('--app-bg-primary');
  }
});

it('falls back to the Darkest canvas when no stylesheet is loaded', () => {
  expect(nativeLevel('default')).toBeCloseTo(VIEWER_BACKGROUND_LEVELS.default);
});

it('dims a custom background to the same share of its grey as the dimmer leaves', () => {
  expect(viewerBackgroundCss('dark', 0.2)).toBe('rgb(16, 16, 16)');
  expect(viewerBackgroundCss('default', 0.2)).toBe(
    'color-mix(in srgb, var(--app-bg-primary, rgb(38, 38, 38)) 20%, black)'
  );
  const [r] = viewerBackgroundRgba('dark', 0.2);
  expect(r).toBeCloseTo((80 / 255) * 0.2);
});
