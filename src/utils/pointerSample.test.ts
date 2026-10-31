import { describe, expect, it, vi } from 'vitest';
import {
  createImagePatchFetch,
  createPatchSampler,
  patchFromRgba,
  sampleBlock,
  frameTexel,
  loupePlacement,
  patchCell,
  pointerToNormalized,
  readoutColor,
  readoutValues,
  type PreviewPatch,
} from './pointerSample';

const patchOf = (overrides: Partial<PreviewPatch> = {}): PreviewPatch => ({
  column: 0,
  row: 0,
  width: 11,
  height: 11,
  pixels: Array.from({ length: 121 * 3 }, (_, i) => i % 256),
  center: [5, 5],
  mean: [10, 20, 30],
  frame: [400, 300],
  ...overrides,
});

describe('pointerToNormalized', () => {
  it('maps across the photograph as shown, at any zoom', () => {
    // Fit: the photograph shown 600 × 400 at (100, 50).
    expect(pointerToNormalized(400, 250, { left: 100, top: 50, width: 600, height: 400 })).toEqual({ x: 0.5, y: 0.5 });
    // 400 %: the same photograph 2400 × 1600, panned so its origin is off screen.
    expect(pointerToNormalized(400, 250, { left: -800, top: -550, width: 2400, height: 1600 })).toEqual({ x: 0.5, y: 0.5 });
  });

  it('is independent of the screen pixel density', () => {
    // Both the pointer and the rect are CSS pixels; a 2× screen changes
    // neither, so the same place maps to the same point.
    const rect = { left: 0, top: 0, width: 300, height: 200 };
    for (const dpr of [1, 2, 3]) {
      const devicePoint = { x: 150 * dpr, y: 50 * dpr };
      expect(pointerToNormalized(devicePoint.x / dpr, devicePoint.y / dpr, rect)).toEqual({ x: 0.5, y: 0.25 });
    }
  });

  it('uses the shown (cropped, rotated) photograph, whose frame is sampled', () => {
    // A crop or a 90° orientation changes the shown rect (portrait here);
    // the native frame is rendered from the same cropped, rotated photograph,
    // so the corner of the rect is the corner of the frame.
    const portrait = { left: 200, top: 0, width: 400, height: 600 };
    expect(pointerToNormalized(200, 0, portrait)).toEqual({ x: 0, y: 0 });
    const point = pointerToNormalized(599.9, 599.9, portrait)!;
    expect(frameTexel(point.x, point.y, 267, 400)).toEqual([266, 399]);
  });

  it('is null off the photograph', () => {
    const rect = { left: 10, top: 10, width: 100, height: 100 };
    expect(pointerToNormalized(9, 50, rect)).toBeNull();
    expect(pointerToNormalized(110, 50, rect)).toBeNull();
    expect(pointerToNormalized(50, 110, rect)).toBeNull();
    expect(pointerToNormalized(50, 50, { left: 0, top: 0, width: 0, height: 0 })).toBeNull();
  });
});

describe('frameTexel', () => {
  it('matches the renderer sample centre (sample_center in crates/gpu)', () => {
    expect(frameTexel(0.5, 0.5, 20, 17)).toEqual([10, 8]);
    expect(frameTexel(1, 1, 20, 17)).toEqual([19, 16]);
    expect(frameTexel(0, 0, 20, 17)).toEqual([0, 0]);
  });
});

describe('readout values', () => {
  it('shows 0–255 or percent with one decimal', () => {
    expect(readoutValues([127.6, 0, 255], 'rgb255')).toEqual(['128', '0', '255']);
    expect(readoutValues([127.5, 0, 255], 'percent')).toEqual(['50.0', '0.0', '100.0']);
  });

  it('is the pickers’ mean, or the pointer’s own cell at a clipped edge', () => {
    expect(readoutColor(patchOf())).toEqual([10, 20, 30]);
    const clipped = patchOf({ column: 3, width: 8, mean: null, center: [2, 5], pixels: Array.from({ length: 88 * 3 }, (_, i) => i) });
    // Cell (5, 5) of the block = (2, 5) of the patch = index 5 * 8 + 2.
    expect(readoutColor(clipped)).toEqual([126, 127, 128]);
    expect(patchCell(clipped, 2, 5)).toBeNull();
  });
});

describe('loupePlacement', () => {
  const loupe = { width: 124, height: 148 };
  const viewer = { width: 1000, height: 700 };
  const covers = (place: { left: number; top: number }, p: { x: number; y: number }) =>
    p.x >= place.left && p.x <= place.left + loupe.width && p.y >= place.top && p.y <= place.top + loupe.height;

  it('never covers the sampled pixel, also at the edges', () => {
    for (const p of [{ x: 10, y: 10 }, { x: 990, y: 10 }, { x: 10, y: 690 }, { x: 990, y: 690 }, { x: 500, y: 350 }]) {
      const place = loupePlacement(p, loupe, viewer);
      expect(covers(place, p)).toBe(false);
      expect(place.left + loupe.width).toBeLessThanOrEqual(viewer.width);
      expect(place.top + loupe.height).toBeLessThanOrEqual(viewer.height);
    }
  });

  it('goes below right by default and flips near the edges', () => {
    expect(loupePlacement({ x: 100, y: 100 }, loupe, viewer)).toEqual({ left: 124, top: 124 });
    expect(loupePlacement({ x: 950, y: 650 }, loupe, viewer)).toEqual({ left: 950 - 24 - 124, top: 650 - 24 - 148 });
  });
});

describe('createPatchSampler', () => {
  it('keeps one request in flight and sends only the newest position', async () => {
    const frames: Array<() => void> = [];
    const answers: Array<(patch: PreviewPatch | null) => void> = [];
    const fetch = vi.fn(() => new Promise<PreviewPatch | null>((resolve) => answers.push(resolve)));
    const onSample = vi.fn();
    const sampler = createPatchSampler(onSample, fetch, (run) => frames.push(run));
    const at = (x: number) => ({ path: '/a.raw', x, y: 0.5, clientX: x * 100, clientY: 50 });

    sampler.request(at(0.1));
    sampler.request(at(0.2));
    expect(frames).toHaveLength(1);
    frames.shift()!();
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(fetch).toHaveBeenLastCalledWith('/a.raw', 0.2, 0.5);

    // Moves while the sample is on its way: only the last one is asked for.
    sampler.request(at(0.3));
    sampler.request(at(0.4));
    frames.shift()!();
    expect(fetch).toHaveBeenCalledTimes(1);
    answers.shift()!(patchOf());
    await vi.waitFor(() => expect(onSample).toHaveBeenCalledTimes(1));
    expect(onSample.mock.calls[0][0].clientX).toBe(20);
    frames.shift()!();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(fetch).toHaveBeenLastCalledWith('/a.raw', 0.4, 0.5);
  });

  it('drops an answer that arrives after cancel (pointer left the photograph)', async () => {
    let answer: (patch: PreviewPatch | null) => void = () => {};
    const onSample = vi.fn();
    const sampler = createPatchSampler(
      onSample,
      () => new Promise((resolve) => (answer = resolve)),
      (run) => run(),
    );
    sampler.request({ path: '/a.raw', x: 0.5, y: 0.5, clientX: 0, clientY: 0 });
    sampler.cancel();
    answer(patchOf());
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(onSample).not.toHaveBeenCalled();
  });
});

describe('browser-rendered preview patches (no native display)', () => {
  // Pixel (x, y) of a 20 × 17 image is [x, y, x + y, 255].
  const image = (left: number, top: number, width: number, height: number) => {
    const data: number[] = [];
    for (let y = top; y < top + height; y += 1) for (let x = left; x < left + width; x += 1) data.push(x, y, x + y, 255);
    return data;
  };

  it('uses the renderer’s block, centre and mean (preview_sample_roi, sample_center)', () => {
    expect(sampleBlock(0.5, 0.5, 20, 17)).toEqual({ left: 5, top: 3, width: 11, height: 11 });
    const centre = patchFromRgba(image, 20, 17, 0.5, 0.5)!;
    expect(centre.center).toEqual([5, 5]);
    expect(patchCell(centre, 5, 5)).toEqual([10, 8, 18]);
    expect(centre.mean).toEqual([10, 8, 18]);
    expect(centre.frame).toEqual([20, 17]);
    // The last pixel: a 5 × 5 block at the corner, centre on that pixel.
    const corner = patchFromRgba(image, 20, 17, 1, 1)!;
    expect([corner.width, corner.height, ...corner.center]).toEqual([5, 5, 4, 4]);
    expect(readoutColor(corner)).toEqual([17, 14, 31]);
    expect(patchFromRgba(image, 20, 17, 1.2, 0.5)).toBeNull();
  });

  it('reads the block from the preview image', async () => {
    const drawn: number[][] = [];
    class FakeImage {
      crossOrigin = '';
      src = '';
      naturalWidth = 20;
      naturalHeight = 17;
      decode() {
        return Promise.resolve();
      }
    }
    vi.stubGlobal('Image', FakeImage);
    const getContext = vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({
      clearRect() {},
      drawImage(_image: unknown, left: number, top: number, width: number, height: number) {
        drawn.push([left, top, width, height]);
      },
      getImageData(_x: number, _y: number, width: number, height: number) {
        const [left, top] = drawn[drawn.length - 1];
        return { data: image(left, top, width, height) };
      },
    } as never);
    let url: string | null = 'blob:preview-1';
    const fetch = createImagePatchFetch(() => url);
    const patch = await fetch('/a.raw', 0.5, 0.5);
    expect(drawn[0]).toEqual([5, 3, 11, 11]);
    expect(patch?.mean).toEqual([10, 8, 18]);
    url = null;
    expect(await fetch('/a.raw', 0.5, 0.5)).toBeNull();
    getContext.mockRestore();
    vi.unstubAllGlobals();
  });
});
