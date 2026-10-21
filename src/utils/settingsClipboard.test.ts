import { describe, expect, it } from 'vitest';
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER, type Adjustments, type MaskContainer } from './adjustments';
import {
  RESPONSE_KEYS,
  SETTINGS_GROUPS,
  chosenSettings,
  detectionPending,
  newEditResponses,
  groupModified,
} from './settingsClipboard';

const layer = (id: string, type = 'linear', parameters: Record<string, unknown> = {}): MaskContainer =>
  ({
    ...structuredClone(INITIAL_MASK_CONTAINER),
    id,
    name: id,
    subMasks: [{ id: `${id}-c`, type, visible: true, invert: false, opacity: 100, mode: 'additive', parameters }],
  }) as MaskContainer;
const recipe = (patch: Partial<Adjustments>): Adjustments => ({ ...structuredClone(INITIAL_ADJUSTMENTS), ...patch });
const group = (label: string) => SETTINGS_GROUPS.find((g) => g.label === `modals.copyPaste.groups.${label}`)!;

describe('settings clipboard', () => {
  it('takes the chosen groups and layers only', () => {
    const source = recipe({ exposure: 1, contrast: 20, masks: [layer('sky'), layer('dodge')] });
    const chosen = chosenSettings(source, [...group('tone').keys, 'masks'], ['sky']);
    expect(chosen.contrast).toBe(20);
    expect('exposure' in chosen).toBe(false);
    expect(chosen.masks?.map((l) => l.id)).toEqual(['sky']);
    expect(chosenSettings(source, group('tone').keys, []).masks).toBeUndefined();
  });

  it('takes processing versions only when chosen, a missing one as missing', () => {
    const source = recipe({ toneVersion: 2, sharpenVersion: 2, contrast: 20 } as Partial<Adjustments>);
    delete (source as Partial<Adjustments>).defringeVersion;
    // Not chosen: none, also with the tools that use them.
    const plain = chosenSettings(source, [...group('tone').keys, ...group('sharpness').keys], []);
    for (const key of RESPONSE_KEYS) expect(key in plain).toBe(false);
    expect(plain.contrast).toBe(20);
    // Chosen: every one, a missing one as null (it travels as missing).
    const withVersions = chosenSettings(source, [...group('tone').keys, ...RESPONSE_KEYS], []) as Record<string, unknown>;
    expect(withVersions.toneVersion).toBe(2);
    expect(withVersions.defringeVersion).toBeNull();
    // Missing ones a never-edited RAW would get anyway are stated as the
    // renderer reads them missing (no edge-fringe pass, the first CA estimate).
    delete (source as Partial<Adjustments>).fringeVersion;
    delete (source as Partial<Adjustments>).autoCaVersion;
    const stated = chosenSettings(source, RESPONSE_KEYS, []) as Record<string, unknown>;
    expect([stated.fringeVersion, stated.autoCaVersion]).toEqual([0, 1]);
  });

  it('takes away what the source does not have: As Shot over a custom white balance', () => {
    const asShot = recipe({ temperature: 0, tint: 0 });
    delete (asShot as Partial<Adjustments>).whiteBalance;
    delete (asShot as Partial<Adjustments>).wbTemperature;
    const chosen = chosenSettings(asShot, group('whiteBalance').keys, []);
    // Null: the destination loses its own (the backend removes it).
    expect(chosen.whiteBalance).toBeNull();
    expect(chosen.wbTemperature).toBeNull();
    expect(chosen.temperature).toBe(0);
  });

  it('knows a component waiting for its detection', () => {
    const sub = layer('sky', 'ai-sky', { maskDataBase64: null, detectPending: true }).subMasks[0];
    expect(detectionPending(sub)).toBe(true);
    expect(detectionPending({ ...sub, parameters: { maskDataBase64: 'data:own', detectPending: true } })).toBe(false);
  });

  it('offers no group of processing versions among the tools', () => {
    for (const g of SETTINGS_GROUPS) for (const key of g.keys) expect(RESPONSE_KEYS).not.toContain(key);
    expect(RESPONSE_KEYS).toContain('toneVersion');
    // The camera CA version goes with the camera's lens data.
    expect(RESPONSE_KEYS).not.toContain('cameraCaVersion');
    expect(SETTINGS_GROUPS.find((g) => g.keys.includes('cameraCaVersion'))?.keys).toContain('lensCorrectionMode');
  });

  it('gives a never-edited photo the versions of a new edit', () => {
    const { raw, other } = newEditResponses();
    expect(raw.toneVersion).toBeGreaterThan(1);
    expect('fringeVersion' in other).toBe(false);
  });

  it('finds the groups a photograph changed', () => {
    const source = recipe({ contrast: 20 });
    expect(groupModified(group('tone'), source, true)).toBe(true);
    expect(groupModified(group('vignette'), source, true)).toBe(false);
  });

});
