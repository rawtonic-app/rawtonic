import { v4 as uuidv4 } from 'uuid';
import { Mask, SubMask, SubMaskMode, formatMaskTypeName } from '../components/panel/right/Masks';
import { ImageDimensions } from '../hooks/useImageRenderSize';

/** Tones in percent of white; falloff in percent on either side. */
export const DEFAULT_LUMA_RANGE = { rangeLow: 0, rangeHigh: 100, falloff: 20 };

/** Select Subject version of new selections (`subjectVersion`; absent = 1,
 * BiRefNet's whole foreground; 2 = the subject in focus, separate objects
 * far out of focus left out: `rawtonic_segment::SUBJECT_VERSION`). */
export const SUBJECT_VERSION = 2;

export const createSubMask = (
  type: Mask,
  imageDimensions: ImageDimensions,
  mode: SubMaskMode = SubMaskMode.Additive
): SubMask => {
  const { width, height } = imageDimensions || { width: 1000, height: 1000 };
  const common = {
    id: uuidv4(),
    visible: true,
    invert: false,
    opacity: 100,
    mode,
    name: formatMaskTypeName(type),
    type,
  };

  switch (type) {
    case Mask.Radial:
      return {
        ...common,
        parameters: {
          centerX: width / 2,
          centerY: height / 2,
          radiusX: width / 4,
          radiusY: width / 4,
          rotation: 0,
          feather: 0.5,
        },
      };
    case Mask.Linear:
      return {
        ...common,
        parameters: { startX: width * 0.25, startY: height / 2, endX: width * 0.75, endY: height / 2, range: 50 },
      };
    case Mask.Brush:
      return { ...common, parameters: { lines: [] } };
    case Mask.LumaRange:
      return { ...common, mode: SubMaskMode.Intersect, parameters: { ...DEFAULT_LUMA_RANGE } };
    case Mask.Flow:
      return { ...common, parameters: { lines: [], flow: 10 } };
    case Mask.AiSubject:
      return { ...common, parameters: { maskDataBase64: null, grow: 0, feather: 0 } };
    case Mask.AiForeground:
      return { ...common, parameters: { maskDataBase64: null, grow: 0, feather: 0, subjectVersion: SUBJECT_VERSION } };
    case Mask.QuickEraser:
      return { ...common, parameters: { maskDataBase64: null, grow: 50, feather: 50 } };
    default:
      return { ...common, parameters: {} };
  }
};

/** What Delete removes in Develop: the component picked in the layer's
 * component list (as Lightroom deletes a selected component), otherwise the
 * selected layer. A layer's only component goes with its layer, so no empty
 * layer is left behind; its luma range does not count as a component. */
export function deleteMaskSelection<T extends { id: string; subMasks: SubMask[] }>(
  masks: T[],
  layerId: string,
  componentId: string | null,
): { masks: T[]; removed: 'component' | 'layer' | null } {
  const layer = masks.find((m) => m.id === layerId);
  if (!layer) return { masks, removed: null };
  const components = layer.subMasks.filter((m) => m.type !== Mask.LumaRange);
  if (componentId && components.length > 1 && components.some((m) => m.id === componentId)) {
    return {
      masks: masks.map((m) =>
        m.id === layerId ? { ...m, subMasks: m.subMasks.filter((s) => s.id !== componentId) } : m,
      ),
      removed: 'component',
    };
  }
  return { masks: masks.filter((m) => m.id !== layerId), removed: 'layer' };
}

/** Adds a component to a layer's components ahead of its luma range, which
 * stays last so that it limits everything drawn on the layer. */
export function addComponentBeforeLumaRange(subMasks: SubMask[], component: SubMask): SubMask[] {
  const index = subMasks.findIndex((m) => m.type === Mask.LumaRange);
  return index < 0 ? [...subMasks, component] : [...subMasks.slice(0, index), component, ...subMasks.slice(index)];
}
