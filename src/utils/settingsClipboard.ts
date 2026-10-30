import {
  ADJUSTMENT_GROUPS,
  RENDER_VERSION,
  initialAdjustmentsFor,
  sameAdjustmentValue,
  type Adjustments,
  type MaskContainer,
} from './adjustments';
import { MAX_RENDERED_LAYERS, isRenderedLayer } from './layerLimit';
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

/** Versions that belong to one tool's data rather than to the processing:
 * the camera CA and geometry versions go with the camera's lens data (Lens
 * Correction), which a never-edited photograph gets with the current ones. */
const TOOL_VERSIONS = new Set(['cameraCaVersion', 'cameraGeometryVersion']);

/** The photo-wide processing versions (responses) settings are rendered
 * with: Lightroom's Process Version, an entry of its own in the picker, off
 * by default. Unchosen, a photograph keeps its own and pasted values render
 * with them; chosen, the source's travel (also missing ones, as `null`). */
export const RESPONSE_KEYS: string[] = [
  ...new Set(
    Object.values(ADJUSTMENT_GROUPS)
      .flatMap((groups) => groups.flatMap((g) => g.keys))
      .filter((key) => /Version$/.test(key) && !TOOL_VERSIONS.has(key)),
  ),
];
/** What the renderer reads for a missing processing version that a
 * never-edited RAW starts with otherwise (edge_fringe.rs: none; lateral_ca.rs:
 * the first estimate): stated, so those defaults do not replace it. */
const MISSING_RESPONSES: Record<string, number> = { fringeVersion: 0, autoCaVersion: 1 };

/** Remembered with the chosen keys when the processing versions are chosen. */
export const RESPONSES_CHOSEN = 'processingVersions';

/** The groups the Copy and Sync settings picker offers, by section, without
 * the processing versions (`RESPONSE_KEYS`) and the local layers (chosen
 * on their own). */
export const SETTINGS_GROUPS: SettingsGroup[] = Object.entries(ADJUSTMENT_GROUPS)
  .filter(([section]) => section !== 'masks')
  .flatMap(([section, groups]) =>
    groups.map((group) => ({
      section: group.clipboardSection ?? section,
      label: group.label,
      keys: group.keys.filter((key) => !RESPONSE_KEYS.includes(key)),
    })),
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
 * (`pasted_metadata`). Processing versions only when chosen (`RESPONSE_KEYS`
 * among `keys`), each as the source has it. */
export function chosenSettings(source: Adjustments, keys: string[], layerIds: string[]): Partial<Adjustments> {
  const chosen: Record<string, unknown> = {};
  const values = source as unknown as Record<string, unknown>;
  for (const key of keys) {
    if (key === 'masks') continue;
    if (values[key] !== undefined && values[key] !== null) chosen[key] = structuredClone(values[key]);
    // A processing version the source lacks, which a never-edited
    // photograph would get anyway: what the renderer reads for it missing.
    else if (key in MISSING_RESPONSES) chosen[key] = MISSING_RESPONSES[key];
    else chosen[key] = null;
  }
  const ids = new Set(layerIds);
  const layers = (source.masks ?? []).filter((layer: MaskContainer) => ids.has(layer.id));
  if (layers.length > 0) chosen.masks = structuredClone(layers);
  return chosen as Partial<Adjustments>;
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

/** Whether two photograph paths are one file (virtual copies share it). */
export function samePhotograph(a: string, b: string): boolean {
  const file = (path: string) => path.split('?vc=')[0];
  return file(a) === file(b);
}

/** What an AI detection writes into a component's parameters (its bitmap,
 * the geometry it was made in, the end of its wait). Keep in sync with
 * `DETECTION_RESULT_KEYS` in crates/engine/src/masks.rs. */
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

/** Whether a component's parameters leave nothing to detect: a People
 * selection without parts (`masks::nothing_to_detect`). */
function nothingToDetect(parameters: Record<string, unknown>): boolean {
  return Array.isArray(parameters.parts) && parameters.parts.length === 0;
}

/** Whether a component waits for its AI detection on this photograph (an
 * Object selection: for the user to point at the object again). */
export function detectionPending(sub: SubMask): boolean {
  const parameters = sub.parameters as Record<string, unknown> | undefined;
  return (
    Boolean(parameters?.[DETECT_PENDING]) &&
    !parameters?.maskDataBase64 &&
    !parameters?.mask_data_base64 &&
    !nothingToDetect(parameters ?? {})
  );
}


/** `base` (a photograph's recipe; none: never edited) with `pasted` pasted
 * onto it as the backend pastes onto a closed photograph (`pasted_into`,
 * `masks::paste_layers`), for the open one: a new recipe starts as a new
 * edit (render version, `newEdit`: a new edit's processing versions); a
 * `null` setting is removed (`keepNulls`: presets keep explicit nulls, as
 * the backend does); pasted layers replace the layer of the same id
 * and join the others (`replaceLayers`: replace them all), their AI
 * detections waiting, under ids of their own, for this photograph's unless
 * it is the source's (`samePhoto`). Null when the layers would exceed what
 * the renderer applies. */
export function pasteInto(
  base: Adjustments | null,
  pasted: Partial<Adjustments>,
  { samePhoto, replaceLayers, newEdit, keepNulls = false }: { samePhoto: boolean; replaceLayers: boolean; newEdit?: Record<string, unknown>; keepNulls?: boolean },
): Adjustments | null {
  const out: Record<string, unknown> = { ...(base ?? {}) };
  if (Object.keys(out).every((key) => key === 'lensProfileFor')) {
    out.renderVersion = RENDER_VERSION;
    for (const [key, value] of Object.entries(newEdit ?? {})) if (!(key in out)) out[key] = value;
  }
  const rendered = (masks: unknown) => ((masks as MaskContainer[] | undefined) ?? []).filter(isRenderedLayer).length;
  const before = rendered(out.masks);
  for (const [key, value] of Object.entries(pasted)) {
    if (value === null) {
      if (keepNulls) out[key] = null;
      else delete out[key];
    } else if (key === 'masks') out.masks = pastedLayers(out.masks as MaskContainer[] | undefined, value as MaskContainer[], samePhoto, replaceLayers);
    else out[key] = value;
  }
  const after = rendered(out.masks);
  if (after > MAX_RENDERED_LAYERS && after > before) return null;
  return out as unknown as Adjustments;
}

function pastedLayers(own: MaskContainer[] | undefined, pasted: MaskContainer[], samePhoto: boolean, replace: boolean): MaskContainer[] {
  const incoming = samePhoto ? pasted : pasted.map(withPendingDetections);
  if (replace) return incoming;
  const layers = [...(own ?? [])];
  for (const layer of incoming) {
    const at = layers.findIndex((mine) => mine.id === layer.id);
    if (at >= 0) layers[at] = layer;
    else layers.push(layer);
  }
  return layers;
}

/** The shared paste/preset reset for a target photo. A bitmapless Object
 * component remains pending until the user selects an object there. */
export function withPendingDetections(layer: MaskContainer): MaskContainer {
  return {
    ...layer,
    subMasks: layer.subMasks.map((sub: SubMask) => {
      const parameters = (sub.parameters ?? {}) as Record<string, unknown>;
      if (!DETECTED_TYPES.has(sub.type)) return sub;
      const next = { ...parameters };
      for (const key of DETECTION_RESULT_KEYS) delete next[key];
      next.maskDataBase64 = null;
      if ('mask_data_base64' in parameters) next.mask_data_base64 = null;
      if (nothingToDetect(parameters)) return { ...sub, parameters: next };
      next[DETECT_PENDING] = true;
      return { ...sub, id: crypto.randomUUID(), parameters: next };
    }),
  };
}

/** Presets keep selection intent only. This makes a new value; a photo's own
 * bitmaps and existing preset files are never changed by reading/applying. */
export function presetToSave<T extends Partial<Adjustments>>(adjustments: T): T {
  return adjustments.masks
    ? { ...adjustments, masks: withoutDetections(adjustments.masks) as MaskContainer[] }
    : adjustments;
}
