import type { ParseKeys, TFunction } from "i18next";
import type { Adjustments, MaskContainer } from "../../utils/adjustments";
import { upgradeLayerAutoMask } from "../../utils/autoMaskUpgrade";

/** Sliders a history step names with their new value (decimals shown). */
const SLIDERS: Record<string, [label: ParseKeys, digits: number]> = {
  exposure: ["studio.develop.sliders.exposure", 2],
  contrast: ["studio.develop.sliders.contrast", 0],
  highlights: ["studio.develop.sliders.highlights", 0],
  shadows: ["studio.develop.sliders.shadows", 0],
  whites: ["studio.develop.sliders.whites", 0],
  blacks: ["studio.develop.sliders.blacks", 0],
  brightness: ["studio.develop.sliders.brightness", 0],
  temperature: ["adjustments.color.temperature", 0],
  tint: ["adjustments.color.tint", 0],
  vibrance: ["adjustments.color.vibrance", 0],
  saturation: ["adjustments.color.saturation", 0],
  hue: ["adjustments.color.hue", 0],
  clarity: ["adjustments.details.clarity", 0],
  dehaze: ["adjustments.details.dehaze", 0],
  structure: ["adjustments.details.structure", 0],
  aiDenoise: ["studio.develop.sliders.aiDenoise", 0],
  rotation: ["studio.develop.sliders.rotation", 1],
  vignetteAmount: ["adjustments.effects.vignette", 0],
  grainAmount: ["adjustments.effects.grain", 0],
  glowAmount: ["adjustments.effects.glow", 0],
  halationAmount: ["adjustments.effects.halation", 0],
};

/** The tool other changed values belong to, by key or key prefix. */
const TOOLS: [match: RegExp, label: ParseKeys][] = [
  [/^relight/, "studio.relight.title"],
  [/^(crop|aspectRatio|flip|orientationSteps)/, "studio.develop.groups.crop"],
  [/^(transform|guidedPerspective)/, "studio.develop.groups.geometry"],
  [/^lens(?!Blur)/, "studio.lens.title"],
  [/^(curves|pointCurves|parametricCurve|curveMode|regionCurve)$/, "studio.develop.groups.toneCurve"],
  [/^levels$/, "studio.tone.levels.title"],
  [/^hsl$/, "studio.develop.groups.colorMixer"],
  [/^colorRanges$/, "studio.color.editor.title"],
  [/^colorGrading$/, "studio.color.grading.title"],
  [/^(blackAndWhite|grayMixer)$/, "studio.color.bw.title"],
  [/^colorCalibration$/, "studio.color.calibration.title"],
  [/^(whiteBalance|wbTemperature|wbTint)$/, "studio.develop.groups.whiteBalance"],
  [/^(baseCurve|cameraProfile|toneMapper)$/, "studio.develop.groups.baseCharacteristics"],
  [/^sharpen|^sharpness/, "adjustments.details.sharpening"],
  [/NoiseReduction$/, "studio.develop.groups.noiseReduction"],
  [/^defringe/, "studio.develop.groups.defringe"],
  [/^moire/, "studio.develop.groups.moire"],
  [/^(vignette|grain|glow|halation|flare|lut|lensBlur)/, "studio.develop.groups.effects"],
  [/^faceRetouch$/, "studio.develop.groups.faceRetouch"],
  [/^spots$/, "studio.develop.history.spots"],
  [/^redEyes$/, "studio.develop.history.redEyes"],
  // AI patches are made by Remove (the spot tool's object removal).
  [/^aiPatches$/, "studio.develop.history.remove"],
];

/** Keys that change with an edit but say nothing about it. */
const IGNORED = new Set(["showClipping", "masks"]);

/** Whether two values write out as the same JSON. Unchanged parts of a
 * recipe are the same objects from step to step, so they are compared by
 * reference and only what changed is written out: a painted layer's strokes
 * are not serialised for every step's label. */
function same(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((item, i) => same(item, b[i]));
  }
  if (isPlainObject(a) && isPlainObject(b)) {
    const keysA = Object.keys(a).filter((key) => a[key] !== undefined);
    const keysB = Object.keys(b).filter((key) => b[key] !== undefined);
    return (
      keysA.length === keysB.length &&
      keysA.every((key, i) => key === keysB[i] && same(a[key], b[key]))
    );
  }
  return JSON.stringify(a) === JSON.stringify(b);
}

const isPlainObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" &&
  value !== null &&
  Object.getPrototypeOf(value) === Object.prototype;

function changedKeys(before: object, after: object) {
  const a = before as Record<string, unknown>;
  const b = after as Record<string, unknown>;
  return [...new Set([...Object.keys(a), ...Object.keys(b)])].filter(
    (key) => !IGNORED.has(key) && !same(a[key], b[key]),
  );
}

function signed(value: number, digits: number, language: string) {
  const text = Math.abs(value).toLocaleString(language, {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  });
  return value > 0 ? `+${text}` : value < 0 ? `−${text}` : text;
}

/** What changed between two sets of values, as Lightroom's history names a
 * step: one slider with its new value, else the tools that changed. */
function describeValues(
  before: object,
  after: object,
  t: TFunction,
  language: string,
): string | null {
  const keys = changedKeys(before, after);
  if (!keys.length) return null;
  const value = (after as Record<string, unknown>)[keys[0]];
  if (keys.length === 1 && SLIDERS[keys[0]] && typeof value === "number") {
    const [label, digits] = SLIDERS[keys[0]];
    return `${t(label)} ${signed(value, digits, language)}`;
  }
  // Sliders in the order the tools show them, then the other tools.
  const order = Object.keys(SLIDERS);
  const rank = (key: string) =>
    order.includes(key) ? order.indexOf(key) : order.length;
  const names = [
    ...new Set(
      [...keys].sort((a, b) => rank(a) - rank(b)).map((key) => {
        if (SLIDERS[key]) return t(SLIDERS[key][0]);
        const tool = TOOLS.find(([match]) => match.test(key));
        return tool ? t(tool[1]) : t("studio.develop.history.other");
      }),
    ),
  ];
  return names.length > 2
    ? t("studio.develop.history.many", {
        first: names.slice(0, 2).join(", "),
        more: names.length - 2,
      })
    : names.join(", ");
}

/** What changed in the layers, named with the layer ("Exposure +0.35 · Sky"). */
function describeLayers(
  before: MaskContainer[],
  after: MaskContainer[],
  t: TFunction,
  language: string,
): string | null {
  const added = after.find((m) => !before.some((b) => b.id === m.id));
  if (added) return t("studio.develop.history.layerAdded", { name: added.name });
  const removed = before.find((m) => !after.some((a) => a.id === m.id));
  if (removed)
    return t("studio.develop.history.layerDeleted", { name: removed.name });
  for (const layer of after) {
    const old = before.find((b) => b.id === layer.id);
    if (!old || same(old, layer)) continue;
    const values = describeValues(old.adjustments, layer.adjustments, t, language);
    const step = values
      ? values
      : old.name !== layer.name
        ? t("studio.develop.history.layerRenamed")
        : same(old, upgradeLayerAutoMask(old)) || !same(upgradeLayerAutoMask(old), layer)
          ? t("studio.develop.history.mask")
          : t("studio.develop.history.autoMaskUpgraded");
    return `${step} · ${layer.name}`;
  }
  return before.length || after.length
    ? t("studio.develop.history.layerOrder")
    : null;
}

/** The name of the history step that led from `before` to `after`. */
/** Steps named by what made them rather than by what changed (a restored
 * snapshot): the history shows this name for that step's recipe object. */
const namedSteps = new WeakMap<object, (t: TFunction) => string>();

/** Names the history step whose recipe is `entry`. */
export function nameHistoryStep(entry: object, name: (t: TFunction) => string) {
  namedSteps.set(entry, name);
}

/** The name given to the step `entry` (`nameHistoryStep`), if any. */
export function historyStepName(entry: object, t: TFunction): string | null {
  return namedSteps.get(entry)?.(t) ?? null;
}

export function describeHistoryStep(
  before: Adjustments,
  after: Adjustments,
  t: TFunction,
  language: string,
): string {
  const photo = describeValues(before, after, t, language);
  const layers = same(before.masks, after.masks)
    ? null
    : describeLayers(before.masks ?? [], after.masks ?? [], t, language);
  return (
    [photo, layers].filter(Boolean).join(" · ") ||
    t("studio.develop.history.other")
  );
}
