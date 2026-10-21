import {
  ADJUSTMENT_GROUPS,
  initialAdjustmentsFor,
  sameAdjustmentValue,
  type Adjustments,
  type MaskContainer,
} from './adjustments';
import type { SubMask } from '../components/panel/right/Masks';

/** Mask component types the AI detects on each photograph (Subject, Sky,
 * depth range, People, again when the photograph is opened; an Object
 * selection, pointed on one photograph, is pointed again): pasted or synced
 * onto another photograph they never keep the source's bitmap
 * (`masks::DETECTED_TYPES`). */
export const DETECTED_TYPES = new Set(['ai-subject', 'ai-foreground', 'ai-sky', 'ai-depth', 'ai-people']);

/** Marks a component pasted from another photograph whose detection has not
 * run on this one yet (`masks::DETECT_PENDING`). */
export const DETECT_PENDING = 'detectPending';

/** One group of settings the picker offers (a tool or part of one). */
export interface SettingsGroup {
  section: string;
  label: string;
  keys: string[];
}

/** The photo-wide processing versions (responses) settings are rendered
 * with: Lightroom's Process Version, an entry of its own in the picker, off
 * by default. Unchosen, a photograph keeps its own and pasted values render
 * with them; chosen, the source's travel (also missing ones, as `null`). */
export const RESPONSE_KEYS: string[] = [
  ...new Set(Object.values(ADJUSTMENT_GROUPS).flatMap((groups) => groups.flatMap((g) => g.keys)).filter((key) => /Version$/.test(key))),
];
/** Remembered with the chosen keys when the processing versions are chosen. */
export const RESPONSES_CHOSEN = 'processingVersions';

/** The groups the Copy and Sync settings picker offers, by section, without
 * the processing versions (`RESPONSE_KEYS`) and the local layers (chosen
 * on their own). */
export const SETTINGS_GROUPS: SettingsGroup[] = Object.entries(ADJUSTMENT_GROUPS)
  .filter(([section]) => section !== 'masks')
  .flatMap(([section, groups]) =>
    groups.map((group) => ({ section, label: group.label, keys: group.keys.filter((key) => !RESPONSE_KEYS.includes(key)) })),
  )
  .filter((group) => group.keys.length > 0);

/** Whether `adjustments` changed any setting of `group` from a new edit's
 * (Capture One's auto-selection of adjusted tools). */
export function groupModified(group: SettingsGroup, adjustments: Adjustments, isRaw: boolean): boolean {
  const initial = initialAdjustmentsFor(isRaw) as unknown as Record<string, unknown>;
  const values = adjustments as unknown as Record<string, unknown>;
  return group.keys.some((key) => key in values && !sameAdjustmentValue(values[key], initial[key]));
}

/** What a copy, sync or Previous takes from `source`: the settings of the
 * chosen groups (`keys`), and the chosen local layers (`layerIds`; none:
 * no layers). A chosen setting the source does not have (As Shot white
 * balance has no custom one) is `null`: the destination loses its own too
 * (`removedWhenNull`). Processing versions only when chosen (`RESPONSE_KEYS`
 * among `keys`), each as the source has it. */
export function chosenSettings(source: Adjustments, keys: string[], layerIds: string[]): Partial<Adjustments> {
  const chosen: Record<string, unknown> = {};
  const values = source as unknown as Record<string, unknown>;
  for (const key of keys) {
    if (key !== 'masks') chosen[key] = values[key] === undefined ? null : structuredClone(values[key]);
  }
  const ids = new Set(layerIds);
  const layers = (source.masks ?? []).filter((layer: MaskContainer) => ids.has(layer.id));
  if (layers.length > 0) chosen.masks = structuredClone(layers);
  return chosen as Partial<Adjustments>;
}

/** `base` with `pasted` applied as a copy, sync or Previous applies it: a
 * `null` setting is removed (the source does not have it), as the backend
 * does (`pasted_metadata`). */
export function removedWhenNull<T extends object>(base: T, pasted: Partial<T>): T {
  const next = { ...base, ...pasted } as Record<string, unknown>;
  for (const [key, value] of Object.entries(pasted)) if (value === null) delete next[key];
  return next as T;
}

/** The processing versions a new edit gets (`initialAdjustmentsFor`), RAW
 * and other: a photograph never edited is pasted on with these as its own. */
export function newEditResponses(): { raw: Record<string, unknown>; other: Record<string, unknown> } {
  const pick = (isRaw: boolean) => {
    const values = initialAdjustmentsFor(isRaw) as unknown as Record<string, unknown>;
    return Object.fromEntries(RESPONSE_KEYS.filter((key) => values[key] !== undefined).map((key) => [key, values[key]]));
  };
  return { raw: pick(true), other: pick(false) };
}

/** Layers pasted (`pasted`) into a photograph's own (`own`), as the backend
 * does for the other photographs (`masks::paste_layers`): a pasted layer
 * replaces the layer of the same id (pasted before), the others are added on
 * top, the photograph's own stay. Onto another photograph than the source
 * (`samePhoto` false) the AI detections drop the source's bitmap and wait
 * for this photograph's own (`DETECT_PENDING`). */
export function pasteLayers(
  own: MaskContainer[] | undefined,
  pasted: MaskContainer[],
  samePhoto: boolean,
  replace = false,
): MaskContainer[] {
  const incoming = samePhoto ? pasted : pasted.map(withPendingDetections);
  // Edit all selected: every photograph has the primary's layers.
  if (replace) return incoming;
  const layers = [...(own ?? [])];
  for (const layer of incoming) {
    const at = layers.findIndex((mine) => mine.id === layer.id);
    if (at >= 0) layers[at] = layer;
    else layers.push(layer);
  }
  return layers;
}

function withPendingDetections(layer: MaskContainer): MaskContainer {
  return {
    ...layer,
    subMasks: layer.subMasks.map((sub: SubMask) => {
      const parameters = (sub.parameters ?? {}) as Record<string, unknown>;
      const nothingToDetect = Array.isArray(parameters.parts) && parameters.parts.length === 0;
      if (!DETECTED_TYPES.has(sub.type) || nothingToDetect) return sub;
      const next: Record<string, unknown> = { ...parameters, maskDataBase64: null, [DETECT_PENDING]: true };
      if ('mask_data_base64' in next) next.mask_data_base64 = null;
      return { ...sub, parameters: next };
    }),
  };
}

/** What an AI detection writes into a component's parameters (its bitmap,
 * the geometry it was made in, the end of its wait). */
const DETECTION_RESULT_KEYS = [
  'maskDataBase64',
  'mask_data_base64',
  DETECT_PENDING,
  'rotation',
  'flipHorizontal',
  'flipVertical',
  'orientationSteps',
  'warp',
];

/** `masks` without what AI detections wrote: two recipes whose layers
 * differ only there differ by a detection on that photograph, no edit. */
export function withoutDetections(masks: MaskContainer[] | undefined): unknown {
  return (masks ?? []).map((layer) => ({
    ...layer,
    subMasks: layer.subMasks.map((sub: SubMask) => {
      if (!DETECTED_TYPES.has(sub.type)) return sub;
      const parameters = { ...((sub.parameters ?? {}) as Record<string, unknown>) };
      for (const key of DETECTION_RESULT_KEYS) delete parameters[key];
      return { ...sub, parameters };
    }),
  }));
}

/** Whether a component waits for its AI detection on this photograph (an
 * Object selection: for the user to point at the object again). */
export function detectionPending(sub: SubMask): boolean {
  const parameters = sub.parameters as Record<string, unknown> | undefined;
  return Boolean(parameters?.[DETECT_PENDING]) && !parameters?.maskDataBase64 && !parameters?.mask_data_base64;
}

/** Whether two photograph paths are one file (virtual copies share it). */
export function samePhotograph(a: string | null | undefined, b: string | null | undefined): boolean {
  const file = (path: string) => path.split('?vc=')[0];
  return Boolean(a && b && file(a) === file(b));
}
