import { describe, expect, it } from 'vitest';
import {
  clampViewerZoom,
  fitInViewer,
  fitZoomValue,
  isOutsidePhoto,
  photoScreenRect,
  MIN_PROOF_MARGIN_PX,
  proofMarginPx,
  resolveViewerBackground,
  viewerBackgroundCss,
  viewerBackgroundRgba,
  zoomPresetsAboveFit,
} from './viewerDisplay';

describe('zoom floor', () => {
  it('never goes below fit (zoom 1)', () => {
    expect(clampViewerZoom(0.1)).toBe(1);
    expect(clampViewerZoom(0.999)).toBe(1);
    expect(clampViewerZoom(1)).toBe(1);
    expect(clampViewerZoom(3.5)).toBe(3.5);
    expect(clampViewerZoom(Number.NaN)).toBe(1);
  });

  it('leaves out zoom menu entries below fit', () => {
    // A 6000 px wide photograph fitted to 1500 CSS px on a 2× screen: fit is 50 %.
    const fit = fitZoomValue(6000, 4000, 1500, 1000, 2);
    expect(fit).toBeCloseTo(0.5);
    expect(zoomPresetsAboveFit(fit)).toEqual([0.5, 1, 2, 4]);
    expect(zoomPresetsAboveFit(0.3)).toEqual([0.5, 1, 2, 4]);
    // A small photograph whose fit is above 100 %.
    expect(zoomPresetsAboveFit(1.6)).toEqual([2, 4]);
    // Unknown fit (no photograph laid out): every entry.
    expect(zoomPresetsAboveFit(null)).toEqual([0.25, 0.5, 1, 2, 4]);
  });
});

describe('proof margin', () => {
  it('is about 6 % of the shorter side, at least 24 px', () => {
    expect(proofMarginPx(1600, 1000)).toBe(60);
    expect(proofMarginPx(1600, 1000, 10)).toBe(100);
    expect(proofMarginPx(500, 300)).toBe(MIN_PROOF_MARGIN_PX);
    // A tiny viewer keeps most of its area for the photograph.
    expect(proofMarginPx(80, 60)).toBe(15);
    expect(proofMarginPx(0, 0)).toBe(0);
  });

  it('fits the photograph inside the margin, centred', () => {
    // 3:2 landscape in a 1600×1000 viewer with a 60 px margin: 1480×880 is
    // left, so the height limits it (880 × 1.5 = 1320 wide).
    const fitted = fitInViewer(1600, 1000, 6000, 4000, 60);
    expect(fitted.width).toBeCloseTo(1320);
    expect(fitted.height).toBeCloseTo(880);
    expect(fitted.offsetX).toBeCloseTo(140);
    expect(fitted.offsetY).toBeCloseTo(60);
    expect(fitted.scale).toBeCloseTo(1320 / 6000);
  });

  it('without a margin fills the viewer as before', () => {
    const fitted = fitInViewer(1600, 1000, 6000, 4000);
    expect(fitted.height).toBeCloseTo(1000);
    expect(fitted.width).toBeCloseTo(1500);
    expect(fitted.offsetX).toBeCloseTo(50);
    expect(fitted.offsetY).toBe(0);
  });

  it('shows nothing before the viewer is laid out', () => {
    expect(fitInViewer(0, 0, 6000, 4000, 0)).toMatchObject({ width: 0, height: 0 });
  });
});

describe('viewer background', () => {
  it('defaults to Dark and migrates the neutral grey canvas switch to Medium', () => {
    expect(resolveViewerBackground(null)).toBe('dark');
    expect(resolveViewerBackground({})).toBe('dark');
    expect(resolveViewerBackground({ editorNeutralGreyBg: false })).toBe('dark');
    expect(resolveViewerBackground({ editorNeutralGreyBg: true })).toBe('medium');
    expect(resolveViewerBackground({ editorNeutralGreyBg: true, viewerBackground: 'white' })).toBe('white');
    expect(resolveViewerBackground({ viewerBackground: 'nonsense' })).toBe('dark');
  });

  it('is a neutral grey; Medium is the 18 % grey (L* 50)', () => {
    expect(viewerBackgroundRgba('black')).toEqual([0, 0, 0, 1]);
    expect(viewerBackgroundRgba('white')).toEqual([1, 1, 1, 1]);
    expect(viewerBackgroundCss('medium')).toBe('rgb(119, 119, 119)');
    const [r, g, b] = viewerBackgroundRgba('medium');
    expect(r).toBe(g);
    expect(g).toBe(b);
    const linear = ((r + 0.055) / 1.055) ** 2.4;
    expect(linear).toBeCloseTo(0.18, 2);
  });
});

describe('right-click target', () => {
  const photo = { left: 100, top: 50, width: 800, height: 600 };
  it('is the empty area only outside the photograph', () => {
    expect(isOutsidePhoto(500, 300, photo)).toBe(false);
    expect(isOutsidePhoto(100, 50, photo)).toBe(false);
    expect(isOutsidePhoto(99, 300, photo)).toBe(true);
    expect(isOutsidePhoto(500, 651, photo)).toBe(true);
    expect(isOutsidePhoto(950, 300, photo)).toBe(true);
  });
});

it('finds the photograph on screen after pan and zoom, so the proof margin counts as empty area', () => {
  // Fitted with a 60 px margin in a viewer at (200, 40), zoomed 2× and panned.
  const fitted = fitInViewer(1600, 1000, 6000, 4000, 60);
  const atFit = photoScreenRect({ left: 200, top: 40 }, { scale: 1, positionX: 0, positionY: 0 }, fitted);
  expect(atFit).toEqual({ left: 340, top: 100, width: fitted.width, height: fitted.height });
  // A click in the margin is beside the photograph; one just inside is on it.
  expect(isOutsidePhoto(330, 500, atFit)).toBe(true);
  expect(isOutsidePhoto(345, 500, atFit)).toBe(false);
  const zoomed = photoScreenRect({ left: 200, top: 40 }, { scale: 2, positionX: -500, positionY: -300 }, fitted);
  expect(zoomed.left).toBeCloseTo(200 - 500 + 140 * 2);
  expect(zoomed.width).toBeCloseTo(fitted.width * 2);
});
