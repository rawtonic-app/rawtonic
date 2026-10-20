import { describe, expect, it } from 'vitest';
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER, type Adjustments, type MaskContainer } from './adjustments';
import {
  SETTINGS_GROUPS,
  chosenSettings,
  detectionPending,
  groupModified,
  pasteLayers,
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

  it('carries the responses a chosen layer’s values mean, also without their tools', () => {
    const sharpLayer = { ...layer('detail'), adjustments: { ...layer('detail').adjustments, sharpness: 30 } };
    const source = recipe({ sharpenVersion: 2, masks: [sharpLayer] } as Partial<Adjustments>);
    const chosen = chosenSettings(source, ['masks'], ['detail']);
    expect(chosen.sharpenVersion).toBe(2);
    // Nothing chosen that sharpens: no response either.
    expect('sharpenVersion' in chosenSettings(source, group('tone').keys, [])).toBe(false);
  });

  it('carries the tone response a chosen layer’s Shadows follow, 1 from a source from before it', () => {
    const shadows = { ...layer('lift'), adjustments: { ...layer('lift').adjustments, shadows: 100 } };
    const current = recipe({ toneVersion: 2, masks: [shadows] } as Partial<Adjustments>);
    expect(chosenSettings(current, ['masks'], ['lift']).toneVersion).toBe(2);
    const legacy = recipe({ masks: [shadows] } as Partial<Adjustments>);
    delete (legacy as Partial<Adjustments>).toneVersion;
    expect(chosenSettings(legacy, ['masks'], ['lift']).toneVersion).toBe(1);
    // A layer without tone values carries none.
    expect('toneVersion' in chosenSettings(current, ['masks'], [])).toBe(false);
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
