import { describe, expect, it } from 'vitest';
import { INITIAL_ADJUSTMENTS } from './adjustments';
import {
  BEFORE_FRAMING_KEYS,
  COMPARE_MODES,
  keepMagnification,
  DEFAULT_SPLIT_POSITION,
  nextCompareMode,
  parseSplitOrientation,
  clampSplitPosition,
  nudgeSplitPosition,
  originalPreviewAdjustments,
  splitPositionAt,
  splitScreenX,
  splitViewAvailable,
  browserSplitWidth,
} from './beforeAfter';
import editorPreview from '../../src-tauri/src/editor_preview.rs?raw';
import cacheUtils from '../../crates/engine/src/cache_utils.rs?raw';

/** The string literals of a Rust `const NAME: &[&str] = &[ … ];`. */
function rustKeys(source: string, name: string): string[] {
  const start = source.indexOf(`const ${name}: &[&str] = &[`);
  expect(start, name).toBeGreaterThanOrEqual(0);
  const body = source.slice(start, source.indexOf('];', start));
  return [...body.matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

describe('before/after', () => {
  it('shows the unedited recipe with the edit’s framing', () => {
    const edited = {
      ...INITIAL_ADJUSTMENTS,
      exposure: 1.5,
      contrast: 30,
      crop: { x: 10, y: 20, width: 300, height: 200, unit: 'px' as const },
      rotation: 2.5,
      orientationSteps: 1,
      flipHorizontal: true,
      lensDistortionAmount: 80,
      transformVertical: -12,
    };
    const original = originalPreviewAdjustments(edited);
    expect(original.exposure).toBe(INITIAL_ADJUSTMENTS.exposure);
    expect(original.contrast).toBe(INITIAL_ADJUSTMENTS.contrast);
    expect(original.crop).toEqual(edited.crop);
    expect(original.rotation).toBe(2.5);
    expect(original.orientationSteps).toBe(1);
    expect(original.flipHorizontal).toBe(true);
    expect(original.lensDistortionAmount).toBe(80);
    expect(original.transformVertical).toBe(-12);
    // The edit itself is untouched.
    expect(edited.exposure).toBe(1.5);
  });

  it("keeps the photo's render version, so before and after differ by the edit only", () => {
    for (const renderVersion of [undefined, 2]) {
      const edited = { ...INITIAL_ADJUSTMENTS, exposure: 1, rotation: 2.5, renderVersion };
      expect(originalPreviewAdjustments(edited).renderVersion).toBe(renderVersion);
    }
  });

  it('keeps a framing key the edit leaves unset unset, so both sides match natively', () => {
    const edited = { ...INITIAL_ADJUSTMENTS } as Record<string, unknown>;
    delete edited.guidedPerspective;
    const original = originalPreviewAdjustments(edited as never) as unknown as Record<string, unknown>;
    expect(JSON.stringify(original.guidedPerspective)).toBe(JSON.stringify(edited.guidedPerspective));
  });

  it('copies every key the native split compares', () => {
    const native = [
      ...rustKeys(editorPreview, 'SPLIT_FRAMING_KEYS'),
      ...rustKeys(cacheUtils, 'GEOMETRY_KEYS'),
    ];
    expect(native.length).toBeGreaterThan(20);
    for (const key of native) expect(BEFORE_FRAMING_KEYS).toContain(key);
  });

  it('places the divider as a share of the viewer, clamped to it', () => {
    const viewer = { left: 100, width: 800 };
    expect(splitPositionAt(500, viewer)).toBe(0.5);
    expect(splitPositionAt(300, viewer)).toBe(0.25);
    expect(splitPositionAt(-50, viewer)).toBe(0);
    expect(splitPositionAt(5000, viewer)).toBe(1);
    expect(splitPositionAt(300, { left: 0, width: 0 })).toBe(DEFAULT_SPLIT_POSITION);
    expect(clampSplitPosition(Number.NaN)).toBe(DEFAULT_SPLIT_POSITION);
  });

  it('gives the native display the divider in device pixels, independent of pan and zoom', () => {
    // Viewer at x 100, 800 CSS px wide, on a 2× display.
    expect(splitScreenX({ left: 100, width: 800 }, 0.25, 2)).toBe(600);
    expect(splitScreenX({ left: 100, width: 800 }, 1.5, 1)).toBe(900);
  });

  it('moves a focused divider with the arrow keys', () => {
    expect(nudgeSplitPosition(0.5, 'ArrowLeft', false)).toBeCloseTo(0.49);
    expect(nudgeSplitPosition(0.5, 'ArrowRight', true)).toBeCloseTo(0.6);
    expect(nudgeSplitPosition(0.995, 'ArrowRight', false)).toBe(1);
    expect(nudgeSplitPosition(0.3, 'Home', false)).toBe(0);
    expect(nudgeSplitPosition(0.3, 'End', false)).toBe(1);
    expect(nudgeSplitPosition(0.3, 'KeyA', false)).toBeNull();
  });

  it('needs the native GPU display', () => {
    expect(splitViewAvailable(undefined, 'macos')).toBe(true);
    expect(splitViewAvailable(true, 'windows')).toBe(true);
    expect(splitViewAvailable(false, 'macos')).toBe(false);
    expect(splitViewAvailable(true, 'linux')).toBe(true);
    expect(splitViewAvailable(false, 'linux')).toBe(true);
  });

  it('keeps the browser split fixed in the viewer while zooming and panning', () => {
    const size = { containerWidth: 1000, offsetX: 100, width: 800 };
    expect(browserSplitWidth(0.5, size, { scale: 1, positionX: 0 })).toBe(400);
    expect(browserSplitWidth(0.5, size, { scale: 2, positionX: -200 })).toBe(250);
    expect(browserSplitWidth(0, size, { scale: 2, positionX: 0 })).toBe(0);
    expect(browserSplitWidth(1, size, { scale: 1, positionX: -100 })).toBe(800);
  });
});

describe('side by side', () => {
  it('is the cycle step after top-bottom and comes back to off', () => {
    expect(COMPARE_MODES).toEqual(['off', 'left-right', 'top-bottom', 'side-by-side']);
    expect(nextCompareMode('off')).toBe('left-right');
    expect(nextCompareMode('left-right')).toBe('top-bottom');
    expect(nextCompareMode('top-bottom')).toBe('side-by-side');
    expect(nextCompareMode('side-by-side')).toBe('off');
  });

  it('restores a stored layout, anything else as left-right', () => {
    expect(parseSplitOrientation('side-by-side')).toBe('side-by-side');
    expect(parseSplitOrientation('top-bottom')).toBe('top-bottom');
    expect(parseSplitOrientation('diagonal')).toBe('left-right');
    expect(parseSplitOrientation(undefined)).toBe('left-right');
  });

  it('has no divider for the keys to move', () => {
    expect(nudgeSplitPosition(0.5, 'ArrowLeft', false, 'side-by-side')).toBeNull();
    expect(nudgeSplitPosition(0.5, 'Home', false, 'side-by-side')).toBeNull();
  });
});

describe('keepMagnification', () => {
  it('keeps 100 % on the same detail when the before pane halves the viewer', () => {
    // 8000 px photo: fit 1000 px wide in a 1000 px viewer, zoomed to 100 % (×8),
    // the photo point (0.25, 0.5) at the centre.
    const wide = { width: 1000, height: 500, offsetX: 0, offsetY: 250, containerWidth: 1000, containerHeight: 1000 };
    const half = { width: 500, height: 250, offsetX: 0, offsetY: 375, containerWidth: 500, containerHeight: 1000 };
    const next = keepMagnification(wide, half, 8, 0.25, 0.5);
    expect(next.scale).toBe(16);
    // The centre of the narrower viewer shows the same photo point.
    expect(((half.containerWidth / 2 - next.x) / next.scale - half.offsetX) / half.width).toBeCloseTo(0.25);
    expect(((half.containerHeight / 2 - next.y) / next.scale - half.offsetY) / half.height).toBeCloseTo(0.5);
    // Back again: the original transform.
    const back = keepMagnification(half, wide, next.scale, 0.25, 0.5);
    expect(back.scale).toBe(8);
    expect(back.x).toBeCloseTo(500 - 8 * 250);
  });
});
