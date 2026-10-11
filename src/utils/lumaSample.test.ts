import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { LUMA_CLICK_WIDTH, LUMA_EDGE_SNAP, lumaRangeFromSample, sampleLumaRange, toPercentile, toTone } from './lumaSample';
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER } from './adjustments';
import { createSubMask } from './maskUtils';
import { Mask } from '../components/panel/right/Masks';
import { useEditorStore } from '../store/useEditorStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

describe("the luma range a sample sets (Lightroom's rule, in percentiles)", () => {
  // A photo whose tones are spread evenly: percentiles equal tones.
  const even = Array.from({ length: 101 }, (_, i) => i);
  // A darker photo: the tone at percentile p is 100 p², so its middle
  // percentile is the tone 25.
  const dark = even.map((i) => (i * i) / 100);
  const at = (median: number, levels = even, low = median, high = median) => ({ low, median, high, levels });

  it('converts between tones and percentiles', () => {
    expect(toPercentile(dark, 25)).toBeCloseTo(0.5, 6);
    expect(toTone(dark, 0.5)).toBeCloseTo(25, 6);
    expect(toPercentile(dark, 0)).toBe(0);
    expect(toPercentile(dark, 100)).toBe(1);
    // A tone many pixels share gets the middle of their percentiles.
    const flat = [...Array(50).fill(10), ...Array.from({ length: 51 }, (_, i) => 10 + i)];
    expect(toPercentile(flat, 10)).toBeCloseTo(0.25, 6);
  });

  it('sets 20 percentiles around a clicked tone, with a 25-percentile falloff', () => {
    expect(LUMA_CLICK_WIDTH).toBe(0.2);
    expect(lumaRangeFromSample(at(50), false)).toEqual({ rangeLow: 40, rangeHigh: 60, falloff: 25 });
    // On the darker photo the same rule gives the tones of percentiles 40–60.
    expect(lumaRangeFromSample(at(25, dark), false)).toEqual({
      rangeLow: 16,
      rangeHigh: 36,
      falloff: Math.round(((16 - 2.25) + (72.25 - 36)) / 2),
    });
  });

  it('takes edges near black or white all the way', () => {
    expect(LUMA_EDGE_SNAP).toBe(0.15);
    expect(lumaRangeFromSample(at(20), false)).toEqual({ rangeLow: 0, rangeHigh: 30, falloff: 25 });
    expect(lumaRangeFromSample(at(80), false)).toEqual({ rangeLow: 70, rangeHigh: 100, falloff: 25 });
    expect(lumaRangeFromSample(at(26), false).rangeLow).toBe(16);
    expect(lumaRangeFromSample(at(50, even, 5, 90), true)).toEqual({ rangeLow: 0, rangeHigh: 100, falloff: 20 });
  });

  it("keeps the current range's width for a click, as Lightroom does", () => {
    const current = { rangeLow: 30, rangeHigh: 70, falloff: 10 };
    // The falloff above 80 reaches white after 20.
    expect(lumaRangeFromSample(at(60), false, current)).toEqual({ rangeLow: 40, rangeHigh: 80, falloff: 23 });
    // Snapped at white: the range is now 0.4 wide below it.
    expect(lumaRangeFromSample(at(75), false, current)).toEqual({ rangeLow: 55, rangeHigh: 100, falloff: 25 });
    // The full range counts as new.
    expect(lumaRangeFromSample(at(60), false, { rangeLow: 0, rangeHigh: 100 })).toMatchObject({ rangeLow: 50, rangeHigh: 70 });
    // An imported Lightroom range keeps its width too.
    expect(
      lumaRangeFromSample(at(50), false, { rangeLow: 0, rangeHigh: 100, lrRange: [0.1, 0.3, 0.6, 0.8] }),
    ).toMatchObject({ rangeLow: 35, rangeHigh: 65 });
  });

  it("takes an area's darkest to brightest tones", () => {
    expect(lumaRangeFromSample(at(42, even, 31.4, 58.2), true)).toEqual({ rangeLow: 31, rangeHigh: 59, falloff: 25 });
    expect(lumaRangeFromSample(at(42, dark, 16, 36), true)).toMatchObject({ rangeLow: 16, rangeHigh: 36 });
  });

  it('widens the range to take the sample in with Shift', () => {
    expect(lumaRangeFromSample(at(42, even, 31.4, 58.2), true, { rangeLow: 60, rangeHigh: 70 }, true)).toEqual({
      rangeLow: 31,
      rangeHigh: 70,
      falloff: 25,
    });
    // Swapped bounds count as the same range.
    expect(
      lumaRangeFromSample(at(42, even, 31.4, 58.2), true, { rangeLow: 50, rangeHigh: 20 }, true),
    ).toMatchObject({ rangeLow: 20, rangeHigh: 59 });
  });

  it('works without levels (tones as percentiles) and stays within black and white', () => {
    expect(lumaRangeFromSample({ low: 40, median: 50, high: 60 }, false)).toMatchObject({ rangeLow: 40, rangeHigh: 60 });
    expect(lumaRangeFromSample({ low: 0, median: 1, high: 2 }, false)).toMatchObject({ rangeLow: 0, rangeHigh: 11 });
    expect(lumaRangeFromSample({ low: 98, median: 99.5, high: 100 }, false)).toMatchObject({ rangeLow: 89, rangeHigh: 100 });
  });
});

describe('sampling tones on the photo', () => {
  const size = { width: 1000, height: 800 };
  const range = () => useEditorStore.getState().adjustments.masks[0].subMasks[1].parameters;

  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    const luma = createSubMask(Mask.LumaRange, size);
    luma.parameters = { ...luma.parameters, rangeLow: 70, rangeHigh: 90, lrRange: undefined };
    useEditorStore.setState({
      selectedImage: { path: '/a.raf', isReady: true, ...size } as never,
      adjustments: {
        ...INITIAL_ADJUSTMENTS,
        masks: [
          {
            ...structuredClone(INITIAL_MASK_CONTAINER),
            id: 'l',
            subMasks: [createSubMask(Mask.All, size), luma],
          },
        ],
      },
    });
  });

  it('asks the backend for the tones under a click and sets the range', async () => {
    vi.mocked(invoke).mockResolvedValue({ low: 25, median: 30, high: 35 });
    expect(await sampleLumaRange('l', { x: 10, y: 20 }, false)).toBe(true);
    expect(invoke).toHaveBeenCalledWith(
      'sample_luma_range',
      expect.objectContaining({ x: 10, y: 20, width: 0, height: 0, jsAdjustments: expect.any(Object) }),
    );
    // The current range 70–90 is 20 percentiles wide; it moves to the tone.
    expect(range()).toMatchObject({ rangeLow: 20, rangeHigh: 40, falloff: 23 });
  });

  it('samples an area, adds with Shift and replaces an imported Lightroom range', async () => {
    vi.mocked(invoke).mockResolvedValue({ low: 30, median: 35, high: 40 });
    await sampleLumaRange('l', { x: 0, y: 0, width: 50, height: 40 }, true);
    expect(invoke).toHaveBeenCalledWith('sample_luma_range', expect.objectContaining({ width: 50, height: 40 }));
    // 30–40 joined with 70–90; 90 is near white, so the range goes there.
    expect(range()).toMatchObject({ rangeLow: 30, rangeHigh: 100 });

    useEditorStore.setState((s) => ({
      adjustments: {
        ...s.adjustments,
        masks: [
          {
            ...s.adjustments.masks[0],
            subMasks: s.adjustments.masks[0].subMasks.map((sub) =>
              sub.type === Mask.LumaRange ? { ...sub, parameters: { ...sub.parameters, lrRange: [0, 0.1, 0.2, 0.3] } } : sub,
            ),
          },
        ],
      },
    }));
    await sampleLumaRange('l', { x: 0, y: 0, width: 50, height: 40 }, true);
    expect(range().lrRange).toBeUndefined();
    // An imported range is in percentiles: Shift starts afresh.
    expect(range()).toMatchObject({ rangeLow: 30, rangeHigh: 40, falloff: 25 });
  });

  it('changes nothing outside the photo or after another photo opened', async () => {
    vi.mocked(invoke).mockResolvedValue(null);
    expect(await sampleLumaRange('l', { x: -5, y: -5 }, false)).toBe(false);
    vi.mocked(invoke).mockImplementation(async () => {
      useEditorStore.setState({ selectedImage: { path: '/b.raf', isReady: true, ...size } as never });
      return { low: 1, median: 2, high: 3 };
    });
    expect(await sampleLumaRange('l', { x: 5, y: 5 }, false)).toBe(false);
    expect(range()).toMatchObject({ rangeLow: 70, rangeHigh: 90 });
  });
});
