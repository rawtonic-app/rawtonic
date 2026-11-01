import type { Adjustments } from './adjustments';

/** The recipe keys that warp the photo before its orientation: the engine's
 * `GEOMETRY_KEYS` (crates/engine/src/cache_utils.rs, compared by
 * geometryKeys.test.ts). AI masks are computed in this frame; thumbnails and
 * face retouch follow it. */
export const GEOMETRY_KEYS = [
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
  'lensManualVignetteAmount',
  'lensManualVignetteMidpoint',
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
] as const satisfies ReadonlyArray<keyof Adjustments>;

/** The photo's orientation, flips and straightening. */
export const ORIENTATION_KEYS = ['orientationSteps', 'flipHorizontal', 'flipVertical', 'rotation'] as const satisfies ReadonlyArray<
  keyof Adjustments
>;
