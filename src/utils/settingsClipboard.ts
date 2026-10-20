import {
  ADJUSTMENT_GROUPS,
  initialAdjustmentsFor,
  sameAdjustmentValue,
  type Adjustments,
  type MaskContainer,
} from './adjustments';
import type { SubMask } from '../components/panel/right/Masks';

/** Mask component types the AI detects on each photograph (Object by its
 * box, Subject, Sky, depth range, People): pasted or synced onto another
 * photograph they never keep the source's bitmap (`masks::DETECTED_TYPES`). */
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

/** The groups the Copy and Sync settings picker offers, by section, local
 * layers last (they are chosen one by one). */
export const SETTINGS_GROUPS: SettingsGroup[] = Object.entries(ADJUSTMENT_GROUPS)
  .filter(([section]) => section !== 'masks')
  .flatMap(([section, groups]) => groups.map((group) => ({ section, ...group })));

/** Whether `adjustments` changed any setting of `group` from a new edit's
 * (Capture One's auto-selection of adjusted tools). */
export function groupModified(group: SettingsGroup, adjustments: Adjustments, isRaw: boolean): boolean {
  const initial = initialAdjustmentsFor(isRaw) as unknown as Record<string, unknown>;
  const values = adjustments as unknown as Record<string, unknown>;
  return group.keys.some((key) => key in values && !sameAdjustmentValue(values[key], initial[key]));
}

/** What a copy, sync or Previous takes from `source`: the settings of the
 * chosen groups (`keys`), and the chosen local layers (`layerIds`; none:
 * no layers). */
export function chosenSettings(source: Adjustments, keys: string[], layerIds: string[]): Partial<Adjustments> {
  const chosen: Record<string, unknown> = {};
  const values = source as unknown as Record<string, unknown>;
  for (const key of keys) {
    if (key !== 'masks' && Object.prototype.hasOwnProperty.call(values, key)) chosen[key] = structuredClone(values[key]);
  }
  const ids = new Set(layerIds);
  const layers = (source.masks ?? []).filter((layer: MaskContainer) => ids.has(layer.id));
  if (layers.length > 0) chosen.masks = structuredClone(layers);
  return chosen as Partial<Adjustments>;
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

/** Whether a component waits for its AI detection on this photograph. */
export function detectionPending(sub: SubMask): boolean {
  const parameters = sub.parameters as Record<string, unknown> | undefined;
  return Boolean(parameters?.[DETECT_PENDING]) && !parameters?.maskDataBase64 && !parameters?.mask_data_base64;
}

/** Whether two photograph paths are one file (virtual copies share it). */
export function samePhotograph(a: string | null | undefined, b: string | null | undefined): boolean {
  const file = (path: string) => path.split('?vc=')[0];
  return Boolean(a && b && file(a) === file(b));
}
