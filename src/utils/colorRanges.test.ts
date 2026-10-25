import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { applyColorPick, pickColorRange } from './colorRanges';
import { INITIAL_ADJUSTMENTS, MAX_LAYER_COLOR_RANGES, type MaskContainer } from './adjustments';
import { useEditorStore } from '../store/useEditorStore';
import { Invokes, type SelectedImage } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const photo: SelectedImage = {
  path: '/a.cr3', isReady: true, isRaw: true, width: 6000, height: 4000, exif: {},
  thumbnailUrl: '', thumbnailSource: 'embedded',
};
const layer = { id: 'L1', name: 'L', adjustments: { colorRanges: [] }, subMasks: [] } as unknown as MaskContainer;

beforeEach(() => {
  useEditorStore.setState({
    selectedImage: photo,
    adjustments: { ...INITIAL_ADJUSTMENTS, masks: [layer] },
    colorPickerTarget: 'new',
    colorPickerLayer: null,
    activeColorRangeId: null,
  });
  vi.mocked(invoke).mockReset();
});

it('adds a range for the sampled display colour and ends the picker', async () => {
  vi.mocked(invoke).mockResolvedValue([255, 0, 0]);
  expect(await pickColorRange('/a.cr3', 'new', 0.5, 0.5)).toBe(true);
  expect(invoke).toHaveBeenCalledWith(Invokes.SamplePreviewColor, { path: '/a.cr3', normalizedX: 0.5, normalizedY: 0.5 });
  const state = useEditorStore.getState();
  const ranges = state.adjustments.colorRanges!;
  expect(ranges).toHaveLength(1);
  expect(ranges[0].hue).toBeCloseTo(29.23, 1);
  expect(state.colorPickerTarget).toBeNull();
  expect(state.activeColorRangeId).toBe(ranges[0].id);
});

it('re-samples an existing range in place', async () => {
  vi.mocked(invoke).mockResolvedValue([255, 0, 0]);
  await pickColorRange('/a.cr3', 'new', 0.5, 0.5);
  const id = useEditorStore.getState().adjustments.colorRanges![0].id;
  vi.mocked(invoke).mockResolvedValue([0, 0, 255]);
  await pickColorRange('/a.cr3', id, 0.1, 0.1);
  const ranges = useEditorStore.getState().adjustments.colorRanges!;
  expect(ranges).toHaveLength(1);
  expect(ranges[0].hue).toBeCloseTo(264.05, 1);
});

it('edits the layer the picker was started on, up to its limit', async () => {
  useEditorStore.setState({ colorPickerLayer: 'L1' });
  vi.mocked(invoke).mockResolvedValue([20, 200, 40]);
  for (let i = 0; i < MAX_LAYER_COLOR_RANGES + 2; i++) {
    useEditorStore.setState({ colorPickerLayer: 'L1' });
    await pickColorRange('/a.cr3', 'new', 0.5, 0.5);
  }
  const state = useEditorStore.getState();
  expect(state.adjustments.colorRanges ?? []).toHaveLength(0);
  expect(state.adjustments.masks[0].adjustments.colorRanges).toHaveLength(MAX_LAYER_COLOR_RANGES);
});

it('ignores samples for another photograph', async () => {
  vi.mocked(invoke).mockResolvedValue([255, 0, 0]);
  expect(await pickColorRange('/other.cr3', 'new', 0.5, 0.5)).toBe(false);
  expect(useEditorStore.getState().adjustments.colorRanges ?? []).toHaveLength(0);
});

it('picks skin into one skin tone range and moves it on the next pick', async () => {
  vi.mocked(invoke).mockResolvedValue([200, 150, 120]);
  useEditorStore.setState({ colorPickerTarget: 'skin' });
  expect(await pickColorRange('/a.cr3', 'skin', 0.5, 0.5)).toBe(true);
  const [skin] = useEditorStore.getState().adjustments.colorRanges!;
  expect(skin.kind).toBe('skin');
  expect(useEditorStore.getState().activeColorRangeId).toBe(skin.id);
  vi.mocked(invoke).mockResolvedValue([120, 80, 60]);
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
