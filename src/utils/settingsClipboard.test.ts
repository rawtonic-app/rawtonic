import { describe, expect, it } from 'vitest';
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER, initialAdjustmentsFor, withGrainStructure, type Adjustments, type MaskContainer } from './adjustments';
import { Mask } from '../components/panel/right/Masks';
import {
  RESPONSE_KEYS,
  SETTINGS_GROUPS,
  chosenSettings,
  detectionPending,
  newEditResponses,
  pasteInto,
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
    // A People selection with all its parts deselected: nothing to detect.
    expect(detectionPending({ ...sub, type: Mask.AiPeople, parameters: { maskDataBase64: null, detectPending: true, parts: [] } })).toBe(false);
  });

  it('offers no group of processing versions among the tools', () => {
    for (const g of SETTINGS_GROUPS) for (const key of g.keys) expect(RESPONSE_KEYS).not.toContain(key);
    expect(RESPONSE_KEYS).toContain('toneVersion');
    // The camera CA version goes with the camera's lens data.
    expect(RESPONSE_KEYS).not.toContain('cameraCaVersion');
  });

  it('never takes Lens Correction, also when its settings are remembered as chosen', () => {
    const lensKeys = ['lensCorrectionMode', 'lensMaker', 'lensModel', 'lensDistortionAmount', 'diffractionCorrection',
      'autoChromaticAberration', 'cameraCaVersion'];
    for (const g of SETTINGS_GROUPS) for (const key of lensKeys) expect(g.keys).not.toContain(key);
    const source = { ...initialAdjustmentsFor(true), exposure: 1, lensCorrectionMode: 'manual', lensMaker: 'M',
      lensModel: '50mm', cameraCaVersion: 2 } as unknown as Adjustments;
    expect(chosenSettings(source, ['exposure', ...lensKeys], [])).toEqual({ exposure: 1 });
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
    // A neutral Luminance curve (a point added and deleted) is no change.
    const neutral = [
      { x: 0, y: 0 },
      { x: 255, y: 255 },
    ];
    const cleared = recipe({ curves: { ...INITIAL_ADJUSTMENTS.curves, luminance: neutral } });
    expect(groupModified(group('curves'), cleared, true)).toBe(false);
    const lifted = recipe({
      curves: { ...INITIAL_ADJUSTMENTS.curves, luminance: [neutral[0], { x: 128, y: 150 }, neutral[1]] },
    });
    expect(groupModified(group('curves'), lifted, true)).toBe(true);
  });


  // The same cases as the backend's `masks::paste_tests` and `edits` tests.
  it('pastes onto a recipe as the backend does', () => {
    const own = recipe({ exposure: 0.1, whiteBalance: 'custom', masks: [layer('dodge'), { ...layer('sky'), name: 'old sky' }] } as Partial<Adjustments>);
    const pasted = { exposure: 1, whiteBalance: null, masks: [layer('sky'), layer('vignette')] } as unknown as Partial<Adjustments>;
    const merged = pasteInto(own, pasted, { samePhoto: false, replaceLayers: false })!;
    expect(merged.exposure).toBe(1);
    expect('whiteBalance' in merged).toBe(false);
    expect(merged.masks.map((l) => [l.id, l.name])).toEqual([['dodge', 'dodge'], ['sky', 'sky'], ['vignette', 'vignette']]);
    expect(pasteInto(own, pasted, { samePhoto: false, replaceLayers: true })!.masks.map((l) => l.id)).toEqual(['sky', 'vignette']);
  });

  it('never pastes another photo’s AI bitmap; a virtual copy of the same keeps it', () => {
    const pasted = { masks: [layer('sky', 'ai-sky', { maskDataBase64: 'data:source', grow: 10 }), layer('people', 'ai-people', { parts: [] })] } as unknown as Partial<Adjustments>;
    const onOther = pasteInto(recipe({}), pasted, { samePhoto: false, replaceLayers: false })!;
    expect(onOther.masks[0].subMasks[0].parameters).toEqual({ maskDataBase64: null, grow: 10, detectPending: true });
    expect(onOther.masks[0].subMasks[0].id).not.toBe('sky-c');
    expect(onOther.masks[1].subMasks[0].id).toBe('people-c');
    const onCopy = pasteInto(recipe({}), pasted, { samePhoto: true, replaceLayers: false })!;
    expect(onCopy.masks[0].subMasks[0].parameters).toMatchObject({ maskDataBase64: 'data:source' });
  });

  it('starts a never-edited recipe as a new edit and refuses more layers than applied', () => {
    const fresh = pasteInto(null, { exposure: 1 } as Partial<Adjustments>, { samePhoto: false, replaceLayers: false, newEdit: { toneVersion: 2 } })!;
    expect(fresh).toMatchObject({ exposure: 1, toneVersion: 2, renderVersion: expect.any(Number) });
    const many = Array.from({ length: 33 }, (_, i) => layer(`l${i}`));
    expect(pasteInto(recipe({}), { masks: many } as unknown as Partial<Adjustments>, { samePhoto: false, replaceLayers: false })).toBeNull();
    expect(pasteInto(recipe({}), { masks: many.slice(0, 32) } as unknown as Partial<Adjustments>, { samePhoto: false, replaceLayers: false })).not.toBeNull();
  });
});

describe('Copy/Sync of grain', () => {
  it('leaves the destination its grain version unless the processing versions are chosen', () => {
    const source = { ...INITIAL_ADJUSTMENTS, grainAmount: 30, grainVersion: 2 } as Adjustments;
    const grain = SETTINGS_GROUPS.find((g) => g.label === 'modals.copyPaste.groups.grain')!;
    expect(grain.keys).not.toContain('grainVersion');
    const applied = withGrainStructure(chosenSettings(source, grain.keys, []));
    expect('grainVersion' in applied).toBe(false);
    expect(chosenSettings(source, [...grain.keys, 'grainVersion'], []).grainVersion).toBe(2);
    // A never-edited destination gets a new edit's grain; an edited one keeps its own.
    const fresh = pasteInto(null, applied, { samePhoto: false, replaceLayers: false, newEdit: newEditResponses().raw })!;
    expect(fresh.grainVersion).toBe(2);
    const old = { ...INITIAL_ADJUSTMENTS, grainAmount: 10, grainVersion: 1 } as Adjustments;
    expect(pasteInto(old, applied, { samePhoto: false, replaceLayers: false })!.grainVersion).toBe(1);
  });
});
