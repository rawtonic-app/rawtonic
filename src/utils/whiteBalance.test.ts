import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { autoWhiteBalance, neutralSliders, neutralWhiteBalance, whiteBalanceForCurrentPreview } from './whiteBalance';
import { INITIAL_ADJUSTMENTS, newNegative } from './adjustments';
import { useEditorStore } from '../store/useEditorStore';
import { Invokes, type SelectedImage } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const photo = (path = '/first.raf'): SelectedImage => ({
  path, isReady: true, isRaw: true, width: 4000, height: 6000, exif: {},
  thumbnailUrl: 'blob:camera-jpeg', thumbnailSource: 'embedded',
});
const request = () => ({
  image: useEditorStore.getState().selectedImage!,
  adjustments: useEditorStore.getState().adjustments,
  normalizedX: 0.25, normalizedY: 0.75,
  useWgpu: true, finalPreviewUrl: null as string | null,
});

beforeEach(() => {
  useEditorStore.setState({ selectedImage: photo(), adjustments: { ...INITIAL_ADJUSTMENTS } });
  vi.mocked(invoke).mockReset();
});
afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });

// The native sample, and the Kelvin answer for RAWs (null: no calibration).
const native = (rgb: number[] | null, kelvin: { temperature: number; tint: number } | null = null) =>
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === Invokes.SamplePreviewWhiteBalance ? rgb : kelvin);

it('samples the native preview input before adjustments instead of the embedded camera JPEG', async () => {
  native([0.2, 0.2, 0.2]);
  expect(await whiteBalanceForCurrentPreview(request())).toEqual({ temperature: 0, tint: 0 });
  expect(invoke).toHaveBeenCalledWith(Invokes.SamplePreviewWhiteBalance, {
    path: '/first.raf', normalizedX: 0.25, normalizedY: 0.75,
  });
});

it('sets a RAW to the Kelvin and tint that neutralise the sample and clears the relative sliders', async () => {
  native([0.18, 0.2, 0.25], { temperature: 6150, tint: 7 });
  useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, temperature: 40, tint: -20 } });
  expect(await whiteBalanceForCurrentPreview(request())).toEqual({
    whiteBalance: 'custom', wbTemperature: 6150, wbTint: 7, temperature: 0, tint: 0,
  });
  expect(invoke).toHaveBeenCalledWith(Invokes.WhiteBalanceFromSample, {
    imagePath: '/first.raf', rgb: [0.18, 0.2, 0.25],
  });
});

it('keeps relative sliders for images without a camera calibration', async () => {
  native([0.18, 0.2, 0.25]);
  useEditorStore.setState({ selectedImage: { ...photo(), isRaw: false } });
  expect(await whiteBalanceForCurrentPreview(request())).toEqual(neutralSliders([0.18, 0.2, 0.25]));
  expect(invoke).not.toHaveBeenCalledWith(Invokes.WhiteBalanceFromSample, expect.anything());
});

// The shader's white balance gains (apply_white_balance) for slider values.
const balanced = ([r, g, b]: number[], temperature: number, tint: number) => {
  const t = temperature / 25, n = tint / 100;
  return [r * (1 + 0.2 * t) * (1 + 0.25 * n), g * (1 + 0.05 * t) * (1 - 0.25 * n), b * (1 - 0.2 * t) * (1 + 0.25 * n)];
};

it.each([
  [[0.18, 0.2, 0.25]],
  [[0.3, 0.22, 0.15]],
  [[0.1, 0.13, 0.1]],
  [[0.05, 0.04, 0.06]],
])('solves the white balance that makes %j neutral', (rgb) => {
  const result = neutralWhiteBalance(rgb as [number, number, number])!;
  const [r, g, b] = balanced(rgb, result.temperature, result.tint);
  expect(b / r).toBeCloseTo(1, 9);
  expect(g / r).toBeCloseTo(1, 9);
});

it('is absolute: the current white balance and tone do not change the result', async () => {
  native([0.18, 0.2, 0.25]);
  const expected = neutralSliders([0.18, 0.2, 0.25]);
  useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, temperature: 40, tint: -20, contrast: 66 } });
  expect(await whiteBalanceForCurrentPreview(request())).toEqual(expected);
});

it('clamps impossible casts and ignores black samples', () => {
  expect(neutralWhiteBalance([1, 0.01, 0.01])).toEqual({ temperature: -100, tint: expect.any(Number) });
  expect(neutralWhiteBalance([0, 0, 0])).toBeNull();
});

it.each(['other image', 'same image reopened', 'new recipe'])('discards an asynchronous sample after %s', async (change) => {
  let finish!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
  const result = whiteBalanceForCurrentPreview(request());
  if (change === 'new recipe') {
    useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 1 } });
  } else {
    useEditorStore.setState({ selectedImage: photo(change === 'other image' ? '/second.raf' : '/first.raf') });
  }
  finish([0.1, 0.15, 0.2]);
  expect(await result).toBeNull();
});

it('does not fall back to camera JPEGs before a CPU-rendered image exists', async () => {
  const image = vi.fn();
  vi.stubGlobal('Image', image);
  expect(await whiteBalanceForCurrentPreview({ ...request(), useWgpu: false })).toBeNull();
  expect(image).not.toHaveBeenCalled();
  expect(invoke).not.toHaveBeenCalled();
});

it('uses the finished CPU image and intersects its sample window with the image edge', async () => {
  const sources: string[] = [];
  class PreviewImage {
    width = 20;
    height = 30;
    crossOrigin = '';
    onload: (() => void) | null = null;
    set src(value: string) { sources.push(value); queueMicrotask(() => this.onload?.()); }
  }
  vi.stubGlobal('Image', PreviewImage);
  const drawImage = vi.fn();
  const getImageData = vi.fn(() => ({ data: new Uint8ClampedArray(Array.from({ length: 25 }, () => [80, 80, 80, 0]).flat()) }) as ImageData);
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({ drawImage, getImageData } as Partial<CanvasRenderingContext2D> as CanvasRenderingContext2D);
  expect(await whiteBalanceForCurrentPreview({
    ...request(), useWgpu: false, finalPreviewUrl: 'blob:developed-raw', normalizedX: 1, normalizedY: 1,
  })).toEqual({ temperature: 0, tint: 0 });
  expect(sources).toEqual(['blob:developed-raw']);
  expect(drawImage).toHaveBeenCalledWith(expect.any(PreviewImage), 15, 25, 5, 5, 0, 0, 5, 5);
  expect(getImageData).toHaveBeenCalledWith(0, 0, 5, 5);
  expect(invoke).not.toHaveBeenCalled();
});

it('keeps an unavailable or stale native frame from changing white balance', async () => {
  vi.mocked(invoke).mockResolvedValue(null);
  expect(await whiteBalanceForCurrentPreview(request())).toBeNull();
});

it('rounds a picked balance to the sliders\' whole steps', () => {
  const exact = neutralWhiteBalance([0.18, 0.2, 0.25])!;
  expect(Number.isInteger(exact.temperature)).toBe(false);
  expect(neutralSliders([0.18, 0.2, 0.25])).toEqual({
    temperature: Math.round(exact.temperature),
    tint: Math.round(exact.tint),
  });
});

describe('Auto white balance', () => {
  it('gives a RAW the Auto Kelvin and tint and clears the relative sliders', async () => {
    vi.mocked(invoke).mockImplementation(async (command: string) =>
      command === Invokes.AutoWhiteBalance ? { temperature: 5400, tint: 12 } : null);
    expect(await autoWhiteBalance({ ...photo(), isRaw: true })).toEqual({
      whiteBalance: 'auto', wbTemperature: 5400, wbTint: 12, temperature: 0, tint: 0,
    });
    expect(invoke).not.toHaveBeenCalledWith(Invokes.SamplePreviewGreyWorld, expect.anything());
  });

  it('neutralises the grey world with the relative sliders for files without a calibration', async () => {
    vi.mocked(invoke).mockImplementation(async (command: string) =>
      command === Invokes.SamplePreviewGreyWorld ? [0.18, 0.2, 0.25] : null);
    expect(await autoWhiteBalance({ ...photo(), isRaw: false })).toEqual(neutralSliders([0.18, 0.2, 0.25]));
    expect(invoke).not.toHaveBeenCalledWith(Invokes.AutoWhiteBalance, expect.anything());
    // A RAW without a camera calibration falls back the same way.
    expect(await autoWhiteBalance({ ...photo(), isRaw: true })).toEqual(neutralSliders([0.18, 0.2, 0.25]));
  });

  it('balances a film negative\'s positive on the grey world, not the camera\'s Kelvin', async () => {
    vi.mocked(invoke).mockImplementation(async (command: string) =>
      command === Invokes.SamplePreviewGreyWorld ? [0.18, 0.2, 0.25] : { temperature: 5400, tint: 12 });
    const negative = { ...INITIAL_ADJUSTMENTS, negative: newNegative([0.7, 0.4, 0.2], [0.1, 0.05, 0.02]) };
    expect(await autoWhiteBalance({ ...photo(), isRaw: true }, negative)).toEqual(neutralSliders([0.18, 0.2, 0.25]));
    expect(invoke).not.toHaveBeenCalledWith(Invokes.AutoWhiteBalance, expect.anything());
  });

  it('picks a film negative\'s white balance with the relative sliders', async () => {
    native([0.18, 0.2, 0.25], { temperature: 5400, tint: 12 });
    useEditorStore.setState({
      adjustments: { ...INITIAL_ADJUSTMENTS, negative: newNegative([0.7, 0.4, 0.2], [0.1, 0.05, 0.02]) },
    });
    expect(await whiteBalanceForCurrentPreview(request())).toEqual(neutralSliders([0.18, 0.2, 0.25]));
  });

  it('is null while the preview is not ready', async () => {
    vi.mocked(invoke).mockResolvedValue(null);
    expect(await autoWhiteBalance({ ...photo(), isRaw: false })).toBeNull();
  });
});
