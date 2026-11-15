/**
 * The user's develop defaults (Preferences → Develop defaults; G28): one
 * global default, per-camera defaults by make and model, per camera body by
 * serial number (the body's winning over the model's, the model's over the
 * global one), RAW files only unless switched off. They are the settings a
 * photograph without an edit starts with; the backend resolves them
 * (crates/engine/src/develop_defaults.rs, `develop_defaults_for`) and every
 * render of a never-edited photograph applies them, so the editor only
 * overlays what it is told (`withDevelopDefaults`).
 *
 * A default never holds the photograph's own settings: masks, crop,
 * transform and lens geometry, AI patches and the other per-photo keys
 * (`isDefaultKey`).
 */
import type { Preset } from '../components/ui/AppProperties';
import { presetValues, toolPresetToSave } from '../components/studio/toolPresets';
import {
  ADJUSTMENT_GROUPS,
  initialAdjustmentsFor,
  NON_COPYABLE_ADJUSTMENT_KEYS,
  sameAdjustmentValue,
  type Adjustments
} from './adjustments';
import { withStyleValues } from './appliedStyle';
import { expressPastedSettings } from './pastedVersions';

/** The recipe key recording the levels a new edit started with (`develop_defaults::KEY`). */
export const DEVELOP_DEFAULTS_KEY = 'developDefaults';
/** The settings those levels gave it (`develop_defaults::SETTINGS_KEY`): its Before. */
export const DEVELOP_DEFAULTS_SETTINGS_KEY = 'developDefaultsSettings';

export type DefaultLevel = 'global' | 'camera' | 'body';

export interface DefaultSettingsSet {
  /** What it was made from (a preset's name, a photo's), for the list. */
  name?: string;
  settings: Record<string, unknown>;
}

export interface CameraDefaults extends DefaultSettingsSet {
  make: string;
  model: string;
  /** A camera body's serial number; none: the whole model. */
  serial?: string;
  /** Joins the more general level (a saved tool) instead of replacing it (a preset). */
  inherit: boolean;
}

export interface DevelopDefaults {
  rawOnly: boolean;
  global?: DefaultSettingsSet;
  cameras?: CameraDefaults[];
}

export interface CameraIdentity {
  make: string;
  model: string;
  serial?: string;
}

export interface ResolvedDefaults {
  levels: DefaultLevel[];
  settings: Record<string, unknown>;
}

/** `develop_defaults_for`'s answer for one photograph. */
export interface PhotoDefaults {
  camera: CameraIdentity | null;
  resolved: ResolvedDefaults | null;
}

export const NO_DEVELOP_DEFAULTS: DevelopDefaults = { rawOnly: true };

const PER_PHOTO_KEYS = new Set<string>([
  ...ADJUSTMENT_GROUPS.geometry.flatMap((group) => group.keys),
  ...ADJUSTMENT_GROUPS.masks.flatMap((group) => group.keys),
  ...NON_COPYABLE_ADJUSTMENT_KEYS,
  DEVELOP_DEFAULTS_KEY,
  DEVELOP_DEFAULTS_SETTINGS_KEY,
  'colorLabel',
  'lensProfileFor'
]);

/** Whether a recipe key may be part of a default (not the photo's own). */
export const isDefaultKey = (key: string): boolean => !PER_PHOTO_KEYS.has(key);

/** Whether a saved recipe follows the defaults: none, or new (a Reset's)
 * without its defaults decided. */
export function followsDefaults(saved: unknown): boolean {
  if (!saved || (saved as { is_null?: boolean }).is_null) return true;
  const keys = Object.keys(saved as object);
  return (
    !keys.includes(DEVELOP_DEFAULTS_KEY) &&
    keys.every((key) => key === 'lensProfileFor' || key === 'sourceFrameVersion' || key === 'colorLabel')
  );
}

/** `recipe` (a new edit) with the defaults `resolved` over it and their record. */
export function withDevelopDefaults(recipe: Adjustments, resolved: ResolvedDefaults | null | undefined): Adjustments {
  if (!resolved) return recipe;
  return {
    ...recipe,
    ...(structuredClone(resolved.settings) as Partial<Adjustments>),
    developDefaults: [...resolved.levels],
    developDefaultsSettings: structuredClone(resolved.settings)
  };
}

/** The neutral new edit's record: no defaults (Reset to neutral). */
export function withoutDevelopDefaults(recipe: Adjustments): Adjustments {
  const out: Adjustments = { ...recipe, developDefaults: [] };
  delete out.developDefaultsSettings;
  return out;
}

/** The default settings of a full recipe `full`: its keys that differ from
 * the neutral new edit `base`, without the photo's own. */
export function defaultSettingsOf(full: Partial<Adjustments>, base: Partial<Adjustments>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const record = full as Record<string, unknown>;
  const neutral = base as Record<string, unknown>;
  for (const key of Object.keys(record)) {
    if (!isDefaultKey(key) || record[key] === undefined) continue;
    if (sameAdjustmentValue(record[key] as never, neutral[key] as never)) continue;
    out[key] = structuredClone(record[key]);
  }
  return out;
}

/** A preset's or style's settings as a default: applied to a neutral new
 * edit of a RAW (`isRaw`) as applying it to a never-edited photo writes
 * them (its masks, crop and transform left out). */
export function presetDefaultSettings(preset: Preset, isRaw = true): Record<string, unknown> {
  const base = initialAdjustmentsFor(isRaw);
  const own = Object.fromEntries(
    Object.entries(preset.adjustments ?? {}).filter(([key]) => isDefaultKey(key))
  ) as Partial<Adjustments>;
  const style = preset.presetType !== 'tool';
  const values = expressPastedSettings(presetValues(own), {
    recipe: null,
    isRaw,
    isNew: true,
    replaceLayers: true,
    mergeGroups: style
  });
  const full = style ? withStyleValues(base, values) : { ...base, ...values };
  return defaultSettingsOf(full, base);
}

/** A tool's settings of `current` as a default (all of them, also those at
 * their neutral value, so they replace a more general default's). */
export function toolDefaultSettings(current: Adjustments, keys: string[], isRaw: boolean): Record<string, unknown> {
  const picked = toolPresetToSave(current, keys, isRaw) as Record<string, unknown>;
  return Object.fromEntries(
    Object.entries(picked)
      .filter(([key, value]) => isDefaultKey(key) && value !== undefined)
      .map(([key, value]) => [key, structuredClone(value)])
  );
}

const same = (a: string | undefined, b: string | undefined) =>
  (a ?? '').trim().toLowerCase() === (b ?? '').trim().toLowerCase();

const entryIndex = (defaults: DevelopDefaults, camera: CameraIdentity, level: 'camera' | 'body') =>
  (defaults.cameras ?? []).findIndex(
    (entry) =>
      same(entry.make, camera.make) &&
      same(entry.model, camera.model) &&
      (level === 'body' ? entry.serial?.trim() === camera.serial?.trim() : !entry.serial)
  );

/** `defaults` with `set` as the global default (null: none). */
export function withGlobalDefault(defaults: DevelopDefaults, set: DefaultSettingsSet | null): DevelopDefaults {
  const next = { ...defaults };
  if (set) next.global = set;
  else delete next.global;
  return next;
}

/** `defaults` with the camera's (`level` 'body': its body's) default
 * replaced by `set` (a preset or a photo's settings: it replaces the more
 * general defaults, as Lightroom's Raw Defaults). */
export function withCameraDefault(
  defaults: DevelopDefaults,
  camera: CameraIdentity,
  level: 'camera' | 'body',
  set: DefaultSettingsSet
): DevelopDefaults {
  const entry: CameraDefaults = {
    make: camera.make,
    model: camera.model,
    ...(level === 'body' && camera.serial ? { serial: camera.serial } : {}),
    ...set,
    inherit: false
  };
  const cameras = [...(defaults.cameras ?? [])];
  const at = entryIndex(defaults, camera, level);
  if (at >= 0) cameras[at] = entry;
  else cameras.push(entry);
  return { ...defaults, cameras };
}

/** `defaults` with a tool's `settings` saved into the camera's (body's)
 * default, key by key (Capture One's per-tool camera defaults); a new
 * entry joins the more general defaults. */
export function withCameraToolDefault(
  defaults: DevelopDefaults,
  camera: CameraIdentity,
  level: 'camera' | 'body',
  settings: Record<string, unknown>
): DevelopDefaults {
  const cameras = [...(defaults.cameras ?? [])];
  const at = entryIndex(defaults, camera, level);
  const own = at >= 0 ? cameras[at] : null;
  const entry: CameraDefaults = {
    make: camera.make,
    model: camera.model,
    ...(level === 'body' && camera.serial ? { serial: camera.serial } : {}),
    ...(own?.name ? { name: own.name } : {}),
    settings: { ...(own?.settings ?? {}), ...structuredClone(settings) },
    inherit: own ? own.inherit : true
  };
  if (at >= 0) cameras[at] = entry;
  else cameras.push(entry);
  return { ...defaults, cameras };
}

/** `defaults` without its camera entry at `index`. */
export function withoutCameraDefault(defaults: DevelopDefaults, index: number): DevelopDefaults {
  return { ...defaults, cameras: (defaults.cameras ?? []).filter((_, i) => i !== index) };
}

/** The camera's name for menus: its model, with the make unless the model names it. */
export function cameraName(camera: Pick<CameraIdentity, 'make' | 'model'>): string {
  const make = camera.make.trim();
  const model = camera.model.trim();
  if (!make) return model;
  return model.toLowerCase().startsWith(make.toLowerCase()) ? model : `${make} ${model}`;
}

/** Whether a default is set at all (else nothing is applied anywhere). */
export const hasDevelopDefaults = (defaults: DevelopDefaults | null | undefined): boolean =>
  Boolean(defaults?.global || defaults?.cameras?.length);
