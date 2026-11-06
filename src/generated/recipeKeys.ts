// Generated from the recipe crate's key registry (crates/recipe/src/ts.rs) by
// `cargo run -p rawtonic-recipe --example ts_type`; do not edit. scripts/check.sh
// fails when this file is stale.
import type { ColorCalibration, ColorGradingProps, ColorRange, GrayMixer, Hsl } from '../utils/adjustments';

export type WhiteBalanceMode =
  'asShot' | 'daylight' | 'cloudy' | 'shade' | 'tungsten' | 'fluorescent' | 'flash' | 'custom' | 'auto';

/** The global recipe keys of the sections in the recipe crate, each optional as in a saved recipe. */
export interface RecipeKeys {
  // tone
  /** Exposure in stops (EV). */
  exposure?: number;
  contrast?: number;
  highlights?: number;
  shadows?: number;
  whites?: number;
  blacks?: number;
  brightness?: number;
  /** Local contrast around the photograph's centre of interest. */
  centré?: number;

  // colour
  temperature?: number;
  tint?: number;
  vibrance?: number;
  saturation?: number;
  hue?: number;
  /**
   * Absolute white balance of RAWs (Lightroom's Temp/Tint, camera space): unset or `asShot` keeps the camera's; a
   * preset or `custom` with `wbTemperature` (K) and `wbTint`. The relative `temperature`/`tint` apply on top (JPEGs,
   * layers, older edits).
   */
  whiteBalance?: WhiteBalanceMode;
  wbTemperature?: number;
  wbTint?: number;
  /** Black & white conversion (Lightroom's Treatment, Capture One's B&W). */
  blackAndWhite?: boolean;
  grayMixer?: GrayMixer;
  /** Colour editor ranges, applied in order. */
  colorRanges?: ColorRange[];
  hsl?: Hsl;
  colorGrading?: ColorGradingProps;
  colorCalibration?: ColorCalibration;
}
