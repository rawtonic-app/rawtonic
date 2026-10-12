/**
 * The Develop viewer's surroundings: the background around the photograph,
 * the proof margin and the zoom floor (fit).
 */

export const VIEWER_BACKGROUNDS = ['black', 'veryDark', 'dark', 'medium', 'light', 'white'] as const;
export type ViewerBackground = (typeof VIEWER_BACKGROUNDS)[number];

export const DEFAULT_VIEWER_BACKGROUND: ViewerBackground = 'dark';

/**
 * Neutral greys as display-encoded sRGB values (0–255). The display surface is
 * not an sRGB format, so these reach the screen unchanged. Medium is 119: an
 * 18 % reflectance grey (L* 50), the middle of perceived lightness, for
 * judging tones; Dark is about the earlier default viewer grey.
 */
export const VIEWER_BACKGROUND_LEVELS: Record<ViewerBackground, number> = {
  black: 0,
  veryDark: 18,
  dark: 38,
  medium: 119,
  light: 200,
  white: 255,
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

/** Normalised RGBA for the native display. */
export function viewerBackgroundRgba(background: ViewerBackground): [number, number, number, number] {
  const v = VIEWER_BACKGROUND_LEVELS[background] / 255;
  return [v, v, v, 1];
}

export function viewerBackgroundCss(background: ViewerBackground): string {
  const v = VIEWER_BACKGROUND_LEVELS[background];
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
  percent: number = DEFAULT_PROOF_MARGIN_PERCENT,
): number {
  const shorter = Math.min(containerWidth, containerHeight);
  if (!(shorter > 0)) return 0;
  const share = Number.isFinite(percent) && percent > 0 ? percent : DEFAULT_PROOF_MARGIN_PERCENT;
  const wanted = Math.max(MIN_PROOF_MARGIN_PX, Math.round((shorter * share) / 100));
  return Math.min(wanted, Math.floor(shorter / 4));
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
  margin = 0,
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
    scale: width / imageWidth,
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
  dpr = 1,
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
  fitted: { width: number; height: number; offsetX: number; offsetY: number },
): ScreenRect {
  return {
    left: viewer.left + transform.positionX + fitted.offsetX * transform.scale,
    top: viewer.top + transform.positionY + fitted.offsetY * transform.scale,
    width: fitted.width * transform.scale,
    height: fitted.height * transform.scale,
  };
}
