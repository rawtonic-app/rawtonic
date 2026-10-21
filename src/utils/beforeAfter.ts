import { Adjustments, INITIAL_ADJUSTMENTS } from './adjustments';

/**
 * Recipe keys that place the photograph in the frame: crop, orientation,
 * transform and lens geometry. The unedited view ("before") keeps them from
 * the edit so it shows the same framing. Must include SPLIT_FRAMING_KEYS
 * (src-tauri/src/editor_preview.rs) and GEOMETRY_KEYS
 * (crates/engine/src/cache_utils.rs): the native split draws the before only
 * beside frames whose keys match (checked by beforeAfter.test.ts).
 */
export const BEFORE_FRAMING_KEYS: Array<keyof Adjustments> = [
  'crop',
  'rotation',
  'flipHorizontal',
  'flipVertical',
  'orientationSteps',
  'aspectRatio',
  'transformDistortion',
  'transformVertical',
  'transformHorizontal',
  'transformRotate',
  'transformAspect',
  'transformScale',
  'transformXOffset',
  'transformYOffset',
  'lensDistortionAmount',
  'lensVignetteAmount',
  'lensTcaAmount',
  'lensDistortionParams',
  'lensCorrectionMode',
  'lensMaker',
  'lensModel',
  'lensDistortionEnabled',
  'lensTcaEnabled',
  'autoChromaticAberration',
  'autoCaVersion',
  'fringeVersion',
  'lensVignetteEnabled',
  'guidedPerspective',
];

/**
 * The recipe shown as "before": the defaults, or a snapshot compared with
 * (`snapshot`, as a restore loads it), with the edit's framing either way,
 * so both sides show the same part of the photograph.
 */
export function originalPreviewAdjustments(adjustments: Adjustments, snapshot?: Adjustments | null): Adjustments {
  const original: Adjustments = { ...(snapshot ?? INITIAL_ADJUSTMENTS) };
  const target = original as unknown as Record<string, unknown>;
  for (const key of BEFORE_FRAMING_KEYS) target[key] = adjustments[key];
  // The unedited photograph through the photo's render pipeline (warps keep
  // wide-gamut colour from 2), so before and after differ by the edit only;
  // a snapshot keeps its own, as it looked.
  if (!snapshot) original.renderVersion = adjustments.renderVersion;
  return original;
}

/** Where the split divider starts: the middle, as in Capture One and Lightroom. */
export const DEFAULT_SPLIT_POSITION = 0.5;

/** A divider position as a share (0–1) of the viewer's width. */
export function clampSplitPosition(position: number): number {
  if (!Number.isFinite(position)) return DEFAULT_SPLIT_POSITION;
  return Math.min(1, Math.max(0, position));
}

/** The divider position for a pointer at `clientX` over a viewer at `rect`. */
export function splitPositionAt(clientX: number, rect: { left: number; width: number }): number {
  if (!(rect.width > 0)) return DEFAULT_SPLIT_POSITION;
  return clampSplitPosition((clientX - rect.left) / rect.width);
}

/**
 * The divider's screen x in device pixels for the native display (the unit
 * of its image rect). The divider belongs to the viewer, not the photograph:
 * it stays where it was put while the photograph is panned or zoomed.
 */
export function splitScreenX(rect: { left: number; width: number }, position: number, dpr: number): number {
  return (rect.left + clampSplitPosition(position) * rect.width) * dpr;
}

/** Viewer-fixed divider expressed in the browser image's local coordinates. */
export function browserSplitWidth(
  position: number,
  size: { containerWidth: number; offsetX: number; width: number },
  transform: { scale: number; positionX: number },
): number {
  if (!(transform.scale > 0)) return 0;
  return Math.max(0, Math.min(size.width,
    (clampSplitPosition(position) * size.containerWidth - transform.positionX) / transform.scale - size.offsetX));
}

/**
 * Linux composes developed browser previews; other platforms use the native
 * GPU display for the split.
 */
export function splitViewAvailable(
  useWgpuRenderer: boolean | undefined | null,
  osPlatform: string | null | undefined,
): boolean {
  return osPlatform === 'linux' || useWgpuRenderer !== false;
}

/** Arrow keys move a focused divider by a small step, Shift by a larger one. */
export function nudgeSplitPosition(position: number, key: string, shift: boolean): number | null {
  const step = shift ? 0.1 : 0.01;
  switch (key) {
    case 'ArrowLeft':
      return clampSplitPosition(position - step);
    case 'ArrowRight':
      return clampSplitPosition(position + step);
    case 'Home':
      return 0;
    case 'End':
      return 1;
    default:
      return null;
  }
}
