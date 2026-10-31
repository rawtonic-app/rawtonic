import { describe, expect, it } from 'vitest';
import {
  CROP_ROTATE_BAND_PX,
  clampViewerZoom,
  cropModeBox,
  cropModeMarginPx,
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

describe('crop mode margin', () => {
  // Every side of the box keeps at least `margin` to the viewer edge.
  const roomAround = (cw: number, ch: number, box: { width: number; height: number }) =>
    Math.min((cw - box.width) / 2, (ch - box.height) / 2);

  it('keeps the rotate band around the uncropped frame without a proof margin', () => {
    expect(cropModeMarginPx(1600, 1000)).toBe(CROP_ROTATE_BAND_PX);
    for (const [cw, ch, w, h] of [
      [1600, 1000, 6000, 4000], // landscape photo, landscape viewer
      [1600, 1000, 4000, 6000], // portrait photo
      [700, 1200, 6000, 4000], // portrait viewer
      [700, 1200, 4000, 6000],
      [2560, 1400, 6000, 6000], // full screen, square
    ]) {
      const box = cropModeBox(cw, ch, w, h);
      expect(roomAround(cw, ch, box)).toBeGreaterThanOrEqual(CROP_ROTATE_BAND_PX - 1e-9);
      expect(box.width / box.height).toBeCloseTo(w / h, 9);
      // Contained: one side touches the margin.
      expect(roomAround(cw, ch, box)).toBeCloseTo(CROP_ROTATE_BAND_PX, 9);
    }
  });

  it('uses the proof margin when it is larger than the band', () => {
    const proof = proofMarginPx(1600, 1000, 10); // 100 px
    expect(cropModeMarginPx(1600, 1000, proof)).toBe(100);
    expect(roomAround(1600, 1000, cropModeBox(1600, 1000, 6000, 4000, proof))).toBeCloseTo(100, 9);
    // A smaller proof margin still leaves the band.
    expect(cropModeMarginPx(1600, 1000, 24)).toBe(CROP_ROTATE_BAND_PX);
  });

  it('keeps a photograph in a very small viewer', () => {
    expect(cropModeMarginPx(200, 100)).toBe(25);
    const box = cropModeBox(200, 100, 6000, 4000);
    expect(box.height).toBe(50);
    expect(box.width).toBeCloseTo(75, 9);
    expect(cropModeMarginPx(0, 0)).toBe(0);
    expect(cropModeBox(0, 0, 6000, 4000)).toEqual({ width: 0, height: 0 });
    expect(cropModeBox(800, 600, 0, 4000)).toEqual({ width: 0, height: 0 });
  });

  it('ignores a proof margin that is not a number', () => {
    expect(cropModeMarginPx(1600, 1000, Number.NaN)).toBe(CROP_ROTATE_BAND_PX);
  });
});
