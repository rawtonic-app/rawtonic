/**
 * Systematic round trip of develop defaults (coordinator condition after
 * review round 3): for every setting a default can carry and every response
 * version a source recipe can have (none = legacy, the oldest, the newest),
 * a default made from the photo (photo level, per tool, preset/global) and
 * applied to a never-edited photo shows the source's value in the source's
 * response — and a tool's reset to it gives the same.
 */
import { describe, expect, it, vi } from 'vitest';
import {
  initialAdjustmentsFor,
  normalizeLoadedAdjustments,
  sameAdjustmentValue,
  type Adjustments
} from './adjustments';
import {
  isDefaultKey,
  presetDefaultSettings,
  resolvedDefaultSettings,
  toolDefaultSettings,
  withDevelopDefaults
} from './developDefaults';
import { effectiveVersions, PASTED_VERSION_KEYS, versionKeysUsing } from './pastedVersions';
import { collectPresetAdjustments } from '../hooks/usePresets';
import { photoDefaultSettings, withToolResetToDefaults } from '../hooks/developDefaultsActions';
import type { Preset } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

const base = initialAdjustmentsFor(true);
// Numeric sliders a default can carry (not response versions themselves).
const SLIDERS = Object.entries(base)
  .filter(([key, value]) => typeof value === 'number' && isDefaultKey(key) && !/Version$/.test(key))
  .map(([key]) => key);
// Nested settings (colour mixer, grading, curves, …): one numeric leaf moved.
const NESTED = Object.entries(base)
  .filter(([key, value]) => value && typeof value === 'object' && !Array.isArray(value) && isDefaultKey(key))
  .map(([key]) => key)
  .filter((key) => leafPath(base[key as keyof Adjustments]) !== null);
function leafPath(value: unknown, path: string[] = []): string[] | null {
  if (typeof value === 'number') return path;
  if (!value || typeof value !== 'object') return null;
  for (const [k, v] of Object.entries(value)) {
    const found = leafPath(v, [...path, k]);
    if (found) return found;
  }
  return null;
}
function withLeafMoved(value: unknown): unknown {
  const out = structuredClone(value) as Record<string, unknown>;
  const path = leafPath(out)!;
  let at = out;
  for (const k of path.slice(0, -1)) at = at[k] as Record<string, unknown>;
  const last = path[path.length - 1];
  at[last] = (at[last] as number) === 0 ? 11 : (at[last] as number) * 0.8;
  return out;
}
// A value off the default within every slider's range.
const moved = (key: string, value: number) => {
  if (key === 'exposure') return 0.7;
  if (key === 'temperature' || key === 'tint') return value + 23;
  if (key === 'blacks') return -75;
  return value === 0 ? 23 : value * 0.8;
};
// Response configurations of the source: every key missing (a legacy edit),
// every key at 1, every key newest.
const configs: Record<string, (r: Record<string, unknown>) => void> = {
  missing: (r) => PASTED_VERSION_KEYS.forEach((k) => delete r[k]),
  oldest: (r) => PASTED_VERSION_KEYS.forEach((k) => (r[k] = 1)),
  newest: () => {}
};
const versionsOf = (key: string) => versionKeysUsing(key);
const asPreset = (adjustments: Partial<Adjustments>, presetType: 'style' | 'tool') =>
  ({ id: 'p', name: 'p', presetType, adjustments }) as unknown as Preset;

describe('develop defaults round trip', () => {
  it('covers every slider and nested tool', () => {
    expect(SLIDERS.length).toBeGreaterThan(40);
    expect(NESTED).toEqual(expect.arrayContaining(['hsl', 'colorGrading']));
  });
  for (const [config, set] of Object.entries(configs)) {
    it(`reproduces every slider of a source with ${config} responses`, () => {
      const failures: string[] = [];
      for (const key of [...SLIDERS, ...NESTED]) {
        const raw = { ...base } as Record<string, unknown>;
        const own = base[key as keyof Adjustments];
        raw[key] = typeof own === 'number' ? moved(key, own) : withLeafMoved(own);
        set(raw);
        const source = normalizeLoadedAdjustments(raw as unknown as Adjustments, true);
        const want = effectiveVersions(source, true);
        const ways: Record<string, Record<string, unknown>> = {
          photo: photoDefaultSettings(source, true),
          tool: toolDefaultSettings(source, [key], true)
        };
        // A preset (or the global default made from one) saved from a photo
        // is portable by design: it carries no response a value does not
        // need and applies in the newest, as applying it to a never-edited
        // photo does; so it reproduces sources in the current responses.
        if (config === 'newest')
          ways.preset = presetDefaultSettings(
            asPreset(
              collectPresetAdjustments(source, base, {
                includeMasks: false,
                includeCropTransform: false,
                presetType: 'style',
                isRaw: true
              }),
              'style'
            )
          );
        for (const [way, settings] of Object.entries(ways)) {
          // As the engine resolves a saved level for a new edit.
          const resolved = { levels: ['camera' as const], settings: resolvedDefaultSettings(settings) };
          const applied = withDevelopDefaults(base, resolved);
          const reset = withToolResetToDefaults(
            { ...applied, [key]: structuredClone(base[key as keyof Adjustments]) } as Adjustments,
            resolved,
            [key],
            true
          );
          for (const [result, recipe] of [
            ['applied', applied],
            ['reset', reset]
          ] as const) {
            const got = effectiveVersions(recipe, true);
            const value = (recipe as unknown as Record<string, unknown>)[key];
            const wanted = source[key as keyof Adjustments];
            const same =
              typeof wanted === 'number'
                ? typeof value === 'number' && Math.abs(value - wanted) < 1e-6
                : sameAdjustmentValue(value, wanted);
            const versionsSame = versionsOf(key).every((k) => sameAdjustmentValue(got[k], want[k]));
            if (!same || !versionsSame)
              failures.push(
                `${way}/${result} ${key}: ${JSON.stringify(value)} vs ${JSON.stringify(source[key as keyof Adjustments])}` +
                  versionsOf(key)
                    .map((k) => ` ${k} ${got[k]} vs ${want[k]}`)
                    .join('')
              );
          }
        }
      }
      expect(failures).toEqual([]);
    });
  }
});
