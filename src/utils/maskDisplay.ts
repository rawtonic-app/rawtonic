import type { AppSettings, MaskDisplay } from '../components/ui/AppProperties';
import type { MaskContainer } from './adjustments';

/** Capture One's mask visibility modes, in its menu order. */
export const MASK_DISPLAY_OPTIONS: ReadonlyArray<readonly [MaskDisplay, string]> = [
  ['always', 'Always'],
  ['drawing', 'While drawing'],
  ['grayscale', 'Grayscale'],
  ['never', 'Never'],
];

/** Capture One's default Mask Color: red at 50% opacity. */
export const DEFAULT_MASK_COLOR = '#ff0000';
export const DEFAULT_MASK_OPACITY = 50;
/** Lightroom's built-in overlay swatches. */
export const MASK_COLOR_PRESETS: ReadonlyArray<readonly [string, string]> = [
  ['#ff0000', 'Red'],
  ['#00ff00', 'Green'],
  ['#ffffff', 'White'],
  ['#000000', 'Black'],
];

const MODES = new Set<string>(MASK_DISPLAY_OPTIONS.map(([mode]) => mode));
type MaskDisplaySettings = Pick<AppSettings, 'maskDisplay' | 'maskDisplayRestore'>;

const mode = (value: unknown, fallback: MaskDisplay): MaskDisplay =>
  typeof value === 'string' && MODES.has(value) ? (value as MaskDisplay) : fallback;

export function maskDisplayOf(settings?: MaskDisplaySettings | null): MaskDisplay {
  return mode(settings?.maskDisplay, 'always');
}

function visibleRestore(settings?: MaskDisplaySettings | null): MaskDisplay {
  const restore = mode(settings?.maskDisplayRestore, 'always');
  return restore === 'never' ? 'always' : restore;
}

/** Choose a mode directly; a visible choice is also what M later restores. */
export function chooseMaskDisplay(settings: MaskDisplaySettings | null | undefined, next: MaskDisplay): MaskDisplaySettings {
  return { maskDisplay: next, maskDisplayRestore: next === 'never' ? visibleRestore(settings) : next };
}

/** M: hide a visible mask, or show it again in its last visible mode. */
export function toggleMaskDisplay(settings?: MaskDisplaySettings | null): MaskDisplaySettings {
  const current = maskDisplayOf(settings);
  return current === 'never'
    ? { maskDisplay: visibleRestore(settings), maskDisplayRestore: visibleRestore(settings) }
    : { maskDisplay: 'never', maskDisplayRestore: current };
}

/** Alt/Option+M: switch to the grayscale mask, or back to the last colored mode. */
export function toggleGrayscaleMask(settings?: MaskDisplaySettings | null): MaskDisplaySettings {
  if (maskDisplayOf(settings) !== 'grayscale') {
    const current = maskDisplayOf(settings);
    const colored = current === 'never' ? visibleRestore(settings) : current;
    return { maskDisplay: 'grayscale', maskDisplayRestore: colored === 'grayscale' ? 'always' : colored };
  }
  const restore = visibleRestore(settings);
  const colored = restore === 'grayscale' ? 'always' : restore;
  return { maskDisplay: colored, maskDisplayRestore: colored };
}

/** Whether the overlay is drawn now. Never requested at all in 'never'. */
export function isMaskOverlayShown(display: MaskDisplay, drawing: boolean): boolean {
  return display === 'always' || display === 'grayscale' || (display === 'drawing' && drawing);
}

/** Opacity (0 or 1) of the mask overlay on the canvas. Moving or reshaping a
 * layer's radial/linear mask counts as drawing, as in Capture One, so the mask
 * stays visible in "Always" and appears in "While drawing". AI patch masks
 * hide while they are moved, and direct patches show only on hover. */
export function maskOverlayOpacity(state: {
  display: MaskDisplay;
  showingOriginal: boolean;
  sliderDragging: boolean;
  masking: boolean;
  drawing: boolean;
  shapeInteracting: boolean;
  /** A mask tool (a layer component) is active; false with the select tool. */
  toolActive?: boolean;
  controlHovered: boolean;
  directPatch?: { markerHovered: boolean } | null;
  /** A mask-shaping slider (luma range) is dragged: shown like drawing. */
  rangeDragging?: boolean;
  /** The layer's first adjustment switched the overlay off (`MaskOverlayAuto`). */
  autoHidden?: boolean;
}): 0 | 1 {
  if (state.rangeDragging && !state.showingOriginal && state.masking)
    return isMaskOverlayShown(state.display, true) ? 1 : 0;
  if (state.showingOriginal || state.sliderDragging || (state.shapeInteracting && !state.masking)) return 0;
  if (state.toolActive === false) return 0;
  if (state.directPatch) return state.directPatch.markerHovered || state.controlHovered ? 1 : 0;
  if (state.controlHovered) return 0;
  if (state.autoHidden && !state.drawing && !state.shapeInteracting) return 0;
  return isMaskOverlayShown(state.display, state.drawing || state.shapeInteracting) ? 1 : 0;
}

/**
 * Lightroom's automatic overlay (notes: reference/layers-panel-c1-lr.md): the
 * overlay shows while a mask is made or painted and switches off at the
 * layer's first adjustment, so the effect is visible; M brings it back.
 * - `auto`: shown; the next adjustment of the active layer hides it.
 * - `hidden`: switched off by that adjustment.
 * - `pinned`: shown by the user (M, ⌥M, the display menu); adjustments keep
 *   it until the next mask is made or painted, which returns to `auto`.
 * View state only; the display mode (`maskDisplay`) still decides how and
 * whether the overlay is drawn.
 */
export type MaskOverlayAuto = 'auto' | 'hidden' | 'pinned';

/**
 * The automatic overlay after the layers changed from `previous` to `next`,
 * `activeId` being the edited layer. Making or reshaping a mask (a new
 * layer, a component added, painted, dragged or computed, the layer
 * inverted) shows the overlay; a change of the active layer's adjustments or
 * opacity then hides it once.
 */
export function maskOverlayAfterEdit(
  current: MaskOverlayAuto,
  previous: ReadonlyArray<MaskContainer> | undefined,
  next: ReadonlyArray<MaskContainer> | undefined,
  activeId: string | null,
): MaskOverlayAuto {
  if (previous === next || !next) return current;
  const before = new Map((previous ?? []).map((layer) => [layer.id, layer]));
  if (next.some((layer) => !before.has(layer.id))) return 'auto';
  const was = activeId ? before.get(activeId) : undefined;
  const now = activeId ? next.find((layer) => layer.id === activeId) : undefined;
  if (!was || !now || was === now) return current;
  if (was.subMasks !== now.subMasks || was.invert !== now.invert) return 'auto';
  if (was.adjustments !== now.adjustments || was.opacity !== now.opacity) return current === 'auto' ? 'hidden' : current;
  return current;
}

/** The overlay colour as lowercase "#rrggbb", falling back to red. */
export function maskColorOf(settings?: Pick<AppSettings, 'maskColor'> | null): string {
  const color = settings?.maskColor;
  return typeof color === 'string' && /^#[0-9a-f]{6}$/i.test(color) ? color.toLowerCase() : DEFAULT_MASK_COLOR;
}

/** The overlay opacity as a whole percentage in 0–100, falling back to 50. */
export function maskOpacityOf(settings?: Pick<AppSettings, 'maskOpacity'> | null): number {
  const opacity = settings?.maskOpacity;
  return typeof opacity === 'number' && Number.isFinite(opacity)
    ? Math.round(Math.min(100, Math.max(0, opacity)))
    : DEFAULT_MASK_OPACITY;
}

export function maskColorRgb(color: string): [number, number, number] {
  const value = parseInt(maskColorOf({ maskColor: color }).slice(1), 16);
  return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
}

/** The edited layer's mask as the native display draws it over the preview. */
export interface NativeMaskOverlay {
  maskId: string;
  color: [number, number, number];
  /** Percent. */
  opacity: number;
  grayscale: boolean;
}

/** Whether the native display draws the mask overlay instead of a PNG from
 * `generate_mask_overlay`: mask layers on the native display with live
 * previews, which carry the in-progress mask to the renderer on every move.
 * AI patches and the JPEG fallback keep the PNG. */
export function drawsMaskOverlayNatively(state: {
  nativeDisplay: boolean;
  masking: boolean;
  livePreviews: boolean;
}): boolean {
  return state.nativeDisplay && state.masking && state.livePreviews;
}

/** The overlay for the native display, or null when nothing is drawn. */
export function nativeMaskOverlay(
  maskId: string | null | undefined,
  shown: boolean,
  settings?: Pick<AppSettings, 'maskDisplay' | 'maskDisplayRestore' | 'maskColor' | 'maskOpacity'> | null,
): NativeMaskOverlay | null {
  const display = maskDisplayOf(settings);
  if (!maskId || !shown || display === 'never') return null;
  return {
    maskId,
    color: maskColorRgb(maskColorOf(settings)),
    opacity: maskOpacityOf(settings),
    grayscale: display === 'grayscale',
  };
}

export function sameNativeMaskOverlay(a: NativeMaskOverlay | null, b: NativeMaskOverlay | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.maskId === b.maskId &&
    a.opacity === b.opacity &&
    a.grayscale === b.grayscale &&
    a.color.every((channel, index) => channel === b.color[index])
  );
}
