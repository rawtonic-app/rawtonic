/**
 * The Develop viewer's surroundings: the background around the photograph,
 * the proof margin and the zoom floor (fit).
 */

export const VIEWER_BACKGROUNDS = ['default', 'white', 'light', 'medium', 'dark', 'veryDark', 'black'] as const;
export type ViewerBackground = (typeof VIEWER_BACKGROUNDS)[number];

export const SURROUNDINGS_OPACITY = { normal: 0, dim: 0.8, black: 1 } as const;

export const DEFAULT_VIEWER_BACKGROUND: ViewerBackground = 'default';

/**
 * Neutral greys as display-encoded sRGB values (0–255). The display surface is
 * not an sRGB format, so these reach the screen unchanged. Medium is 119: an
 * 18 % reflectance grey (L* 50), the middle of perceived lightness, for
 * judging tones. Dark (80) is one step down from Medium (OKLab L 0.43 from
 * 0.57), well clear of Medium and Darker (18, 0.18) on both sides and of the
 * canvas colours it meets in the shades (24, 38 and 76). Default is the app's canvas
 * colour (`--app-bg-primary`, see viewerBackgroundCss) and follows the
 * interface shade; its entry here is only the fallback when no stylesheet
 * answers (the Darkest canvas, 24).
 */
export const VIEWER_BACKGROUND_LEVELS: Record<ViewerBackground, number> = {
  default: 24,
  black: 0,
  veryDark: 18,
  dark: 80,
  medium: 119,
  light: 201,
  white: 255
};

interface ViewerBackgroundSettings {
  viewerBackground?: string | null;
  /** Replaced by `viewerBackground` (true → Medium); read for old settings. */
  editorNeutralGreyBg?: boolean | null;
}

export const isViewerBackground = (value: unknown): value is ViewerBackground =>
  typeof value === 'string' && (VIEWER_BACKGROUNDS as readonly string[]).includes(value);

/** The viewer background a settings object asks for, migrating the old switch. */
export function resolveViewerBackground(settings: ViewerBackgroundSettings | null | undefined): ViewerBackground {
  if (isViewerBackground(settings?.viewerBackground)) return settings.viewerBackground;
  if (settings?.editorNeutralGreyBg) return 'medium';
  return DEFAULT_VIEWER_BACKGROUND;
}

/** The grey a fixed background renders as; Default is the canvas colour instead. */
function backgroundLevel(background: ViewerBackground): number | null {
  return background === 'default' ? null : VIEWER_BACKGROUND_LEVELS[background];
}

/**
 * The interface shade's canvas colour (`--app-bg-primary`) as 0–255 sRGB.
 * Sources write it as `rgb(r, g, b)` and the built stylesheet as `#rrggbb`,
 * so both are read; anything else falls back like the CSS path does (the
 * Darkest canvas), never to black, which is a choice of its own.
 */
function canvasLevel(): number {
  const declared = getComputedStyle(document.documentElement).getPropertyValue('--app-bg-primary').trim();
  const hex = declared.match(/^#([0-9a-f]{6})$/i);
  const values = hex
    ? [0, 2, 4].map((i) => parseInt(hex[1].slice(i, i + 2), 16))
    : declared
        .match(/[\d.]+/g)
        ?.slice(0, 3)
        .map(Number);
  if (!values || values.length < 3 || values.some((n) => !Number.isFinite(n) || n < 0 || n > 255)) {
    return VIEWER_BACKGROUND_LEVELS.default;
  }
  return (values[0] + values[1] + values[2]) / 3;
}

/** Normalised RGBA for the native display. */
export function viewerBackgroundRgba(background: ViewerBackground, brightness = 1): [number, number, number, number] {
  const level = backgroundLevel(background) ?? canvasLevel();
  const v = (level / 255) * brightness;
  return [v, v, v, 1];
}

export function viewerBackgroundCss(background: ViewerBackground, brightness = 1): string {
  const level = backgroundLevel(background);
  if (level === null) {
    const fallback = VIEWER_BACKGROUND_LEVELS.default;
    const canvas = `var(--app-bg-primary, rgb(${fallback}, ${fallback}, ${fallback}))`;
    return brightness === 1 ? canvas : `color-mix(in srgb, ${canvas} ${brightness * 100}%, black)`;
  }
  const v = level * brightness;
  return `rgb(${v}, ${v}, ${v})`;
}

/** Proof margin sizes offered in Preferences, in % of the viewer's shorter side. */
export const PROOF_MARGIN_PERCENTS = [3, 6, 10] as const;
export const DEFAULT_PROOF_MARGIN_PERCENT = 6;
/** Smallest proof margin in CSS pixels, so a small viewer still shows one. */
export const MIN_PROOF_MARGIN_PX = 24;

/**
 * Width of the proof margin around the fitted photograph, in CSS pixels: a
 * share of the viewer's shorter side, at least MIN_PROOF_MARGIN_PX, and never
 * more than a quarter of that side (so a tiny viewer keeps a photograph).
 */
export function proofMarginPx(
  containerWidth: number,
  containerHeight: number,
  percent: number = DEFAULT_PROOF_MARGIN_PERCENT
): number {
  const shorter = Math.min(containerWidth, containerHeight);
  if (!(shorter > 0)) return 0;
  const share = Number.isFinite(percent) && percent > 0 ? percent : DEFAULT_PROOF_MARGIN_PERCENT;
  const wanted = Math.max(MIN_PROOF_MARGIN_PX, Math.round((shorter * share) / 100));
  return Math.min(wanted, Math.floor(shorter / 4));
}

/** Screen pixels outside the crop frame where a drag rotates the photo. */
export const CROP_ROTATE_BAND_PX = 36;

/**
 * Margin around the uncropped photograph in crop mode, in CSS pixels: room
 * for the crop handles and the rotate band outside the frame (Lightroom,
 * Capture One), or the proof margin when that is larger. Like the proof
 * margin, never more than a quarter of the viewer's shorter side.
 */
export function cropModeMarginPx(containerWidth: number, containerHeight: number, proofMargin = 0): number {
  const shorter = Math.min(containerWidth, containerHeight);
  if (!(shorter > 0)) return 0;
  const wanted = Math.max(CROP_ROTATE_BAND_PX, Number.isFinite(proofMargin) ? proofMargin : 0);
  return Math.min(wanted, Math.floor(shorter / 4));
}

/**
 * Size of the uncropped photograph in crop mode, centred in the viewer: the
 * one box the crop overlay draws in and the native display places the photo
 * in, so the two cannot drift apart.
 */
export function cropModeBox(
  containerWidth: number,
  containerHeight: number,
  frameWidth: number,
  frameHeight: number,
  proofMargin = 0
): { width: number; height: number } {
  if (!(frameWidth > 0 && frameHeight > 0)) return { width: 0, height: 0 };
  const margin = cropModeMarginPx(containerWidth, containerHeight, proofMargin);
  const { width, height } = fitInViewer(containerWidth, containerHeight, frameWidth, frameHeight, margin);
  return { width, height };
}

/**
 * Where the crop selection is on screen while cropping: `box` is where the
 * uncropped, oriented photo (`frame`, px) is placed (x, y, w, h); `crop` is
 * the stored crop in px of that photo (or %). The whole box without a crop.
 * The native display draws the photo's cached (cropped) thumbnail there
 * after a lost device, until its live frame is back (G74 DVL6).
 */
export function cropSelectionOnScreen(
  box: [number, number, number, number],
  frame: { width: number; height: number },
  crop: { x: number; y: number; width: number; height: number; unit?: string } | null | undefined
): [number, number, number, number] {
  if (!crop || !(crop.width > 0 && crop.height > 0) || !(frame.width > 0 && frame.height > 0)) return box;
  const [x, y, w, h] = box;
  const [fw, fh] = crop.unit === '%' ? [100, 100] : [frame.width, frame.height];
  return [x + (crop.x / fw) * w, y + (crop.y / fh) * h, (crop.width / fw) * w, (crop.height / fh) * h];
}

export interface FittedSize {
  width: number;
  height: number;
  offsetX: number;
  offsetY: number;
  scale: number;
}

/** The photograph contained and centred in the viewer, inside `margin` on every side. */
export function fitInViewer(
  containerWidth: number,
  containerHeight: number,
  imageWidth: number,
  imageHeight: number,
  margin = 0
): FittedSize {
  const availableWidth = Math.max(containerWidth - 2 * margin, 0);
  const availableHeight = Math.max(containerHeight - 2 * margin, 0);
  const imageAspect = imageWidth / imageHeight;
  let width: number;
  let height: number;
  if (availableWidth === 0 || availableHeight === 0) {
    // Not laid out yet: nothing is shown.
    width = 0;
    height = 0;
  } else if (imageAspect > availableWidth / availableHeight) {
    width = availableWidth;
    height = availableWidth / imageAspect;
  } else {
    height = availableHeight;
    width = availableHeight * imageAspect;
  }
  return {
    width,
    height,
    offsetX: (containerWidth - width) / 2,
    offsetY: (containerHeight - height) / 2,
    scale: width / imageWidth
  };
}

/**
 * The viewer never shows the photograph smaller than fit: zoom 1 is fit (with
 * the proof margin when it is on), as in Capture One.
 */
export const MIN_VIEWER_ZOOM = 1;

export function clampViewerZoom(zoom: number): number {
  return Number.isFinite(zoom) ? Math.max(MIN_VIEWER_ZOOM, zoom) : MIN_VIEWER_ZOOM;
}

/**
 * Fit as a zoom value of the zoom menu and the keys (1 = 100 %, one image
 * pixel per screen pixel), from the photograph's size and its fitted size.
 */
export function fitZoomValue(
  imageWidth: number,
  imageHeight: number,
  fittedWidth: number,
  fittedHeight: number,
  dpr = 1
): number | null {
  if (!(imageWidth > 0 && imageHeight > 0 && fittedWidth > 0 && fittedHeight > 0)) return null;
  const perPixel =
    imageWidth / imageHeight > fittedWidth / fittedHeight ? fittedWidth / imageWidth : fittedHeight / imageHeight;
  return perPixel * dpr;
}

export const ZOOM_PRESETS = [0.25, 0.5, 1, 2, 4] as const;

/** The zoom menu's entries that are not smaller than fit. */
export function zoomPresetsAboveFit(fit: number | null): number[] {
  if (fit === null || !Number.isFinite(fit)) return [...ZOOM_PRESETS];
  return ZOOM_PRESETS.filter((preset) => preset >= fit * (1 - 1e-3));
}

export interface ScreenRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Whether a point (client coordinates) is on the viewer's empty area, not on the photograph. */
export function isOutsidePhoto(x: number, y: number, photo: ScreenRect): boolean {
  if (!(photo.width > 0 && photo.height > 0)) return true;
  return x < photo.left || y < photo.top || x > photo.left + photo.width || y > photo.top + photo.height;
}

/**
 * Where the photograph is on screen: the fitted size (`fitted`, at zoom 1)
 * after the viewer's pan and zoom, inside the viewer at `viewer`.
 */
export function photoScreenRect(
  viewer: { left: number; top: number },
  transform: { scale: number; positionX: number; positionY: number },
  fitted: { width: number; height: number; offsetX: number; offsetY: number }
): ScreenRect {
  return {
    left: viewer.left + transform.positionX + fitted.offsetX * transform.scale,
    top: viewer.top + transform.positionY + fitted.offsetY * transform.scale,
    width: fitted.width * transform.scale,
    height: fitted.height * transform.scale
  };
}

export interface NativeDisplayState {
  /** Develop, the Library's loupe or the full screen is on screen. */
  viewerOn: boolean;
  /** The native (wgpu) renderer is on in Preferences. */
  nativeRenderer: boolean;
  /** The selected photograph's own first native frame was presented. */
  firstFrame: boolean;
  /** The previous photograph's native frame is held while the next opens. */
  holdingFrame: boolean;
  /** The multi view is shown with natively drawn cells (not Linux's browser cells). */
  nativeMultiView: boolean;
}

/**
 * Whether the window is clear over the native display. The multi view's
 * cells always have a native frame of their own (their render, a stand-in or
 * the old primary's frame), so the window stays clear over them while the
 * next primary's first frame is on its way; the viewer then covers only the
 * primary's cell. An opaque window hid every other cell for as long as that
 * decode took (5-44 s on a slow Windows laptop).
 */
export function windowClearOverNative(state: NativeDisplayState): boolean {
  return state.viewerOn && state.nativeRenderer && (state.firstFrame || state.holdingFrame || state.nativeMultiView);
}

/**
 * Whether the Develop viewer paints its own background (and its loading
 * veil) over the photograph's area. Not once a native frame shows the
 * photograph, and never in the multi view with native cells: there the
 * primary's cell keeps its own native frame (its render, a stand-in or the
 * old primary's frame) until the new primary's first frame, and an opaque
 * viewer blanked that cell for as long as the frame took.
 */
export function viewerPaintsBackground(state: { nativeFrame: boolean; nativeMultiView: boolean }): boolean {
  return !state.nativeFrame && !state.nativeMultiView;
}
