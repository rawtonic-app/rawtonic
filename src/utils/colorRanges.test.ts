import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  applyColorPick,
  binned,
  eyedropperBlock,
  invalidatePendingColorPicks,
  pickColorRange,
  rangeInputFor,
  rangePickOf,
  TARGET_PICK,
  targetPickFor
} from './colorRanges';
import {
  applyColorRanges,
  deltaE00,
  labToLinear,
  linearToLab,
  linearToOklab,
  meanOklab,
  renderedLab,
  srgb8ToLinear,
  linearToSrgb8,
  oklabToLinear,
  rangeCentre,
  solveColorTarget
} from './colorTarget';
import { INITIAL_ADJUSTMENTS, MAX_COLOR_RANGES, type MaskContainer } from './adjustments';
import { useEditorStore } from '../store/useEditorStore';
import { Invokes, type SelectedImage } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const photo: SelectedImage = {
  path: '/a.cr3',
  isReady: true,
  isRaw: true,
  width: 6000,
  height: 4000,
  exif: {},
  thumbnailUrl: '',
  thumbnailSource: 'embedded'
};
/** The renderer showing `shown` (display sRGB) wherever it is sampled: the
 * eyedropper's patch (its mean, on a 100 × 100 frame), else the value as is. */
const shows = (shown: unknown) =>
  vi
    .mocked(invoke)
    .mockImplementation(async (command: string) =>
      command === Invokes.SamplePreviewPatch ? shown && { mean: shown, frame: [100, 100] } : shown
    );

const layer = { id: 'L1', name: 'L', adjustments: { colorRanges: [] }, subMasks: [] } as unknown as MaskContainer;

beforeEach(() => {
  useEditorStore.setState({
    selectedImage: photo,
    adjustments: { ...INITIAL_ADJUSTMENTS, masks: [layer] },
    colorPickerTarget: 'new',
    colorPickerLayer: null,
    activeColorRangeId: null,
    colorTarget: null,
    colorPicksThroughRanges: [],
    colorRangeInputs: {}
  });
  vi.mocked(invoke).mockReset();
});

it('adds a range for the sampled display colour and ends the picker', async () => {
  shows([255, 0, 0]);
  expect(await pickColorRange('/a.cr3', 'new', 0.5, 0.5)).toBe(true);
  expect(invoke).toHaveBeenCalledWith(Invokes.SamplePreviewPatch, {
    path: '/a.cr3',
    normalizedX: 0.5,
    normalizedY: 0.5
  });
  const state = useEditorStore.getState();
  const ranges = state.adjustments.colorRanges!;
  expect(ranges).toHaveLength(1);
  expect(ranges[0].hue).toBeCloseTo(29.23, 1);
  expect(state.colorPickerTarget).toBeNull();
  expect(state.activeColorRangeId).toBe(ranges[0].id);
});

it('re-samples an existing range in place', async () => {
  shows([255, 0, 0]);
  await pickColorRange('/a.cr3', 'new', 0.5, 0.5);
  const id = useEditorStore.getState().adjustments.colorRanges![0].id;
  shows([0, 0, 255]);
  await pickColorRange('/a.cr3', id, 0.1, 0.1);
  const ranges = useEditorStore.getState().adjustments.colorRanges!;
  expect(ranges).toHaveLength(1);
  expect(ranges[0].hue).toBeCloseTo(264.05, 1);
});

it('edits the layer the picker was started on, up to its limit', async () => {
  useEditorStore.setState({ colorPickerLayer: 'L1' });
  shows([20, 200, 40]);
  for (let i = 0; i < MAX_COLOR_RANGES + 2; i++) {
    useEditorStore.setState({ colorPickerLayer: 'L1' });
    await pickColorRange('/a.cr3', 'new', 0.5, 0.5);
  }
  const state = useEditorStore.getState();
  expect(state.adjustments.colorRanges ?? []).toHaveLength(0);
  expect(state.adjustments.masks[0].adjustments.colorRanges).toHaveLength(MAX_COLOR_RANGES);
  expect(MAX_COLOR_RANGES).toBe(8);
});

it('ignores samples for another photograph', async () => {
  shows([255, 0, 0]);
  expect(await pickColorRange('/other.cr3', 'new', 0.5, 0.5)).toBe(false);
  expect(useEditorStore.getState().adjustments.colorRanges ?? []).toHaveLength(0);
});

it('picks skin into one skin tone range and moves it on the next pick', async () => {
  shows([200, 150, 120]);
  useEditorStore.setState({ colorPickerTarget: 'skin' });
  expect(await pickColorRange('/a.cr3', 'skin', 0.5, 0.5)).toBe(true);
  const [skin] = useEditorStore.getState().adjustments.colorRanges!;
  expect(skin.kind).toBe('skin');
  expect(useEditorStore.getState().activeColorRangeId).toBe(skin.id);
  shows([120, 80, 60]);
  await pickColorRange('/a.cr3', 'skin', 0.2, 0.2);
  const ranges = useEditorStore.getState().adjustments.colorRanges!;
  expect(ranges).toHaveLength(1);
  expect(ranges[0].id).toBe(skin.id);
  expect(ranges[0].lightness).toBeLessThan(skin.lightness);
});

it('adds no range beyond the limit', () => {
  const full = Array.from({ length: 4 }, () => applyColorPick([], 'new', 4, 10, 0.1, 0.5).ranges[0]);
  expect(applyColorPick(full, 'skin', 4, 50, 0.06, 0.6)).toEqual({ ranges: full, id: null });
});

it('drops a sample whose pick was finished meanwhile (e.g. on the range wheel)', async () => {
  let resolve!: (patch: { mean: [number, number, number]; frame: [number, number] }) => void;
  vi.mocked(invoke).mockReturnValue(new Promise((r) => (resolve = r)));
  useEditorStore.setState({ colorPickerTarget: 'skin', colorPickerLayer: 'L1' });
  const pending = pickColorRange('/a.cr3', 'skin', 0.5, 0.5);
  useEditorStore.setState({ colorPickerTarget: null, colorPickerLayer: null });
  resolve({ mean: [200, 150, 120], frame: [100, 100] });
  expect(await pending).toBe(false);
  const state = useEditorStore.getState();
  expect(state.adjustments.colorRanges ?? []).toHaveLength(0);
  expect(state.adjustments.masks[0].adjustments.colorRanges).toHaveLength(0);
});

it('drops a sample of a pick finished and re-armed for the same range meanwhile', async () => {
  let resolve!: (patch: { mean: [number, number, number]; frame: [number, number] }) => void;
  vi.mocked(invoke).mockReturnValue(new Promise((r) => (resolve = r)));
  useEditorStore.setState({ colorPickerTarget: 'skin' });
  const pending = pickColorRange('/a.cr3', 'skin', 0.5, 0.5);
  // Finished on the wheel, then armed again for the same target.
  invalidatePendingColorPicks();
  useEditorStore.setState({ colorPickerTarget: 'skin' });
  resolve({ mean: [200, 150, 120], frame: [100, 100] });
  expect(await pending).toBe(false);
  expect(useEditorStore.getState().adjustments.colorRanges ?? []).toHaveLength(0);
  expect(useEditorStore.getState().colorPickerTarget).toBe('skin');
});

describe('target colour picks', () => {
  const redRange = () => {
    useEditorStore.setState({ colorPickerTarget: 'new' });
    return pickColorRange('/a.cr3', 'new', 0.5, 0.5);
  };

  it('moves the armed range onto the sampled target in one edit and keeps the target', async () => {
    shows([200, 60, 50]);
    await redRange();
    const before = useEditorStore.getState().adjustments.colorRanges![0];
    shows([200, 20, 40]);
    const target = targetPickFor(before.id);
    useEditorStore.setState({ colorPickerTarget: target });
    let updates = 0;
    const stop = useEditorStore.subscribe((s, p) => {
      if (s.adjustments !== p.adjustments) updates += 1;
    });
    expect(await pickColorRange('/a.cr3', target, 0.2, 0.2)).toBe(true);
    stop();
    expect(updates).toBe(1);
    const state = useEditorStore.getState();
    const [after] = state.adjustments.colorRanges!;
    // Only the shifts change; the range keeps its sampled colour and shape.
    expect({ ...after, hueShift: 0, saturation: 0, lightnessShift: 0 }).toEqual(before);
    expect(after.hueShift).toBeLessThan(0);
    const reached = solveColorTarget([after], after.id, state.colorTarget!.lab)!;
    expect(reached.deltaE).toBeLessThan(0.5);
    expect(state.colorTarget).toMatchObject({
      sampledIn: '/a.cr3',
      appliedTo: { photo: '/a.cr3', layer: null, rangeId: before.id }
    });
    expect(state.colorPickerTarget).toBeNull();
  });

  it('a range picked again on its own shifted colour keeps selecting the colour before the shift', async () => {
    shows([190, 100, 120]);
    await redRange();
    const picked = useEditorStore.getState().adjustments.colorRanges![0];
    const shifted = { ...picked, hueShift: 100 };
    useEditorStore.setState({ adjustments: { ...useEditorStore.getState().adjustments, colorRanges: [shifted] } });
    // The photo now shows the shifted colour there.
    const shown = linearToSrgb8(oklabToLinear(applyColorRanges([shifted], rangeCentre(picked))));
    shows(shown);
    useEditorStore.setState({ colorPickerTarget: picked.id });
    await pickColorRange('/a.cr3', picked.id, 0.5, 0.5);
    const [again] = useEditorStore.getState().adjustments.colorRanges!;
    expect(again.hue).toBeCloseTo(picked.hue, 4);
    expect(again.chroma).toBeCloseTo(picked.chroma, 6);
    expect(again.lightness).toBeCloseTo(picked.lightness, 6);
    expect(again.hueShift).toBe(100);
  });

  it('marks a range picked again through its own strong change as uncertain, and clears it on a clean pick', async () => {
    shows([180, 85, 55]);
    await redRange();
    const picked = useEditorStore.getState().adjustments.colorRanges![0];
    const drained = { ...picked, saturation: -90 };
    useEditorStore.setState({ adjustments: { ...useEditorStore.getState().adjustments, colorRanges: [drained] } });
    const shown = linearToSrgb8(oklabToLinear(applyColorRanges([drained], rangeCentre(picked)))).map(Math.round);
    shows(shown);
    useEditorStore.setState({ colorPickerTarget: picked.id });
    await pickColorRange('/a.cr3', picked.id, 0.5, 0.5);
    expect(useEditorStore.getState().colorPicksThroughRanges).toEqual([`/a.cr3||${picked.id}`]);
    // Picked again on a colour the range does not touch: certain again.
    shows([40, 160, 60]);
    useEditorStore.setState({ colorPickerTarget: picked.id });
    await pickColorRange('/a.cr3', picked.id, 0.5, 0.5);
    expect(useEditorStore.getState().colorPicksThroughRanges).toEqual([]);
  });

  it('only keeps a target sampled without a range (for another photo)', async () => {
    shows([200, 60, 50]);
    await redRange();
    const recipe = useEditorStore.getState().adjustments;
    shows([10, 120, 200]);
    useEditorStore.setState({ colorPickerTarget: TARGET_PICK });
    expect(await pickColorRange('/a.cr3', TARGET_PICK, 0.3, 0.3)).toBe(true);
    const state = useEditorStore.getState();
    expect(state.adjustments).toBe(recipe);
    expect(state.colorTarget).toMatchObject({ sampledIn: '/a.cr3', appliedTo: null });
    expect(linearToSrgb8(labToLinear(state.colorTarget!.lab)).map(Math.round)).toEqual([10, 120, 200]);
  });

  it('solves a layer range with the layer’s own ranges', async () => {
    useEditorStore.setState({ colorPickerLayer: 'L1' });
    shows([60, 160, 80]);
    await pickColorRange('/a.cr3', 'new', 0.5, 0.5);
    const id = useEditorStore.getState().adjustments.masks[0].adjustments.colorRanges![0].id;
    shows([40, 170, 120]);
    useEditorStore.setState({ colorPickerTarget: targetPickFor(id), colorPickerLayer: 'L1' });
    await pickColorRange('/a.cr3', targetPickFor(id), 0.5, 0.5);
    const state = useEditorStore.getState();
    const [moved] = state.adjustments.masks[0].adjustments.colorRanges!;
    expect(moved.hueShift).toBeGreaterThan(0);
    expect(state.adjustments.colorRanges ?? []).toHaveLength(0);
    expect(state.colorTarget!.appliedTo).toEqual({ photo: '/a.cr3', layer: 'L1', rangeId: id });
  });
});

describe('the renderer probe at a pick', () => {
  // The colour entering the colour editor: a blue beyond sRGB the display clips.
  const entering: [number, number, number][] = [
    [-0.05, 0.05, 0.6],
    [-0.04, 0.06, 0.58]
  ];
  const probeWith = (influence: number, shown: [number, number, number] = [0, 40, 255]) =>
    vi
      .mocked(invoke)
      .mockImplementation(async (command: string) =>
        command === Invokes.SampleColorRangeInput
          ? { pixels: entering, influence: entering.map(() => influence) }
          : command === Invokes.SamplePreviewPatch
            ? { mean: shown, frame: [100, 100] }
            : shown
      );

  it('sets the range to the unclipped colour entering the colour editor, also through a strong change', async () => {
    probeWith(1);
    expect(await pickColorRange('/a.cr3', 'new', 0.4, 0.6)).toBe(true);
    expect(invoke).toHaveBeenCalledWith(Invokes.SampleColorRangeInput, {
      path: '/a.cr3',
      normalizedX: 0.4,
      normalizedY: 0.6,
      // The eyedropper's 11 × 11 block on the 100 × 100 frame around (40, 60).
      block: [0.35, 0.55, 0.11, 0.11],
      adjustments: expect.anything(),
      layer: null
    });
    const picked = useEditorStore.getState().adjustments.colorRanges![0];
    const want = meanOklab(entering.map((p) => linearToOklab(p)));
    expect(picked.lightness).toBeCloseTo(want[0], 9);
    expect(picked.chroma).toBeCloseTo(Math.hypot(want[1], want[2]), 9);
    // Picked again through Saturation −90: the probe is before the ranges,
    // so the range keeps its colour exactly and nothing is uncertain.
    const drained = { ...picked, saturation: -90 };
    useEditorStore.setState({ adjustments: { ...useEditorStore.getState().adjustments, colorRanges: [drained] } });
    probeWith(1, [90, 90, 100]);
    useEditorStore.setState({ colorPickerTarget: picked.id });
    await pickColorRange('/a.cr3', picked.id, 0.4, 0.6);
    const state = useEditorStore.getState();
    const [again] = state.adjustments.colorRanges!;
    expect([again.hue, again.chroma, again.lightness]).toEqual([picked.hue, picked.chroma, picked.lightness]);
    expect(again.saturation).toBe(-90);
    expect(state.colorPicksThroughRanges).toEqual([]);
    expect(state.colorRangeInputs[`/a.cr3||${picked.id}`].point).toEqual([0.4, 0.6]);
  });

  it('forgets the pick when the photo is cropped, rotated or flipped (its point lies on other pixels)', async () => {
    probeWith(1);
    await pickColorRange('/a.cr3', 'new', 0.4, 0.6);
    const state = useEditorStore.getState();
    const range = state.adjustments.colorRanges![0];
    expect(rangePickOf('/a.cr3', null, range)).not.toBeNull();
    useEditorStore.setState({ adjustments: { ...state.adjustments, flipHorizontal: true } });
    expect(rangePickOf('/a.cr3', null, range)).toBeNull();
    useEditorStore.setState({
      adjustments: { ...state.adjustments, crop: { x: 10, y: 10, width: 50, height: 50, unit: '%' } as never }
    });
    expect(rangePickOf('/a.cr3', null, range)).toBeNull();
    useEditorStore.setState({ adjustments: state.adjustments });
    expect(rangePickOf('/a.cr3', null, range)).not.toBeNull();
  });

  it('drops the sample but keeps the point while the renderer has none there (a hidden layer)', async () => {
    probeWith(0.5);
    useEditorStore.setState({ colorPickerTarget: 'new', colorPickerLayer: 'L1' });
    await pickColorRange('/a.cr3', 'new', 0.4, 0.6);
    const range = useEditorStore.getState().adjustments.masks[0].adjustments.colorRanges![0];
    expect(rangePickOf('/a.cr3', 'L1', range)!.input).not.toBeNull();
    // Hidden: no sample, the point stays.
    shows(null);
    expect(await rangeInputFor('/a.cr3', 'L1', range)).toBeNull();
    expect(rangePickOf('/a.cr3', 'L1', range)).toMatchObject({ point: [0.4, 0.6], input: null });
    // Shown again: the next sample at the same point.
    probeWith(0.25);
    expect((await rangeInputFor('/a.cr3', 'L1', range))!.influence[0]).toBe(0.25);
    expect(invoke).toHaveBeenLastCalledWith(
      Invokes.SampleColorRangeInput,
      expect.objectContaining({ normalizedX: 0.4, normalizedY: 0.6, layer: 'L1' })
    );
    expect(rangePickOf('/a.cr3', 'L1', range)!.input!.influence[0]).toBe(0.25);
  });

  it('solves a target on a feathered layer at the mask influence of the pick', async () => {
    probeWith(0.5);
    useEditorStore.setState({ colorPickerTarget: 'new', colorPickerLayer: 'L1' });
    await pickColorRange('/a.cr3', 'new', 0.4, 0.6);
    const ranges = useEditorStore.getState().adjustments.masks[0].adjustments.colorRanges!;
    const range = ranges[0];
    // The target picked elsewhere: a slightly turned blue.
    const target: [number, number, number] = [40, 30, 230];
    probeWith(0.5, target);
    useEditorStore.setState({ colorPickerTarget: targetPickFor(range.id), colorPickerLayer: 'L1' });
    await pickColorRange('/a.cr3', targetPickFor(range.id), 0.1, 0.1);
    // The range's pick point was sampled again for the influence.
    expect(invoke).toHaveBeenCalledWith(
      Invokes.SampleColorRangeInput,
      expect.objectContaining({ normalizedX: 0.4, layer: 'L1' })
    );
    const [solved] = useEditorStore.getState().adjustments.masks[0].adjustments.colorRanges!;
    const input = { pixels: entering.map((p) => linearToOklab(p)), influence: [0.5, 0.5] };
    const lab = linearToLab(srgb8ToLinear(target));
    expect(deltaE00(lab, renderedLab([solved], input))).toBeLessThan(1);
    expect(deltaE00(lab, renderedLab([solved], { ...input, influence: [1, 1] }))).toBeGreaterThan(1);
  });
});

describe('the pick’s block at full resolution', () => {
  it('is the eyedropper’s 11 × 11 block on the shown frame, cut at the edges', () => {
    expect(eyedropperBlock(0.5, 0.25, [1000, 500])).toEqual([0.495, 0.24, 0.011, 0.022]);
    expect(eyedropperBlock(0, 0, [1000, 500])).toEqual([0, 0, 0.006, 0.012]);
    expect(eyedropperBlock(0.5, 0.5, [0, 0])).toBeNull();
  });

  it('gives the solve 11 × 11 cell means, so a fine stripe cannot stand for the block', () => {
    // 44 × 44 pixels: every fourth column a red stripe, the rest green.
    const red = linearToOklab(srgb8ToLinear([220, 30, 40]));
    const green = linearToOklab(srgb8ToLinear([40, 160, 60]));
    const pixels = Array.from({ length: 44 * 44 }, (_, i) => (i % 4 === 0 ? red : green));
    const cells = binned(
      pixels,
      pixels.map(() => 1),
      [44, 44]
    );
    expect(cells.pixels).toHaveLength(121);
    const whole = meanOklab(pixels);
    for (const cell of cells.pixels) {
      // Every cell holds the stripe and its ground, as the whole block does.
      expect(Math.hypot(...cell.map((v, k) => v - whole[k]))).toBeLessThan(1e-6);
    }
    // Small blocks (the preview's own) stay as they are.
    expect(
      binned(
        pixels.slice(0, 121),
        pixels.slice(0, 121).map(() => 1),
        [11, 11]
      ).pixels
    ).toHaveLength(121);
  });
});
