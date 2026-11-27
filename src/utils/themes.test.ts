// @ts-expect-error node:fs is untyped in the frontend tsconfig
import { readFileSync } from 'node:fs';
import { describe, expect, it, vi } from 'vitest';
import { interfaceShade } from './themes';
import { useSettingsStore } from '../store/useSettingsStore';
import type { AppSettings } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

const css = readFileSync('src/styles.css', 'utf8');
function tokens(selector: string): Record<string, string> {
  const body = css.slice(css.indexOf(selector) + selector.length).split('}')[0];
  return Object.fromEntries([...body.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map((m) => [m[1], m[2]]));
}
function rgb(value: string): number[] {
  return value.startsWith('#')
    ? [1, 3, 5].map((i) => parseInt(value.slice(i, i + 2), 16))
    : value
        .match(/[\d.]+/g)!
        .slice(0, 3)
        .map(Number);
}
function contrast(a: string, b: string): number {
  const luminance = (value: string) =>
    rgb(value).reduce((sum, n, i) => {
      const c = n / 255;
      return sum + [0.2126, 0.7152, 0.0722][i] * (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
    }, 0);
  const [light, dark] = [luminance(a), luminance(b)].sort((a, b) => b - a);
  return (light + 0.05) / (dark + 0.05);
}

const SHADES = ['darkest', 'dark-grey', 'medium-grey'] as const;
const NON_GREY = [
  '--studio-focus',
  '--studio-shade-accent',
  '--studio-rgb-accent',
  '--studio-accent-fill',
  '--studio-accent-fill-hover',
  '--studio-accent-tint',
  '--studio-on-accent',
  '--studio-shade-pressed-rgb',
  '--studio-shade-pressed-alpha',
  '--studio-shade-warning',
  '--studio-shade-error',
  '--studio-shade-success',
  '--app-accent',
  '--app-button-text'
];
const shadeTokens = (shade: string) =>
  tokens(
    shade === 'darkest' ? ":root,\n:root[data-interface-shade='darkest'] {" : `:root[data-interface-shade='${shade}'] {`
  );
const luminance = (value: string) => contrast(value, '#000000');

it('defines the same role tokens in every interface shade', () => {
  const keys = Object.keys(shadeTokens('darkest')).sort();
  expect(keys).toContain('--studio-shade-viewer');
  for (const shade of SHADES) expect(Object.keys(shadeTokens(shade)).sort()).toEqual(keys);
});

it('keeps chrome colours in the token file: studio stylesheets name no grey or accent literals', () => {
  const sheets = [
    'src/components/studio/studio.css',
    'src/components/session/session.css',
    'src/components/session/tether.css',
    'src/components/activity/activity.css',
    'src/components/studio/multiView.css',
    'src/components/ui/naming-editor.css'
  ];
  for (const sheet of sheets) {
    const text = readFileSync(sheet, 'utf8') as string;
    for (const [hex] of text.matchAll(/#[0-9a-fA-F]{6}\b/g)) {
      const [r, g, b] = rgb(hex.toLowerCase());
      const max = Math.max(r, g, b);
      const min = Math.min(r, g, b);
      const grey = max - min <= 6 || (max > 0 && (max - min) / max < 0.07);
      const pureExtreme = (r === 255 && g === 255 && b === 255) || max === 0;
      expect(grey && !pureExtreme, `${sheet}: ${hex}`).toBe(false);
    }
  }
});

for (const shade of SHADES) {
  describe(shade, () => {
    const palette = shadeTokens(shade);
    const role = (name: string) => palette[`--studio-shade-${name}`];
    it('uses strictly neutral role greys', () => {
      for (const [key, value] of Object.entries(palette)) {
        if (NON_GREY.includes(key)) continue;
        const channels = key.includes('rgb') ? value.split(' ').map(Number) : rgb(value);
        expect(new Set(channels).size, key).toBe(1);
      }
    });
    it('orders the roles: chrome and panels darker than the viewer, controls above panels', () => {
      const order = ['ink', 'inset', 'bg', 'viewer'];
      for (let i = 1; i < order.length; i++)
        expect(luminance(role(order[i])), `${order[i - 1]} < ${order[i]}`).toBeGreaterThan(
          luminance(role(order[i - 1]))
        );
      expect(luminance(role('raised'))).toBeGreaterThan(luminance(role('bg')));
      expect(luminance(role('hover'))).toBeGreaterThan(luminance(role('raised')));
      expect(palette['--app-bg-primary']).toBe(role('viewer'));
      expect(palette['--studio-shade-panel']).toBe(role('bg'));
    });
    it('meets AA for text on every surface', () => {
      for (const bg of ['ink', 'inset', 'bg', 'viewer', 'raised', 'hover'])
        for (const fg of ['text', 'muted'])
          expect(contrast(role(fg), role(bg)), `${fg} / ${bg}`).toBeGreaterThanOrEqual(4.5);
      for (const bg of ['inset', 'bg', 'viewer', 'raised']) {
        expect(contrast(role('accent'), role(bg)), `accent / ${bg}`).toBeGreaterThanOrEqual(4.5);
        expect(contrast(role('line'), role(bg)), `line / ${bg}`).toBeGreaterThanOrEqual(3);
      }
      expect(contrast(palette['--studio-on-accent'], palette['--studio-accent-fill'])).toBeGreaterThanOrEqual(4.5);
      expect(contrast(palette['--studio-on-accent'], palette['--studio-accent-fill-hover'])).toBeGreaterThanOrEqual(
        4.5
      );
      expect(contrast(palette['--app-button-text'], palette['--app-accent'])).toBeGreaterThanOrEqual(4.5);
      // Inherited dialogs use the app accent for links, focus rings and
      // accent text: readable on their surfaces, and a ring on hover fills.
      for (const bg of ['bg-primary', 'bg-secondary', 'surface'])
        expect(contrast(palette['--app-accent'], palette[`--app-${bg}`]), `app accent / ${bg}`).toBeGreaterThanOrEqual(
          4.5
        );
      expect(contrast(palette['--app-accent'], palette['--app-card-active'])).toBeGreaterThanOrEqual(3);
      // Expanded stack badges and selection tints carry text.
      const mix = palette['--studio-accent-tint'].match(/color-mix\(in srgb, (#\w+) (\d+)%, (#\w+)\)/)!;
      const share = Number(mix[2]) / 100;
      const [a, b] = [rgb(mix[1]), rgb(mix[3])];
      const tint = `rgb(${a.map((c, i) => Math.round(c * share + b[i] * (1 - share))).join(', ')})`;
      expect(contrast(role('text'), tint), 'text / accent tint').toBeGreaterThanOrEqual(4.5);
      for (const fg of ['warning', 'error', 'success'])
        for (const bg of ['inset', 'bg', 'raised'])
          expect(contrast(role(fg), role(bg)), `${fg} / ${bg}`).toBeGreaterThanOrEqual(4.5);
    });
    it('marks pressed buttons with the accent, readable and distinct from the panel', () => {
      const studio = readFileSync('src/components/studio/studio.css', 'utf8') as string;
      const block = studio.slice(studio.indexOf("button[aria-pressed='true'] {")).split('}')[0];
      expect(block).toContain('var(--studio-shade-pressed-rgb)');
      expect(block).toContain('var(--studio-shade-pressed-alpha)');
      const tint = rgb(`rgb(${palette['--studio-shade-pressed-rgb'].split(' ').join(', ')})`);
      const alpha = Number(palette['--studio-shade-pressed-alpha']);
      for (const name of ['bg', 'raised']) {
        const bg = rgb(role(name));
        const composite = `rgb(${bg.map((c, i) => c * (1 - alpha) + tint[i] * alpha).join(', ')})`;
        expect(contrast(composite, role(name))).toBeGreaterThanOrEqual(1.25);
        expect(contrast(role('text'), composite)).toBeGreaterThanOrEqual(4.5);
      }
    });
    it('preserves disabled/dim hierarchy and recognisable semantic hues', () => {
      expect(contrast(role('text'), role('dim'))).toBeGreaterThanOrEqual(2);
      expect(contrast(role('dim'), role('bg'))).toBeGreaterThanOrEqual(2.5);
      const [wr, , wb] = rgb(role('warning'));
      const [er, eg] = rgb(role('error'));
      const [sr, sg, sb] = rgb(role('success'));
      expect(wr - wb).toBeGreaterThanOrEqual(80);
      expect(er - eg).toBeGreaterThanOrEqual(50);
      expect(sg - Math.max(sr, sb)).toBeGreaterThanOrEqual(25);
    });
    it('keeps photo-overlay captions readable over white and black photographs', () => {
      const studio = readFileSync('src/components/studio/studio.css', 'utf8') as string;
      for (const selector of [
        '.studio-photo-type',
        '.studio-photo-stars',
        '.brush-gesture-readout',
        '.studio-camera-preview-note'
      ]) {
        const block = studio.slice(studio.indexOf(`${selector} {`)).split('}')[0];
        const resolve = (value: string): string =>
          value.replace(/var\((--[\w-]+)(?:,\s*([^()]+))?\)/g, (_, key: string, fallback?: string) => {
            const v = palette[key] ?? fallback ?? '';
            return /^\d+ \d+ \d+$/.test(v) ? v.split(' ').join(', ') : v;
          });
        const bg = resolve(block.match(/background: ([^;]+);/)![1]);
        const fg = resolve(block.match(/(?:^|\n)\s*color: ([^;]+);/)![1]);
        const channels = rgb(bg);
        const alpha =
          bg.startsWith('#') && bg.length === 9
            ? parseInt(bg.slice(7), 16) / 255
            : Number(bg.match(/\/\s*([\d.]+)/)?.[1] ?? 1);
        for (const photo of [0, 255]) {
          const composite = `rgb(${channels.map((c) => Math.round(c * alpha + photo * (1 - alpha))).join(', ')})`;
          expect(contrast(fg, composite), `${selector} over ${photo}`).toBeGreaterThanOrEqual(4.5);
        }
      }
    });
  });
}

it('applies loaded and changed settings live, saves the choice, and falls back safely', async () => {
  const { invoke } = await import('@tauri-apps/api/core');
  useSettingsStore.getState().setAppSettings({ interfaceShade: 'dark-grey' } as AppSettings);
  expect(document.documentElement.dataset.interfaceShade).toBe('dark-grey');
  await useSettingsStore
    .getState()
    .handleSettingsChange({ interfaceShade: 'medium-grey', viewerBackground: 'black' } as AppSettings);
  expect(document.documentElement.dataset.interfaceShade).toBe('medium-grey');
  expect(invoke).toHaveBeenLastCalledWith('save_settings', {
    settings: { interfaceShade: 'medium-grey', viewerBackground: 'black' }
  });
  useSettingsStore.getState().setAppSettings({} as AppSettings);
  expect(document.documentElement.dataset.interfaceShade).toBe('darkest');
  expect(interfaceShade('future-choice')).toBe('darkest');
});
