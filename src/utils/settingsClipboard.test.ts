import { describe, expect, it } from 'vitest';
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER, type Adjustments, type MaskContainer } from './adjustments';
import {
  RESPONSE_KEYS,
  SETTINGS_GROUPS,
  chosenSettings,
  newEditResponses,
  detectionPending,
  groupModified,
  pasteLayers,
  removedWhenNull,
  samePhotograph,
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
  });

  it('takes away what the source does not have: As Shot over a custom white balance', () => {
    const asShot = recipe({ temperature: 0, tint: 0 });
    delete (asShot as Partial<Adjustments>).whiteBalance;
    delete (asShot as Partial<Adjustments>).wbTemperature;
    const chosen = chosenSettings(asShot, group('whiteBalance').keys, []);
    expect(chosen.whiteBalance).toBeNull();
    const custom = recipe({ whiteBalance: 'custom', wbTemperature: 2850 } as Partial<Adjustments>);
    const pasted = removedWhenNull(custom, chosen);
    expect('whiteBalance' in pasted).toBe(false);
    expect('wbTemperature' in pasted).toBe(false);
    expect(pasted.temperature).toBe(0);
  });

  it('offers no group of processing versions among the tools', () => {
    for (const g of SETTINGS_GROUPS) for (const key of g.keys) expect(RESPONSE_KEYS).not.toContain(key);
    expect(RESPONSE_KEYS).toContain('toneVersion');
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

  it('adds pasted layers, replaces one pasted before, keeps the photo’s own', () => {
    const own = [layer('dodge'), { ...layer('sky'), name: 'old sky' }];
    const merged = pasteLayers(own, [layer('sky'), layer('vignette')], false);
    expect(merged.map((l) => [l.id, l.name])).toEqual([
      ['dodge', 'dodge'],
      ['sky', 'sky'],
      ['vignette', 'vignette'],
    ]);
    // Edit all selected: the primary's layers only.
    expect(pasteLayers(own, [layer('sky')], false, true).map((l) => l.id)).toEqual(['sky']);
  });

  it('never pastes another photo’s AI bitmap; a virtual copy of the same keeps it', () => {
    const pasted = [layer('sky', 'ai-sky', { maskDataBase64: 'data:source', grow: 10 }), layer('people', 'ai-people', { parts: [] })];
    const onOther = pasteLayers([], pasted, false);
    expect(onOther[0].subMasks[0].parameters).toEqual({ maskDataBase64: null, grow: 10, detectPending: true });
    expect(detectionPending(onOther[0].subMasks[0])).toBe(true);
    // A People component without parts has nothing to detect.
    expect(detectionPending(onOther[1].subMasks[0])).toBe(false);
    const onCopy = pasteLayers([], pasted, true);
    expect(onCopy[0].subMasks[0].parameters).toEqual({ maskDataBase64: 'data:source', grow: 10 });
    // Detected there: no longer pending.
    expect(detectionPending({ ...onOther[0].subMasks[0], parameters: { maskDataBase64: 'data:own', detectPending: true } })).toBe(false);
  });

  it('knows virtual copies of one photograph', () => {
    expect(samePhotograph('/a/B.ARW', '/a/B.ARW?vc=c0ffee')).toBe(true);
    expect(samePhotograph('/a/B.ARW', '/a/C.ARW')).toBe(false);
    expect(samePhotograph(null, '/a/B.ARW')).toBe(false);
  });
});
