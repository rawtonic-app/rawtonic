import { describe, expect, it } from 'vitest';
import {
  ADJUSTMENT_GROUPS,
  CLARITY_VERSION,
  GRAIN_VERSION,
  INITIAL_ADJUSTMENTS,
  normalizeLoadedAdjustments,
  type Adjustments,
} from '../../utils/adjustments';
import { pick } from './StudioToolCopy';
import { presetValues, toolPresetToSave } from './toolPresets';

describe('Tool copy of grain', () => {
  it('copies the source grain version, never downgrades it', () => {
    // A source that never used grain: its Size goes with the newest version.
    const fresh = { ...INITIAL_ADJUSTMENTS } as Adjustments;
    expect(pick(fresh, ['grainSize']).grainVersion).toBe(GRAIN_VERSION);
    const v1 = { ...INITIAL_ADJUSTMENTS, grainAmount: 30, grainVersion: 1 } as Adjustments;
    expect(pick(v1, ['grainSize']).grainVersion).toBe(1);
    const v2 = { ...INITIAL_ADJUSTMENTS, grainAmount: 30, grainVersion: 2 } as Adjustments;
    expect(pick(v2, ['grainAmount', 'grainSize']).grainVersion).toBe(2);
    expect('grainVersion' in pick(v2, ['exposure'])).toBe(false);
  });

  it("states film grain's default Colour, so a saved preset or copy sets it", () => {
    // New neutral grain whose Colour was never touched (missing = 0).
    const neutral = { ...INITIAL_ADJUSTMENTS, grainAmount: 40, grainVersion: 3 } as Adjustments;
    const keys = ['grainAmount', 'grainSize', 'grainRoughness', 'grainStructure', 'grainColor'];
    const saved = JSON.parse(JSON.stringify(toolPresetToSave(neutral, keys)));
    expect(saved.grainColor).toBe(0);
    const coloured = { ...INITIAL_ADJUSTMENTS, grainAmount: 40, grainVersion: 3, grainColor: 74 } as Adjustments;
    expect({ ...coloured, ...presetValues(saved) }.grainColor).toBe(0);
    expect(JSON.parse(JSON.stringify(pick(neutral, keys))).grainColor).toBe(0);
    // Older grain has no Colour, and a copy without it leaves the destination's.
    const v2 = { ...INITIAL_ADJUSTMENTS, grainAmount: 30, grainVersion: 2 } as Adjustments;
    expect(JSON.parse(JSON.stringify(pick(v2, keys))).grainColor).toBeUndefined();
    expect(pick(neutral, ['grainAmount'])).not.toHaveProperty('grainColor');
  });
});

describe('Tool copy of Clarity', () => {
  it('carries the response of an edit made before Clarity version 2', () => {
    // An older edit with Clarity and no key: its copy states version 1, so a
    // photo already at the current version renders the amount as the source.
    const old = normalizeLoadedAdjustments({ clarity: 40 } as unknown as Adjustments, true);
    expect(JSON.parse(JSON.stringify(pick(old, ['clarity'])))).toEqual({ clarity: 40, clarityVersion: 1 });
    const destination = { ...INITIAL_ADJUSTMENTS, clarityVersion: CLARITY_VERSION } as Adjustments;
    expect({ ...destination, ...pick(old, ['clarity']) }.clarityVersion).toBe(1);
    const current = normalizeLoadedAdjustments({ clarity: 40, clarityVersion: 2 } as unknown as Adjustments, true);
    expect(pick(current, ['clarity']).clarityVersion).toBe(2);
  });
});

describe('Tool copy of LUT', () => {
  it('carries the table and input space with the selected LUT', () => {
    const source = { ...INITIAL_ADJUSTMENTS, lutPath: '/qa/own.cube', lutName: 'Own table', lutSize: 2, lutData: "AAAAAA==", lutIsSceneReferred: true, lutIntensity: 70 } as Adjustments;
    expect(pick(source, ['lutPath', 'lutIntensity'])).toEqual({ lutPath: source.lutPath, lutName: source.lutName, lutSize: 2, lutData: "AAAAAA==", lutIsSceneReferred: true, lutIntensity: 70 });
  });
});

describe('Detail copy groups', () => {
  it('keeps grain settings in the Detail copy, sync and preset group', () => {
    const grain = ADJUSTMENT_GROUPS.details.find(group => group.label === 'modals.copyPaste.groups.grain');
    expect(grain?.keys).toEqual(['grainAmount', 'grainRoughness', 'grainSize', 'grainStructure', 'grainColor', 'grainVersion']);
    expect(ADJUSTMENT_GROUPS.effects.some(group => group.label === 'modals.copyPaste.groups.grain')).toBe(false);
  });
});
