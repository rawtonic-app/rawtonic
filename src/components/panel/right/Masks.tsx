import {
  Brush,
  BringToFront,
  Circle,
  Cloud,
  Droplet,
  Droplets,
  Eraser,
  SquareMousePointer,
  RectangleHorizontal,
  TriangleRight,
  User,
  Users,
  Sun,
  Stamp,
  Bandage,
  Spline,
  MopSparkles,
  type LucideIcon,
} from 'lucide-react';
import i18n from 'i18next';
import type { ShapeFrame } from '../../../utils/maskGeometry';

export enum Mask {
  AiDepth = 'ai-depth',
  AiForeground = 'ai-foreground',
  /** Lightroom's People mask: parts of everyone or of one person. */
  AiPeople = 'ai-people',
  AiSky = 'ai-sky',
  AiSubject = 'ai-subject',
  All = 'all',
  Brush = 'brush',
  Flow = 'flow',
  Color = 'color',
  Linear = 'linear',
  Luminance = 'luminance',
  /** Capture One's Luma Range: a layer limited to a tonal range (intersected last). */
  LumaRange = 'luma-range',
  QuickEraser = 'quick-eraser',
  Radial = 'radial',
  Clone = 'clone',
  Heal = 'heal',
  Liquify = 'liquify',
  Retouch = 'retouch',
}

export enum SubMaskMode {
  Additive = 'additive',
  Subtractive = 'subtractive',
  Intersect = 'intersect',
}

export enum ToolType {
  AiSeletor = 'ai-selector',
  Brush = 'brush',
  Eraser = 'eraser',
  GenerativeReplace = 'generative-replace',
  SelectSubject = 'select-subject',
}

/** A painted stroke in image coordinates (`BrushLine` / `FlowLine` in mask_generation.rs). */
export interface MaskStrokeLine {
  brushSize: number;
  feather?: number;
  /** Percent. A brush stroke's opacity and flow (Capture One); a Flow mask
   * line's flow is its per-stroke strength. */
  opacity?: number;
  flow?: number;
  /** Percent; Capture One's tip, used instead of `feather` when present. */
  hardness?: number;
  /** Auto Mask: the stroke keeps to colours like the one under its dabs' centre. */
  autoMask?: boolean;
  /** Auto Mask method; absent = 1 (old strokes, Lightroom imports), 2 = edge-aware, 3 = new strokes (`AUTO_MASK_VERSION`). */
  autoMaskVersion?: number;
  /** Dab placement; absent = 1 (old strokes), 2 = new strokes (crates/engine `DabTip::dab_version_2`). */
  dabVersion?: number;
  /** The photo's geometry the stroke was painted in (`ShapeFrame`); absent on older strokes. */
  frame?: ShapeFrame;
  points: Array<{ x: number; y: number }>;
  tool: ToolType;
}

/**
 * Parameters of a sub-mask. Which fields are present depends on the mask type
 * (the `*MaskParameters` structs in mask_generation.rs and ai_processing.rs,
 * and the patch parameters read in inpainting.rs), so every field is optional.
 */
export interface SubMaskParameters {
  /** Settings sliders address parameters by key. */
  [key: string]: unknown;
  // Radial
  centerX?: number;
  centerY?: number;
  radiusX?: number;
  radiusY?: number;
  // Linear, and the AI subject selection box
  startX?: number;
  startY?: number;
  endX?: number;
  endY?: number;
  range?: number;
  rangeEnd?: number;
  // Brush, flow and patch strokes
  lines?: Array<MaskStrokeLine>;
  flow?: number;
  // Colour and luminance ranges
  targetX?: number;
  targetY?: number;
  /** Inherited RGB tolerance; masks with `refine` use the measured colour range. */
  tolerance?: number;
  /** Lightroom's Refine (0–100) of the colour range. */
  refine?: number;
  /** Colour-range samples: points, or areas with a size (image coordinates). */
  samples?: Array<{ x: number; y: number; width?: number; height?: number }>;
  // AI masks
  maskDataBase64?: string | null;
  /** The same mask data under the key older recipes used. */
  mask_data_base64?: string | null;
  minDepth?: number;
  maxDepth?: number;
  minFade?: number;
  maxFade?: number;
  // Shared refinements and orientation
  grow?: number;
  feather?: number;
  rotation?: number;
  flipHorizontal?: boolean;
  flipVertical?: boolean;
  orientationSteps?: number;
  // Clone/heal source point and liquify mode
  sourceX?: number;
  sourceY?: number;
  liquifyMode?: string;
  /** Radial and linear: the photo's geometry they were drawn in (`ShapeFrame`). */
  frame?: ShapeFrame;
}

export interface SubMask {
  id: string;
  invert: boolean;
  mode: SubMaskMode;
  name?: string;
  opacity: number;
  parameters: SubMaskParameters;
  type: Mask;
  visible: boolean;
}

export function formatMaskTypeName(type: string) {
  if (type === Mask.AiDepth) return i18n.t('masks.types.depth');
  if (type === Mask.AiSubject) return i18n.t('masks.types.subject');
  if (type === Mask.AiForeground) return i18n.t('masks.types.foreground');
  if (type === Mask.AiSky) return i18n.t('masks.types.sky');
  if (type === Mask.AiPeople) return i18n.t('masks.types.people');
  if (type === Mask.All) return i18n.t('masks.types.all');
  if (type === Mask.QuickEraser) return i18n.t('masks.types.quickEraser');
  if (type === Mask.Brush) return i18n.t('masks.types.brush');
  if (type === Mask.Flow) return i18n.t('masks.types.flow');
  if (type === Mask.Color) return i18n.t('masks.types.color');
  if (type === Mask.Linear) return i18n.t('masks.types.linear');
  if (type === Mask.Luminance) return i18n.t('masks.types.luminance');
  if (type === Mask.LumaRange) return 'Luma range';
  if (type === Mask.Radial) return i18n.t('masks.types.radial');
  if (type === Mask.Clone) return i18n.t('masks.types.clone');
  if (type === Mask.Heal) return i18n.t('masks.types.heal');
  if (type === Mask.Liquify) return i18n.t('masks.types.liquify');
  if (type === Mask.Retouch) return i18n.t('masks.types.retouch');
  return type.charAt(0).toUpperCase() + type.slice(1);
}

export function getSubMaskName(subMask: Pick<SubMask, 'name' | 'type'>) {
  return subMask.name?.trim() || formatMaskTypeName(subMask.type);
}

export const MASK_ICON_MAP: Record<Mask, LucideIcon> = {
  [Mask.AiDepth]: BringToFront,
  [Mask.AiForeground]: User,
  [Mask.AiPeople]: Users,
  [Mask.AiSky]: Cloud,
  [Mask.AiSubject]: SquareMousePointer,
  [Mask.All]: RectangleHorizontal,
  [Mask.Brush]: Brush,
  [Mask.Flow]: Droplets,
  [Mask.Color]: Droplet,
  [Mask.Linear]: TriangleRight,
  [Mask.Luminance]: Sun,
  [Mask.LumaRange]: Sun,
  [Mask.QuickEraser]: Eraser,
  [Mask.Radial]: Circle,
  [Mask.Clone]: Stamp,
  [Mask.Heal]: Bandage,
  [Mask.Liquify]: Spline,
  [Mask.Retouch]: MopSparkles,
};
