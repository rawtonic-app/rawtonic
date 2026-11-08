// @ts-expect-error node:fs is untyped in the frontend tsconfig
import { readFileSync } from 'node:fs';
import { describe, expect, it, vi } from 'vitest';
import { interfaceShade } from './themes';
import { useSettingsStore } from '../store/useSettingsStore';
import type { AppSettings } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

const APP_COLOR_VARIABLES: Record<string, string> = {
  '--app-bg-primary': 'rgb(24, 24, 24)',
  '--app-bg-secondary': 'rgb(35, 35, 35)',
  '--app-surface': 'rgb(28, 28, 28)',
  '--app-card-active': 'rgb(43, 43, 43)',
  '--app-button-text': 'rgb(0, 0, 0)',
  '--app-text-primary': 'rgb(232, 234, 237)',
  '--app-text-secondary': 'rgb(158, 158, 158)',
  '--app-accent': 'rgb(255, 255, 255)',
  '--app-border-color': 'rgb(45, 45, 45)',
  '--app-hover-color': 'rgb(255, 255, 255)'
};

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

it('retains the incumbent Darkest app palette as stylesheet defaults', () => {
  expect(tokens(':root {')).toEqual(APP_COLOR_VARIABLES);
});

for (const shade of ['darkest', 'dark-grey', 'medium-grey']) {
  describe(shade, () => {
    const palette = shade === 'darkest' ? APP_COLOR_VARIABLES : tokens(`:root[data-interface-shade='${shade}'] {`);
    it('meets AA for inherited body text and action icons on its surfaces', () => {
      for (const bg of ['bg-primary', 'bg-secondary', 'surface', 'card-active']) {
        for (const fg of ['text-primary', 'text-secondary']) {
          expect(contrast(palette[`--app-${fg}`], palette[`--app-${bg}`]), `${fg} / ${bg}`).toBeGreaterThanOrEqual(4.5);
        }
        expect(contrast(palette['--app-accent'], palette[`--app-${bg}`])).toBeGreaterThanOrEqual(3);
      }
      expect(contrast(palette['--app-button-text'], palette['--app-accent'])).toBeGreaterThanOrEqual(4.5);
    });
    it('meets AA for the body and icon pairs used by the studio', () => {
      if (shade === 'darkest') {
        for (const bg of ['#292a2c', '#323335'])
          for (const fg of ['#e0e1e3', '#a5a7aa']) expect(contrast(fg, bg)).toBeGreaterThanOrEqual(4.5);
        return;
      }
      // Secondary text is on normal panels, headings and raised rows. Hover
      // buttons use primary/accent text. Line is an icon/grid role, not body.
      for (const bg of ['ink', 'inset', 'bg', 'raised']) {
        for (const fg of ['text', 'muted'])
          expect(contrast(palette[`--studio-shade-${fg}`], palette[`--studio-shade-${bg}`])).toBeGreaterThanOrEqual(
            4.5
          );
      }
      for (const bg of ['ink', 'inset', 'bg', 'raised', 'hover']) {
        expect(contrast(palette['--studio-shade-text'], palette[`--studio-shade-${bg}`])).toBeGreaterThanOrEqual(4.5);
        expect(contrast(palette['--studio-shade-line'], palette[`--studio-shade-${bg}`])).toBeGreaterThanOrEqual(3);
        expect(contrast(palette['--studio-shade-accent'], palette[`--studio-shade-${bg}`])).toBeGreaterThanOrEqual(3);
      }
      // Export errors and destructive menu text use the normal panel/menu;
      // menu hover changes to white on a red surface. Warning/success labels
      // also appear on raised rows; warning stars on buttons are icons.
      for (const fg of ['warning', 'error', 'success'])
        for (const bg of fg === 'error' ? ['inset', 'bg'] : ['inset', 'bg', 'raised'])
          expect(contrast(palette[`--studio-shade-${fg}`], palette[`--studio-shade-${bg}`])).toBeGreaterThanOrEqual(
            4.5
          );
    });
    if (shade !== 'darkest') {
      it('separates sunken, panel and raised surfaces at least as much as Darkest', () => {
        // Darkest's incumbent studio panel/raised pair; the brighter Medium
        // ramp keeps stronger steps without sacrificing secondary-text AA.
        const darkestStep = contrast('#292a2c', '#323335');
        for (const [lower, upper] of [
          ['inset', 'bg'],
          ['bg', 'raised']
        ]) {
          const a = palette[`--studio-shade-${lower}`];
          const b = palette[`--studio-shade-${upper}`];
          expect(rgb(b)[0], `${lower} < ${upper}`).toBeGreaterThan(rgb(a)[0]);
          expect(contrast(a, b), `${lower} / ${upper}`).toBeGreaterThanOrEqual(darkestStep);
        }
      });
      it('keeps pressed panels at least as distinct as Darkest, independently of photo captions', () => {
        const studio = readFileSync('src/components/studio/studio.css', 'utf8') as string;
        const block = studio.slice(studio.indexOf("button[aria-pressed='true'] {")).split('}')[0];
        expect(block).toContain('var(--studio-shade-pressed-rgb,');
        const tint = rgb(palette['--studio-shade-pressed-rgb']);
        expect(block).toContain('var(--studio-shade-pressed-alpha,');
        const alpha = Number(palette['--studio-shade-pressed-alpha']);
        const darkestPanel = rgb('#292a2c');
        const darkestPressed = darkestPanel.map((c, i) => c * (1 - 36 / 255) + [120, 147, 170][i] * (36 / 255));
        const darkestStep = contrast(`rgb(${darkestPanel.join(', ')})`, `rgb(${darkestPressed.join(', ')})`);
        for (const role of ['ink', 'inset', 'bg', 'raised', 'hover']) {
          const bg = rgb(palette[`--studio-shade-${role}`]);
          const composite = `rgb(${bg.map((c, i) => c * (1 - alpha) + tint[i] * alpha).join(', ')})`;
          if (role === 'bg' || role === 'raised')
            expect(contrast(composite, palette[`--studio-shade-${role}`])).toBeGreaterThanOrEqual(darkestStep);
          expect(contrast(palette['--studio-shade-accent'], composite)).toBeGreaterThanOrEqual(4.5);
        }
      });
    }
    if (shade !== 'darkest')
      it('preserves disabled/dim hierarchy and recognisable semantic hues', () => {
        const studio = readFileSync('src/components/studio/studio.css', 'utf8') as string;
        for (const selector of ['.rawtonic-menu-item.is-disabled', '.studio-preset-amount.is-idle']) {
          const block = studio.slice(studio.indexOf(`${selector} {`)).split('}')[0];
          const role = block.match(/color: var\((--[\w-]+),/)![1];
          expect(contrast(palette['--studio-shade-text'], palette[role]), selector).toBeGreaterThanOrEqual(2);
        }
        for (const selector of [
          '.studio-layer.is-hidden .studio-layer-label',
          '.studio-mask-component.is-hidden .studio-mask-component-label'
        ]) {
          const block = studio.slice(studio.indexOf(`${selector} {`)).split('}')[0];
          const role = block.match(/color: var\((--[\w-]+),/)![1];
          for (const bg of ['bg', 'raised', 'hover'])
            expect(contrast(palette[role], palette[`--studio-shade-${bg}`]), selector).toBeGreaterThanOrEqual(4.5);
        }
        expect(contrast(palette['--studio-shade-text'], palette['--studio-shade-dim'])).toBeGreaterThanOrEqual(2);
        expect(contrast(palette['--studio-shade-text'], palette['--studio-shade-line'])).toBeGreaterThanOrEqual(1.5);
        expect(contrast(palette['--studio-shade-text'], palette['--studio-shade-muted'])).toBeGreaterThanOrEqual(1.25);
        const [wr, , wb] = rgb(palette['--studio-shade-warning']);
        const [er, eg] = rgb(palette['--studio-shade-error']);
        const [sr, sg, sb] = rgb(palette['--studio-shade-success']);
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
          value.replace(
            /var\((--[\w-]+),\s*([^()]+)\)/g,
            (_, key: string, fallback: string) => palette[key] ?? fallback
          );
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
    if (shade !== 'darkest')
      it('uses strictly neutral chrome greys', () => {
        for (const [key, value] of Object.entries(palette)) {
          if (
            ![
              '--studio-focus',
              '--studio-shade-accent',
              '--studio-shade-warning',
              '--studio-shade-error',
              '--studio-shade-success',
              '--studio-shade-pressed-alpha'
            ].includes(key)
          )
            expect(new Set(rgb(value)).size).toBe(1);
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
