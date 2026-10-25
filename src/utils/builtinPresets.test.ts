import { describe, expect, it } from 'vitest';
import shipped from '../../src-tauri/resources/presets/builtin.json';
import en from '../i18n/locales/en.json';
import de from '../i18n/locales/de.json';
import { COPYABLE_ADJUSTMENT_KEYS, INITIAL_ADJUSTMENTS } from './adjustments';
import { COPIED_ALONG } from '../components/studio/StudioToolCopy';
import { presetFitsTool } from '../components/studio/toolPresets';
import type { Preset } from '../components/ui/AppProperties';

const presets = shipped.presets as unknown as Preset[];

/** The parameters of the tools that have a preset menu (as in their headers). */
const TOOL_KEYS = {
  grain: ['grainAmount', 'grainSize', 'grainRoughness', 'grainStructure'],
  vignette: ['vignetteAmount', 'vignetteMidpoint', 'vignetteRoundness', 'vignetteFeather', 'vignetteStyle', 'vignetteHighlights'],
  grading: ['colorGrading'],
  bw: ['blackAndWhite', 'grayMixer'],
  sharpening: ['sharpness', 'sharpenRadius', 'sharpenDetail', 'sharpenMasking', 'sharpnessThreshold', 'sharpnessFalloff'],
  noise: ['aiDenoise', 'lumaNoiseReduction', 'lumaNoiseDetail', 'lumaNoiseContrast', 'colorNoiseReduction', 'colorNoiseDetail', 'colorNoiseSmoothness'],
};

describe('built-in presets resource', () => {
  it('has unique ids with a version and an English and German name', () => {
    expect(shipped.format).toBe(1);
    expect(new Set(presets.map((p) => p.id)).size).toBe(presets.length);
    const names = (locale: typeof en) => locale.studio.presets.builtin as Record<string, string>;
    for (const preset of presets) {
      expect(preset.version).toBeGreaterThanOrEqual(1);
      expect(names(en)[preset.id]).toBe(preset.name);
      expect(names(de)[preset.id]).toBeTruthy();
    }
  });

  it('sets only recipe settings, with the versions their values render by', () => {
    for (const preset of presets) {
      for (const [key, value] of Object.entries(preset.adjustments)) {
        expect(COPYABLE_ADJUSTMENT_KEYS, `${preset.id}: ${key}`).toContain(key);
        // Nested settings are complete, so applying one sets the whole tool.
        const initial = (INITIAL_ADJUSTMENTS as Record<string, unknown>)[key];
        if (initial && typeof initial === 'object' && !Array.isArray(initial))
          expect(Object.keys(value as object).sort(), `${preset.id}: ${key}`).toEqual(Object.keys(initial).sort());
        for (const along of COPIED_ALONG[key] ?? [])
          expect(preset.adjustments, `${preset.id}: ${key} needs ${along}`).toHaveProperty(along);
      }
      if ('grainAmount' in preset.adjustments) expect(preset.adjustments).toHaveProperty('grainVersion');
      // Lowered Highlights also render by the tone-slider response (shader.wgsl).
      if (((preset.adjustments.highlights as number | undefined) ?? 0) < 0)
        expect(preset.adjustments, `${preset.id}: highlights needs toneVersion`).toHaveProperty('toneVersion');
    }
  });

  it("gives every tool preset to exactly one tool's menu", () => {
    for (const preset of presets.filter((p) => p.presetType === 'tool')) {
      const tools = Object.entries(TOOL_KEYS).filter(([, keys]) => presetFitsTool(preset, keys));
      expect(tools, preset.id).toHaveLength(1);
    }
    for (const keys of Object.values(TOOL_KEYS))
      expect(presets.filter((p) => presetFitsTool(p, keys)).length).toBeGreaterThanOrEqual(3);
  });
});
