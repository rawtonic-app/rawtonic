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
  WHITES_TRAVEL_TONE_VERSION,
  isNewRecipe,
  whitesAmountV17,
  whitesValueV17,
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
}

type Translate = (value: number, from: number, to: number) => number;

interface VersionKey {
  key: string;
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
  { key: 'toneVersion', users: 'always', translated: ['whites'], translate: translateWhites },
  { key: 'colorNoiseVersion', users: 'always', translated: [] },
  { key: 'clarityVersion', users: sliders('clarity'), translated: [] },
  { key: 'structureVersion', users: sliders('structure'), translated: ['structure'], translate: translateStructure },
  { key: 'vibranceVersion', users: sliders('vibrance'), translated: [] },
  {
    key: 'temperatureVersion',
    users: sliders('temperature'),
    translated: ['temperature'],
    translate: translateTemperature
  },
  { key: 'tintVersion', users: sliders('tint'), translated: ['tint'], translate: translateTint },
  { key: 'hslVersion', users: sliders('hsl'), translated: [] },
  { key: 'sharpenVersion', users: sliders('sharpness'), translated: [] },
  { key: 'lumaNoiseVersion', users: sliders('lumaNoiseReduction'), translated: [] },
  {
    key: 'defringeVersion',
    users: { global: ['defringePurpleAmount', 'defringeGreenAmount'], layer: ['defringe'] },
    translated: []
  }
];

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
  const globalUsed = global.some(
    (slider) =>
      !has(pasted, slider) &&
      (has(recipe, slider) && recipe[slider] !== undefined
        ? moved(recipe[slider])
        : target.isRaw && RAW_DEFAULTED.has(slider))
  );
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
 * `target` keeps renders with left as `target` has it, the pasted values
 * re-expressed in that response.
 */
export function expressPastedSettings<T extends Partial<Adjustments>>(settings: T, target: PasteTarget): T {
  const recipe = (target.recipe ?? {}) as Values;
  const carried = withCarriedWhitesFor(settings, target.recipe ?? undefined);
  if (target.isNew ?? (!target.recipe || isNewRecipe(target.recipe))) return carried;
  let out = carried as unknown as Values;
  for (const key of KEYS) {
    const from = asVersion(out[key.key]);
    if (from === undefined) continue;
    const to = asVersion(recipe[key.key]) ?? 1;
    if (from === to || !usedByKept(key, out, recipe, target)) continue;
    const { [key.key]: _dropped, ...kept } = out;
    out = kept;
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
  return out as unknown as T;
}
