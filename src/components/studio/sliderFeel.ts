/**
 * Keyboard steps and drag travel of the Develop sliders, Lightroom's as the
 * reference for switchers (notes: agent-knowledge/slider-feel-familiarity.md,
 * benchmark-results/uxv-slider-use-test-2026-11-10). UI only: a stored
 * recipe value means what it meant before.
 */

/** Plain, Shift and Alt (fine) step of one key press, in the control's units. */
export type StepSet = readonly [plain: number, shift: number, alt: number];

export interface KeySteps {
  /** A hovered or focused slider. */
  slider: StepSet;
  /** The numeric value field. */
  field: StepSet;
}

/** Whole-unit sliders (Contrast, Clarity, HSL, Tint, …): the slider steps 5 /
 * Shift 20 / Alt 1, the field 1 / Shift 10. */
export const WHOLE_UNIT_KEYS: KeySteps = { slider: [5, 20, 1], field: [1, 10, 1] };
/** Exposure: 0.10 EV / Shift 0.33 EV on both, Alt 0.01. */
export const EXPOSURE_KEYS: KeySteps = { slider: [0.1, 0.33, 0.01], field: [0.1, 0.33, 0.01] };
/** Kelvin: 50 K / Shift 200 K on both, Alt 10 K. */
export const KELVIN_KEYS: KeySteps = { slider: [50, 200, 10], field: [50, 200, 10] };

/** The key steps of a control: its own, the whole-unit set for a step of 1,
 * else its step, ten times and a tenth of it. */
export function resolveKeySteps(step: number, keys?: KeySteps): KeySteps {
  if (keys) return keys;
  if (step === 1) return WHOLE_UNIT_KEYS;
  const set: StepSet = [step, step * 10, step / 10];
  return { slider: set, field: set };
}

/** The step of one key press for the held modifiers (Alt wins over Shift). */
export function stepOf(set: StepSet, shift: boolean, alt: boolean) {
  return alt ? set[2] : shift ? set[1] : set[0];
}

/** Lightroom's HSL track: the knob travels about 200 pt for ±100 (one unit
 * per point), Basic sliders about 150 pt. A track shorter than this moves the
 * value as if it had this length, so a point of hand movement changes an HSL
 * value as much as there. */
export const HSL_TRAVEL_PT = 200;
