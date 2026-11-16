import { describe, expect, it } from 'vitest';
import nativeDefaults from '../../crates/engine/src/develop_defaults.rs?raw';
import { initialAdjustmentsFor, INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';
import { originalPreviewAdjustments } from './beforeAfter';
import { editorRecipe } from './neverEdited';
import {
  DEVELOP_DEFAULTS_KEY,
  defaultSettingsOf,
  followsDefaults,
  isDefaultKey,
  NO_DEVELOP_DEFAULTS,
  presetDefaultSettings,
  toolDefaultSettings,
  withCameraDefault,
  withCameraToolDefault,
  withDevelopDefaults,
  withGlobalDefault,
  withoutCameraDefault,
  withoutDevelopDefaults,
  type ResolvedDefaults
} from './developDefaults';
import type { Preset } from '../components/ui/AppProperties';
import { toolPresetToSave } from '../components/studio/toolPresets';

const z9 = { make: 'Nikon', model: 'Z 9', serial: '3001234' };
const resolved: ResolvedDefaults = { levels: ['global', 'camera'], settings: { contrast: 20, vibrance: 5 } };

describe('develop defaults', () => {
  it('records its key as the backend does, and never takes a per-photo key the backend refuses', () => {
    expect(nativeDefaults).toContain(`pub const KEY: &str = "${DEVELOP_DEFAULTS_KEY}";`);
    const list = nativeDefaults.match(/const NEVER_DEFAULTED: &\[&str\] = &\[([\s\S]*?)\];/)![1];
    const keys = [...list.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    expect(keys.length).toBeGreaterThan(5);
    for (const key of keys) expect(isDefaultKey(key), key).toBe(false);
  });

  it('follows the defaults only while it has no edit and its defaults are not decided', () => {
    expect(followsDefaults(null)).toBe(true);
    expect(followsDefaults({ is_null: true })).toBe(true);
    expect(followsDefaults({ sourceFrameVersion: 1, colorLabel: '' })).toBe(true);
    expect(followsDefaults({ sourceFrameVersion: 1, developDefaults: [] })).toBe(false);
    expect(followsDefaults({ exposure: 0.3 })).toBe(false);
    expect(followsDefaults({ contrast: 20, developDefaults: ['global'] })).toBe(false);
  });

  it('overlays the resolved settings with their record; nothing without defaults', () => {
    const base = initialAdjustmentsFor(true);
    expect(withDevelopDefaults(base, null)).toBe(base);
    const applied = withDevelopDefaults(base, resolved);
    expect(applied.contrast).toBe(20);
    expect(applied.vibrance).toBe(5);
    expect(applied.developDefaults).toEqual(['global', 'camera']);
    expect(withoutDevelopDefaults(base).developDefaults).toEqual([]);
  });

  it('takes a preset as it applies to a new edit, without its masks, crop, transform or patches', () => {
    const preset = {
      id: 'p',
      name: 'House look',
      presetType: 'style',
      adjustments: {
        exposure: 0.4,
        crop: { x: 0, y: 0, width: 10, height: 10, unit: 'px' },
        rotation: 3,
        masks: [],
        aiPatches: [],
        lensCorrectionMode: 'off'
      }
    } as unknown as Preset;
    const settings = presetDefaultSettings(preset);
    expect(settings.exposure).toBe(0.4);
    for (const key of ['crop', 'rotation', 'masks', 'aiPatches', 'lensCorrectionMode', 'renderVersion'])
      expect(settings, key).not.toHaveProperty(key);
  });

  it('keeps of a photo only what differs from a new edit; a tool keeps all of its settings', () => {
    const base = initialAdjustmentsFor(true);
    const photo = { ...base, exposure: 1, rotation: 5, crop: { x: 1 } } as unknown as Adjustments;
    expect(defaultSettingsOf(photo, base)).toEqual({ exposure: 1 });
    const tool = toolDefaultSettings({ ...base, grainAmount: 0 } as Adjustments, ['grainAmount'], true);
    expect(tool).toHaveProperty('grainAmount', 0);
    // Blacks travel as amounts in a tool copy; a default holds the value the
    // new edit renders, so it reproduces the photo's −75.
    const blacks = toolDefaultSettings({ ...base, blacks: -75 } as Adjustments, ['blacks'], true);
    expect(blacks.blacks as number).toBeCloseTo(-75, 6);
    // A preset saved from that photo's tool (Blacks as its amount) likewise.
    const preset = {
      id: 't',
      name: 'Blacks',
      presetType: 'tool',
      adjustments: toolPresetToSave({ ...base, blacks: -75 } as Adjustments, ['blacks'], true)
    } as unknown as Preset;
    expect(presetDefaultSettings(preset).blacks as number).toBeCloseTo(-75, 6);
  });

  it('a preset replaces a camera default, a tool joins it; bodies are kept apart by serial', () => {
    let d = withGlobalDefault(NO_DEVELOP_DEFAULTS, { name: 'G', settings: { contrast: 10 } });
    d = withCameraToolDefault(d, z9, 'camera', { grainAmount: 20 });
    expect(d.cameras).toHaveLength(1);
    expect(d.cameras![0]).toMatchObject({ make: 'Nikon', model: 'Z 9', inherit: true, settings: { grainAmount: 20 } });
    expect(d.cameras![0]).not.toHaveProperty('serial');
    d = withCameraToolDefault(d, { ...z9, make: 'NIKON' }, 'camera', { sharpness: 30 });
    expect(d.cameras).toHaveLength(1);
    expect(d.cameras![0].settings).toEqual({ grainAmount: 20, sharpness: 30 });
    d = withCameraDefault(d, z9, 'body', { name: 'Body look', settings: { exposure: 0.2 } });
    expect(d.cameras).toHaveLength(2);
    expect(d.cameras![1]).toMatchObject({ serial: '3001234', inherit: false });
    d = withCameraDefault(d, z9, 'camera', { name: 'P', settings: { clarity: 5 } });
    expect(d.cameras![0]).toMatchObject({ inherit: false, settings: { clarity: 5 } });
    expect(withoutCameraDefault(d, 0).cameras).toHaveLength(1);
    expect(withGlobalDefault(d, null)).not.toHaveProperty('global');
  });

  it('opens a never-edited photo with its defaults over the new edit; edited and neutral ones without', () => {
    const fresh = editorRecipe(null, true, 6000, 4000, null, null, resolved);
    expect(fresh.contrast).toBe(20);
    expect(fresh.developDefaults).toEqual(['global', 'camera']);
    // A Reset's recipe (only its frame) follows too.
    expect(editorRecipe({ sourceFrameVersion: 1 }, true, 6000, 4000, null, null, resolved).contrast).toBe(20);
    const neutral = editorRecipe({ developDefaults: [] }, true, 6000, 4000, null, null, resolved);
    expect(neutral.contrast).toBe(INITIAL_ADJUSTMENTS.contrast);
    expect(neutral.developDefaults).toEqual([]);
    const edited = editorRecipe(
      { ...initialAdjustmentsFor(true), exposure: 1 },
      true,
      6000,
      4000,
      null,
      null,
      resolved
    );
    expect(edited.contrast).toBe(INITIAL_ADJUSTMENTS.contrast);
    expect(edited.developDefaults).toBeUndefined();
  });

  it('shows in the before the defaults its edit began with, never later ones', () => {
    // Begun with Contrast +20 / Vibrance 5; the defaults changed since (the
    // before takes nothing from today's).
    const edit = { ...withDevelopDefaults(initialAdjustmentsFor(true), resolved), contrast: 35, exposure: 1 };
    expect(edit.developDefaultsSettings).toEqual(resolved.settings);
    const before = originalPreviewAdjustments(edit, null, true);
    expect(before.contrast).toBe(20);
    expect(before.vibrance).toBe(5);
    expect(before.exposure).toBe(INITIAL_ADJUSTMENTS.exposure);
    // Begun neutral, or before develop defaults: a neutral before.
    const neutralEdit = { ...withoutDevelopDefaults(edit), contrast: 35 };
    expect(neutralEdit).not.toHaveProperty('developDefaultsSettings');
    expect(originalPreviewAdjustments(neutralEdit, null, true).contrast).toBe(INITIAL_ADJUSTMENTS.contrast);
    const older = { ...initialAdjustmentsFor(true), contrast: 35 };
    expect(originalPreviewAdjustments(older, null, true).contrast).toBe(INITIAL_ADJUSTMENTS.contrast);
  });
});
