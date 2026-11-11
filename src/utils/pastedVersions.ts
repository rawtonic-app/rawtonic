/**
 * Version keys carried by pasted settings (tool copy, presets, Edit all
 * selected) as a photograph takes them: a version key goes with the values
 * it renders only when nothing the paste leaves on the photograph renders
 * with it; otherwise the photograph keeps its own and the pasted values are
 * re-expressed in that response. The editor's half of the backend's
 * `pasted_versions::express_pasted` (crates/engine/src/pasted_versions.rs);
 * both are checked against crates/engine/tests/data/pasted_versions.json.
 */
import {
  CLARITY_VERSION,
  COLOR_NOISE_VERSION,
  DEFRINGE_VERSION,
  DEHAZE_VERSION,
  HSL_VERSION,
  LUMA_NOISE_VERSION,
  SHARPEN_VERSION,
  STRUCTURE_VERSION,
  TEMPERATURE_VERSION,
  TINT_VERSION,
  TONE_VERSION,
  VIBRANCE_VERSION,
  WHITES_TRAVEL_TONE_VERSION,
  isNewRecipe,
  whitesAmountV17,
  whitesValueV17,
  withCarriedBlacksFor,
  withCarriedWhitesFor,
  type Adjustments
} from './adjustments';

/** The photograph pasted onto. */
export interface PasteTarget {
  /** Its recipe before the paste (a style it replaces taken back). */
  recipe: Partial<Adjustments> | null | undefined;
  /** A RAW file: Sharpening and Luminance noise reduction without a value are its defaults. */
  isRaw: boolean;
  /** Pasted layers replace all of its own (else they join them). */
  replaceLayers: boolean;
  /** It has no edit yet and takes every pasted version (default: `recipe` is none or a new recipe). */
  isNew?: boolean;
  /** A style: its nested settings join the photograph's own setting by
   * setting (a colour mixer's other colours stay) instead of replacing them. */
  mergeGroups?: boolean;
}

type Translate = (value: number, from: number, to: number) => number;

interface VersionKey {
  key: string;
  /** The newest version this build renders (a recipe's is clamped to it). */
  newest: number;
  /** `always`: the key changes how the photograph renders without any slider. */
  users: 'always' | { global: string[]; layer: string[] };
  translated: string[];
  translate?: Translate;
}

/** Structure version 2's amount for the slider value (the engine's `structure_amount_v2`). */
export const structureAmountV2 = (value: number): number => {
  const v = Math.min(Math.max(value, -100), 100);
  const [a, b] = v >= 0 ? [0.45, -0.00155] : [0.6, -0.0019];
  return Math.sign(v) * (a * Math.abs(v) + b * v * v);
};

/** The version-2 Structure value whose amount is `amount` (the engine's `structure_value_v2`). */
export const structureValueV2 = (amount: number): number => {
  const [a, b, reach] =
    amount >= 0 ? [0.45, -0.00155, structureAmountV2(100)] : [0.6, -0.0019, -structureAmountV2(-100)];
  const target = Math.min(Math.abs(amount), reach);
  if (target === 0) return 0;
  const x = (-a + Math.sqrt(a * a + 4 * b * target)) / (2 * b);
  let value = Math.sign(amount) * Math.min(x, 100);
  const wanted = Math.fround(Math.sign(amount) * target);
  for (let i = 0; i < 8; i++) {
    const got = Math.fround(structureAmountV2(value));
    if (got === wanted) break;
    const step = Number.EPSILON * Math.max(Math.abs(value), 1);
    value += got < wanted ? step : -step;
  }
  return value;
};

const translateStructure: Translate = (value, from, to) => {
  if (from <= 1 === to <= 1) return value;
  const strength = from <= 1 ? value : structureAmountV2(value);
  return to <= 1 ? strength : structureValueV2(strength);
};

/** Whites: the amount version `from` renders, as `to`'s slider value.
 * (Blacks travel as amounts, expressed once the keys are decided:
 * `withCarriedBlacksFor`.) */
const translateWhites: Translate = (value, from, to) => {
  const amount = from >= WHITES_TRAVEL_TONE_VERSION ? whitesAmountV17(value) : value;
  return to >= WHITES_TRAVEL_TONE_VERSION ? whitesValueV17(amount) : amount;
};

const translateTemperature: Translate = (value, from, to) => {
  if (value <= 0 || Math.min(from, 2) === Math.min(to, 2)) return value;
  if (to >= 2) return value >= 75 ? 100 : 200 - Math.sqrt(40000 - 400 * value);
  const s = Math.min(value, 100);
  return s - (s * s) / 400;
};

const translateTint: Translate = (value, from, to) => {
  if (Math.min(from, 2) === Math.min(to, 2)) return value;
  const factor = value > 0 ? 1.9 : 1.55;
  return to >= 2 ? value / factor : Math.min(Math.max(value * factor, -100), 100);
};

const sliders = (...names: string[]) => ({ global: names, layer: names });

const KEYS: VersionKey[] = [
  {
    key: 'toneVersion',
    newest: TONE_VERSION,
    users: 'always',
    translated: ['whites'],
    translate: translateWhites
  },
  { key: 'colorNoiseVersion', newest: COLOR_NOISE_VERSION, users: 'always', translated: [] },
  { key: 'clarityVersion', newest: CLARITY_VERSION, users: sliders('clarity'), translated: [] },
  {
    key: 'structureVersion',
    newest: STRUCTURE_VERSION,
    users: sliders('structure'),
    translated: ['structure'],
    translate: translateStructure
  },
  { key: 'dehazeVersion', newest: DEHAZE_VERSION, users: sliders('dehaze'), translated: [] },
  { key: 'vibranceVersion', newest: VIBRANCE_VERSION, users: sliders('vibrance'), translated: [] },
  {
    key: 'temperatureVersion',
    newest: TEMPERATURE_VERSION,
    users: sliders('temperature'),
    translated: ['temperature'],
    translate: translateTemperature
  },
  { key: 'tintVersion', newest: TINT_VERSION, users: sliders('tint'), translated: ['tint'], translate: translateTint },
  { key: 'hslVersion', newest: HSL_VERSION, users: sliders('hsl'), translated: [] },
  { key: 'sharpenVersion', newest: SHARPEN_VERSION, users: sliders('sharpness'), translated: [] },
  { key: 'lumaNoiseVersion', newest: LUMA_NOISE_VERSION, users: sliders('lumaNoiseReduction'), translated: [] },
  {
    key: 'defringeVersion',
    newest: DEFRINGE_VERSION,
    users: { global: ['defringePurpleAmount', 'defringeGreenAmount'], layer: ['defringe'] },
    translated: []
  }
];

/** The version keys a paste may keep as the photograph has them. */
export const PASTED_VERSION_KEYS: readonly string[] = KEYS.map((key) => key.key);

/** `before` (the photograph before a style, which a style's record keeps)
 * with each version key the paste kept (`expressed` states it otherwise
 * than `pasted`) as the paste stated it: the photograph renders the same
 * with either (the editor states a missing key's default its own way), and
 * the record must not switch it at the style's Amount's midpoint. */
export function withKeptVersions<T extends Partial<Adjustments>>(
  before: T,
  pasted: Partial<Adjustments>,
  expressed: Partial<Adjustments>
): T {
  const p = pasted as Values;
  const e = expressed as Values;
  const kept = PASTED_VERSION_KEYS.filter((key) => has(p, key) && has(e, key) && e[key] !== p[key]);
  return kept.length === 0 ? before : { ...before, ...Object.fromEntries(kept.map((key) => [key, e[key]])) };
}

const RAW_DEFAULTED = new Set(['sharpness', 'lumaNoiseReduction']);

type Values = Record<string, unknown>;
type Layer = { id?: unknown; adjustments?: Values };

const asVersion = (value: unknown): number | undefined =>
  typeof value === 'number' && Number.isInteger(value) && value >= 0 ? value : undefined;

const moved = (value: unknown): boolean => {
  if (typeof value === 'number') return value !== 0;
  if (value && typeof value === 'object' && !Array.isArray(value))
    return Object.values(value as Values).some(
      (channel) =>
        !!channel &&
        typeof channel === 'object' &&
        Object.values(channel as Values).some((v) => typeof v === 'number' && v !== 0)
    );
  return false;
};

const has = (values: Values, key: string) => Object.prototype.hasOwnProperty.call(values, key);

const isObject = (value: unknown): value is Values => !!value && typeof value === 'object' && !Array.isArray(value);

/** A number off 0 anywhere in `value`. */
const movedAnywhere = (value: unknown): boolean =>
  typeof value === 'number' ? value !== 0 : isObject(value) && Object.values(value).some(movedAnywhere);

/** Whether `own`, merged by setting with `pasted`, keeps a moved value the paste does not set. */
const keepsMovedLeaves = (own: unknown, pasted: unknown): boolean =>
  isObject(own) &&
  isObject(pasted) &&
  Object.entries(own).some(([key, value]) =>
    has(pasted, key) ? keepsMovedLeaves(value, pasted[key]) : movedAnywhere(value)
  );

/** The version `recipe` renders with (the engine's `recipe_versions`: its
 * default when missing, clamped to what this build knows). */
const effective = (key: VersionKey, recipe: Values, isRaw: boolean): number => {
  const own = asVersion(recipe[key.key]);
  if (own !== undefined) return Math.min(Math.max(own, 1), key.newest);
  return key.key === 'colorNoiseVersion' && isRaw && recipe.colorNoiseReduction === undefined ? COLOR_NOISE_VERSION : 1;
};

const keptLayers = (pasted: Values, recipe: Values, replaceLayers: boolean): Layer[] => {
  const own = Array.isArray(recipe.masks) ? (recipe.masks as Layer[]) : [];
  if (!has(pasted, 'masks')) return own;
  if (replaceLayers) return [];
  const ids = new Set(
    (Array.isArray(pasted.masks) ? (pasted.masks as Layer[]) : [])
      .map((layer) => layer?.id)
      .filter((id) => typeof id === 'string')
  );
  return own.filter((layer) => typeof layer?.id !== 'string' || !ids.has(layer.id));
};

const usedByKept = (key: VersionKey, pasted: Values, recipe: Values, target: PasteTarget): boolean => {
  if (key.users === 'always') return true;
  const { global, layer } = key.users;
  const globalUsed = global.some((slider) => {
    const own = recipe[slider];
    if (has(pasted, slider))
      return Boolean(target.mergeGroups) && own !== undefined && keepsMovedLeaves(own, pasted[slider]);
    return own !== undefined ? moved(own) : target.isRaw && RAW_DEFAULTED.has(slider);
  });
  return (
    globalUsed ||
    keptLayers(pasted, recipe, target.replaceLayers).some((own) =>
      layer.some((slider) => moved(own?.adjustments?.[slider]))
    )
  );
};

/**
 * `settings` (carried from another photograph, a preset, Edit all selected)
 * as `target` takes them: Whites carried as their amount in its tone-slider
 * response (`withCarriedWhitesFor`), and every version key that something
 * `target` keeps renders with stated as `target` renders it, the pasted
 * values re-expressed in that response; then Blacks (always carried as
 * amounts) in the tone version and look the photograph ends up with
 * (`withCarriedBlacksFor`).
 */
export function expressPastedSettings<T extends Partial<Adjustments>>(settings: T, target: PasteTarget): T {
  const recipe = (target.recipe ?? {}) as Values;
  const carried = withCarriedWhitesFor(settings, target.recipe ?? undefined);
  if (target.isNew ?? (!target.recipe || isNewRecipe(target.recipe)))
    return withCarriedBlacksFor(carried, target.recipe ?? undefined, target.isRaw);
  let out = carried as unknown as Values;
  for (const key of KEYS) {
    const from = asVersion(out[key.key]);
    if (from === undefined) continue;
    const to = effective(key, recipe, target.isRaw);
    if (from === to || !usedByKept(key, out, recipe, target)) continue;
    // Stated, as the photograph renders it: a missing key may read otherwise
    // once pasted values join (colour noise without an amount).
    out = { ...out, [key.key]: to };
    const translate = key.translate;
    if (!translate) continue;
    const express = (value: unknown) => (typeof value === 'number' ? translate(value, from, to) : value);
    for (const slider of key.translated) {
      if (has(out, slider)) out = { ...out, [slider]: express(out[slider]) };
      if (Array.isArray(out.masks))
        out = {
          ...out,
          masks: (out.masks as Layer[]).map((layer) =>
            layer?.adjustments && has(layer.adjustments, slider)
              ? { ...layer, adjustments: { ...layer.adjustments, [slider]: express(layer.adjustments[slider]) } }
              : layer
          )
        };
    }
  }
  // Blacks (amounts) in the response the photograph now has.
  return withCarriedBlacksFor(out as unknown as T, target.recipe ?? undefined, target.isRaw);
}
