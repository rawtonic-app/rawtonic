// Frozen mask-local object contract from main 448a2e3b, before registry generation.
// Includes required/optional modifiers; the index signature must not hide lost fields.
import type {
  MaskAdjustments,
  MaskContainer,
  SubMask,
  ColorRange,
  ColorGradingProps,
  Curves,
  Hsl,
  ParametricCurve,
  SectionVisibility
} from './adjustments';
interface OldMaskAdjustments {
  [index: string]: unknown;
  colorRanges?: ColorRange[];
  blacks: number;
  brightness: number;
  clarity: number;
  colorGrading: ColorGradingProps;
  colorNoiseReduction: number;
  contrast: number;
  curves: Curves;
  pointCurves?: Curves;
  parametricCurve?: ParametricCurve;
  curveMode?: 'point' | 'parametric';
  dehaze: number;
  exposure: number;
  flareAmount: number;
  glowAmount: number;
  halationAmount: number;
  highlights: number;
  hsl: Hsl;
  hue: number;
  id?: string;
  lumaNoiseReduction: number;
  saturation: number;
  sectionVisibility: SectionVisibility;
  shadows: number;
  sharpness: number;
  sharpnessThreshold: number;
  structure: number;
  temperature: number;
  tint: number;
  vibrance: number;
  whites: number;
}

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
type Assert<T extends true> = T;
type MaskNames =
  | 'colorRanges'
  | 'blacks'
  | 'brightness'
  | 'clarity'
  | 'colorGrading'
  | 'colorNoiseReduction'
  | 'contrast'
  | 'curves'
  | 'pointCurves'
  | 'parametricCurve'
  | 'curveMode'
  | 'dehaze'
  | 'exposure'
  | 'flareAmount'
  | 'glowAmount'
  | 'halationAmount'
  | 'highlights'
  | 'hsl'
  | 'hue'
  | 'id'
  | 'lumaNoiseReduction'
  | 'saturation'
  | 'sectionVisibility'
  | 'shadows'
  | 'sharpness'
  | 'sharpnessThreshold'
  | 'structure'
  | 'temperature'
  | 'tint'
  | 'vibrance'
  | 'whites';
export type MaskTypeUnchanged = Assert<Equal<Pick<MaskAdjustments, MaskNames>, Pick<OldMaskAdjustments, MaskNames>>>;
export type MaskStillHasRequiredFields = Assert<
  Equal<Partial<Pick<MaskAdjustments, MaskNames>> extends Pick<MaskAdjustments, MaskNames> ? true : false, false>
>;

interface OldMaskContainer {
  adjustments: MaskAdjustments;
  id: string;
  invert: boolean;
  /** 2: the mask is computed at 16 bits; 3: also Refine edge on both sides
   * of a thin mask; 4 (new layers): also AI masks grown and feathered from
   * the photo around the crop; absent: 8 bits. */
  maskVersion?: number;
  name: string;
  opacity: number;
  /** Refine edge radius in pixels of the full photo (`maskVersion` 2+). */
  refineEdge?: number;
  subMasks: Array<SubMask>;
  visible: boolean;
}

type ContainerNames =
  'adjustments' | 'id' | 'invert' | 'maskVersion' | 'name' | 'opacity' | 'refineEdge' | 'subMasks' | 'visible';
export type MaskContainerUnchanged = Assert<
  Equal<Pick<MaskContainer, ContainerNames>, Pick<OldMaskContainer, ContainerNames>>
>;
