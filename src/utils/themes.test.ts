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
    : value.match(/\d+/g)!.map(Number);
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
    it('meets AA for studio body text and icons on the chrome ramp', () => {
      const surfaces =
        shade === 'darkest'
          ? ['#292a2c', '#323335']
          : ['ink', 'inset', 'bg', 'raised', 'hover'].map((role) => palette[`--studio-shade-${role}`]);
      const foregrounds =
        shade === 'darkest'
          ? ['#e0e1e3', '#a5a7aa']
          : ['text', 'muted', 'line'].map((role) => palette[`--studio-shade-${role}`]);
      for (const bg of surfaces) {
        for (const fg of foregrounds) expect(contrast(fg, bg)).toBeGreaterThanOrEqual(4.5);
        expect(contrast(shade === 'darkest' ? '#9cb8d0' : palette['--studio-shade-accent'], bg)).toBeGreaterThanOrEqual(
          3
        );
      }
    });
    if (shade !== 'darkest')
      it('uses strictly neutral chrome greys', () => {
        for (const [key, value] of Object.entries(palette)) {
          if (key !== '--studio-focus' && key !== '--studio-shade-accent') expect(new Set(rgb(value)).size).toBe(1);
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
