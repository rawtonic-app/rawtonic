import { describe, expect, it } from 'vitest';
import {
  chooseMaskDisplay,
  drawsMaskOverlayNatively,
  isMaskOverlayShown,
  maskColorOf,
  maskColorRgb,
  maskDisplayOf,
  maskOpacityOf,
  maskOverlayOpacity,
  nativeMaskOverlay,
  sameNativeMaskOverlay,
  toggleGrayscaleMask,
  toggleMaskDisplay,
} from './maskDisplay';

it('defaults to always and ignores unknown stored values', () => {
  expect(maskDisplayOf(undefined)).toBe('always');
  expect(maskDisplayOf({ maskDisplay: 'sometimes' as never })).toBe('always');
});

it('M hides the mask and restores the mode that was visible', () => {
  let settings = chooseMaskDisplay(null, 'drawing');
  settings = toggleMaskDisplay(settings);
  expect(settings.maskDisplay).toBe('never');
  settings = toggleMaskDisplay(settings);
  expect(settings).toEqual({ maskDisplay: 'drawing', maskDisplayRestore: 'drawing' });
  expect(toggleMaskDisplay({ maskDisplay: 'never' }).maskDisplay).toBe('always');
});

it('Alt/Option+M switches to grayscale and back to the colored mode', () => {
  let settings = chooseMaskDisplay(null, 'drawing');
  settings = toggleGrayscaleMask(settings);
  expect(settings).toEqual({ maskDisplay: 'grayscale', maskDisplayRestore: 'drawing' });
  settings = toggleGrayscaleMask(settings);
  expect(settings.maskDisplay).toBe('drawing');
  // From a hidden mask, grayscale shows the mask and later returns to color.
  settings = toggleGrayscaleMask(toggleMaskDisplay(settings));
  expect(settings.maskDisplay).toBe('grayscale');
  expect(toggleGrayscaleMask(settings).maskDisplay).toBe('drawing');
  // M hides grayscale and brings grayscale back.
  settings = toggleMaskDisplay(settings);
  expect(settings).toEqual({ maskDisplay: 'never', maskDisplayRestore: 'grayscale' });
  expect(toggleMaskDisplay(settings).maskDisplay).toBe('grayscale');
});

it('choosing never keeps the last visible mode for M', () => {
  const settings = chooseMaskDisplay({ maskDisplay: 'grayscale', maskDisplayRestore: 'grayscale' }, 'never');
  expect(settings).toEqual({ maskDisplay: 'never', maskDisplayRestore: 'grayscale' });
});

it('shows the overlay only while drawing in the drawing mode', () => {
  expect(isMaskOverlayShown('always', false)).toBe(true);
  expect(isMaskOverlayShown('grayscale', false)).toBe(true);
  expect(isMaskOverlayShown('drawing', false)).toBe(false);
  expect(isMaskOverlayShown('drawing', true)).toBe(true);
  expect(isMaskOverlayShown('never', true)).toBe(false);
});

it('defaults to Capture One red at 50% and rejects malformed stored values', () => {
  expect(maskColorOf(undefined)).toBe('#ff0000');
  expect(maskColorOf({ maskColor: '#00FF7f' })).toBe('#00ff7f');
  for (const maskColor of ['red', '#fff', '#12345g', '', 42 as never]) {
    expect(maskColorOf({ maskColor })).toBe('#ff0000');
  }
  expect(maskOpacityOf(undefined)).toBe(50);
  expect(maskOpacityOf({ maskOpacity: 0 })).toBe(0);
  expect(maskOpacityOf({ maskOpacity: 72.6 })).toBe(73);
  expect(maskOpacityOf({ maskOpacity: 250 })).toBe(100);
  expect(maskOpacityOf({ maskOpacity: -5 })).toBe(0);
  expect(maskOpacityOf({ maskOpacity: Number.NaN })).toBe(50);
  expect(maskOpacityOf({ maskOpacity: '80' as never })).toBe(50);
  expect(maskColorRgb('#00ff7f')).toEqual([0, 255, 127]);
  expect(maskColorRgb('nonsense')).toEqual([255, 0, 0]);
});

it('keeps a layer mask visible while its shape is moved, like Capture One', () => {
  const base = {
    display: 'always' as const,
    showingOriginal: false,
    sliderDragging: false,
    masking: true,
    drawing: false,
    shapeInteracting: false,
    controlHovered: false,
  };
  expect(maskOverlayOpacity(base)).toBe(1);
  expect(maskOverlayOpacity({ ...base, shapeInteracting: true })).toBe(1);
  expect(maskOverlayOpacity({ ...base, display: 'drawing' })).toBe(0);
  expect(maskOverlayOpacity({ ...base, display: 'drawing', shapeInteracting: true })).toBe(1);
  expect(maskOverlayOpacity({ ...base, display: 'drawing', drawing: true })).toBe(1);
  expect(maskOverlayOpacity({ ...base, display: 'grayscale', shapeInteracting: true })).toBe(1);
  expect(maskOverlayOpacity({ ...base, display: 'never', shapeInteracting: true })).toBe(0);
  // Sliders, Before and the mask controls' own hover still hide the overlay.
  expect(maskOverlayOpacity({ ...base, sliderDragging: true })).toBe(0);
  expect(maskOverlayOpacity({ ...base, showingOriginal: true })).toBe(0);
  expect(maskOverlayOpacity({ ...base, controlHovered: true })).toBe(0);
  // AI patch masks keep hiding while moved; direct patches show on hover only.
  expect(maskOverlayOpacity({ ...base, masking: false, shapeInteracting: true })).toBe(0);
  expect(maskOverlayOpacity({ ...base, masking: false, directPatch: { markerHovered: false } })).toBe(0);
  expect(maskOverlayOpacity({ ...base, masking: false, directPatch: { markerHovered: true } })).toBe(1);
  expect(maskOverlayOpacity({ ...base, masking: false, controlHovered: true, directPatch: { markerHovered: false } })).toBe(1);
});

it('draws mask layers natively only on the native display with live previews', () => {
  expect(drawsMaskOverlayNatively({ nativeDisplay: true, masking: true, livePreviews: true })).toBe(true);
  // AI patches, the JPEG fallback and disabled live previews keep the PNG overlay.
  expect(drawsMaskOverlayNatively({ nativeDisplay: true, masking: false, livePreviews: true })).toBe(false);
  expect(drawsMaskOverlayNatively({ nativeDisplay: false, masking: true, livePreviews: true })).toBe(false);
  expect(drawsMaskOverlayNatively({ nativeDisplay: true, masking: true, livePreviews: false })).toBe(false);
});

it('describes the native overlay from the display settings', () => {
  expect(nativeMaskOverlay('layer', true, null)).toEqual({
    maskId: 'layer',
    color: [255, 0, 0],
    opacity: 50,
    grayscale: false,
  });
  expect(nativeMaskOverlay('layer', true, { maskDisplay: 'grayscale', maskColor: '#00ff00', maskOpacity: 80 })).toEqual({
    maskId: 'layer',
    color: [0, 255, 0],
    opacity: 80,
    grayscale: true,
  });
  expect(nativeMaskOverlay('layer', false, null)).toBeNull();
  expect(nativeMaskOverlay(null, true, null)).toBeNull();
  expect(nativeMaskOverlay('layer', true, { maskDisplay: 'never' })).toBeNull();
});

it('compares native overlays by value', () => {
  const overlay = nativeMaskOverlay('layer', true, null);
  expect(sameNativeMaskOverlay(overlay, nativeMaskOverlay('layer', true, null))).toBe(true);
  expect(sameNativeMaskOverlay(overlay, nativeMaskOverlay('other', true, null))).toBe(false);
  expect(sameNativeMaskOverlay(overlay, nativeMaskOverlay('layer', true, { maskColor: '#ffffff' }))).toBe(false);
  expect(sameNativeMaskOverlay(overlay, null)).toBe(false);
  expect(sameNativeMaskOverlay(null, null)).toBe(true);
});

it('hides the overlay with the select tool and shows it with a mask tool', () => {
  const state = {
    display: 'always' as const, showingOriginal: false, sliderDragging: false, masking: true,
    drawing: false, shapeInteracting: false, controlHovered: false,
  };
  expect(maskOverlayOpacity({ ...state, toolActive: true })).toBe(1);
  expect(maskOverlayOpacity({ ...state, toolActive: false })).toBe(0);
  expect(maskOverlayOpacity(state)).toBe(1);
});

describe('mask overlay while a luma range slider is dragged', () => {
  const base = {
    display: 'drawing' as const,
    showingOriginal: false,
    sliderDragging: true,
    masking: true,
    drawing: false,
    shapeInteracting: false,
    toolActive: false,
    controlHovered: false,
  };
  it('shows the mask, unlike adjustment sliders', () => {
    expect(maskOverlayOpacity(base)).toBe(0);
    expect(maskOverlayOpacity({ ...base, rangeDragging: true })).toBe(1);
    expect(maskOverlayOpacity({ ...base, display: 'always', rangeDragging: true })).toBe(1);
  });
  it('still respects Never and Before', () => {
    expect(maskOverlayOpacity({ ...base, display: 'never', rangeDragging: true })).toBe(0);
    expect(maskOverlayOpacity({ ...base, showingOriginal: true, rangeDragging: true })).toBe(0);
  });
});
