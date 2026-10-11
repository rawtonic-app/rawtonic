import { Mask, SubMask, SubMaskMode } from '../components/panel/right/Masks';
import type { ImageDimensions } from '../hooks/useImageRenderSize';
import { createSubMask } from './maskUtils';

/**
 * What a layer or a component can be made from, after Lightroom's mask
 * menu (Capture One offers a subset): the three drawn shapes, the automatic
 * selections and the ranges. `background` is the subject inverted (as
 * Lightroom's Select Background); `luminance` is the whole photo limited to
 * a luma range.
 */
export type MaskKind =
  | 'brush'
  | 'linear'
  | 'radial'
  | 'subject'
  | 'sky'
  | 'background'
  | 'object'
  | 'people'
  | 'color'
  | 'luminance'
  | 'depth';

export const MASK_KIND_LABELS: Record<MaskKind, string> = {
  brush: 'Brush',
  linear: 'Linear',
  radial: 'Radial',
  subject: 'Subject',
  sky: 'Sky',
  background: 'Background',
  object: 'Object',
  people: 'People',
  color: 'Color range',
  luminance: 'Luminance range',
  depth: 'Depth range',
};

/** What to do on the photo after creating a component of this kind. */
export const MASK_KIND_HINTS: Partial<Record<MaskKind, string>> = {
  object: 'Drag a box around the object.',
  color: 'Click the colour to select.',
};

/** Parts of a People mask (`rawtonic_faces::Part` names), in Lightroom's order. */
export const PEOPLE_PARTS = [
  { part: 'entirePerson', label: 'Entire person' },
  { part: 'faceSkin', label: 'Face skin' },
  { part: 'bodySkin', label: 'Body skin' },
  { part: 'eyebrows', label: 'Eyebrows' },
  { part: 'sclera', label: 'Eye sclera' },
  { part: 'iris', label: 'Iris and pupil' },
  { part: 'lips', label: 'Lips' },
  { part: 'teeth', label: 'Teeth' },
  { part: 'hair', label: 'Hair' },
  { part: 'clothes', label: 'Clothes' },
] as const;
export type PeoplePart = (typeof PEOPLE_PARTS)[number]['part'];

/** A new People mask: everyone, entire person (Lightroom's default). */
export const DEFAULT_PEOPLE_MASK = { parts: ['entirePerson'] as PeoplePart[], person: null, grow: 0, feather: 0 };

/** Lightroom's colour range (measured in Camera Raw): Refine 50, no feather. */
export const DEFAULT_COLOR_RANGE = { refine: 50, grow: 0, feather: 0 };
export const DEFAULT_DEPTH_RANGE = { minDepth: 50, maxDepth: 100, minFade: 15, maxFade: 15, feather: 10 };

/** The components (sub-masks) a new layer or component of `kind` starts
 * with: one, or for a luminance range the whole photo and its range. */
export function createComponents(kind: MaskKind, image: ImageDimensions, mode = SubMaskMode.Additive): SubMask[] {
  const one = (type: Mask, parameters?: Record<string, unknown>, invert = false): SubMask[] => {
    const sub = createSubMask(type, image, mode);
    return [{ ...sub, invert, parameters: { ...sub.parameters, ...parameters } }];
  };
  switch (kind) {
    case 'brush':
      return one(Mask.Brush);
    case 'linear':
      return one(Mask.Linear, {
        range: Math.min(image?.width || 1000, image?.height || 1000) * 0.1,
      });
    case 'radial':
      return one(Mask.Radial);
    case 'subject':
      return one(Mask.AiForeground, { maskDataBase64: null, grow: 0, feather: 0 });
    case 'background':
      return one(Mask.AiForeground, { maskDataBase64: null, grow: 0, feather: 0 }, true);
    case 'sky':
      return one(Mask.AiSky, { maskDataBase64: null, grow: 0, feather: 0 });
    case 'object':
      return one(Mask.AiSubject, { maskDataBase64: null, grow: 0, feather: 0 });
    case 'people':
      return one(Mask.AiPeople, { maskDataBase64: null, ...structuredClone(DEFAULT_PEOPLE_MASK) });
    case 'color':
      return one(Mask.Color, { ...DEFAULT_COLOR_RANGE });
    case 'depth':
      return one(Mask.AiDepth, { maskDataBase64: null, ...DEFAULT_DEPTH_RANGE });
    case 'luminance':
      // The whole photo, limited to its luma range (the range stays last).
      return [...one(Mask.All), createSubMask(Mask.LumaRange, image)];
  }
}

/** The kind a component was made as, for its label. */
export function kindOf(sub: SubMask): MaskKind | null {
  switch (sub.type) {
    case Mask.Brush:
    case Mask.Flow:
      return 'brush';
    case Mask.Linear:
      return 'linear';
    case Mask.Radial:
      return 'radial';
    case Mask.AiForeground:
      return sub.invert ? 'background' : 'subject';
    case Mask.AiSky:
      return 'sky';
    case Mask.AiSubject:
      return 'object';
    case Mask.AiPeople:
      return 'people';
    case Mask.Color:
      return 'color';
    case Mask.AiDepth:
      return 'depth';
    case Mask.LumaRange:
    case Mask.Luminance:
      return 'luminance';
    default:
      return null;
  }
}

/**
 * Whether a brush component erases from what its layer shows: a subtracting
 * brush, or an adding one on an inverted layer (the eraser paints there, as
 * the layer's invert flips the combined mask). Read from the mode and the
 * layer's invert, as the viewer's brush/eraser tool is, so the label stays
 * true when the layer is inverted later and old recipes need no marker.
 */
export function erasesFromLayer(sub: SubMask, layerInverted: boolean): boolean {
  if (sub.type !== Mask.Brush && sub.type !== Mask.Flow) return false;
  return layerInverted ? sub.mode === SubMaskMode.Additive : sub.mode === SubMaskMode.Subtractive;
}

/** Label of a component in the layer's component list. */
export function componentLabel(sub: SubMask): string {
  if (sub.type === Mask.All) return 'Whole photo';
  const kind = kindOf(sub);
  return kind ? MASK_KIND_LABELS[kind] : sub.name || sub.type;
}
