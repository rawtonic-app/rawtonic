import { validSequenceStart } from "./namingTemplates";
import {
  BorderBasis,
  FILE_FORMATS,
  WatermarkAnchor,
  type ExportPreset,
  type ExportSettings,
  type OutputColorSpace,
  type OutputSharpeningRecipe,
  type TiffBitDepth,
} from '../components/ui/ExportImportProperties';

export const COLOR_SPACES: OutputColorSpace[] = ['srgb', 'displayP3', 'adobeRgb', 'proPhoto'];

export const COLOR_SPACE_NAMES: Record<OutputColorSpace, string> = {
  srgb: 'sRGB',
  displayP3: 'Display P3',
  adobeRgb: 'Adobe RGB (1998)',
  proPhoto: 'ProPhoto RGB',
};


/** How `export_images` resizes: an edge in pixels, a megapixel budget or a percentage. */
export type ResizeMode = 'longEdge' | 'shortEdge' | 'width' | 'height' | 'dimensions' | 'megapixels' | 'percentage';

export const RESIZE_MODES: ResizeMode[] = [
  'longEdge',
  'shortEdge',
  'width',
  'height',
  'dimensions',
  'megapixels',
  'percentage',
];

/** The settings the Studio export form edits, as one value. */
export interface RecipeDraft {
  /** Output extension: jpg, png, tiff, webp, jxl, avif or cube (the look as a 3D LUT). */
  format: string;
  quality: number;
  depth: TiffBitDepth;
  colorSpace: OutputColorSpace;
  resize: boolean;
  resizeMode: ResizeMode;
  /** Pixels for the edge modes, megapixels, or percent. */
  resizeValue: number;
  /** The box's other side for `dimensions` (pixels). */
  resizeValue2: number;
  dontEnlarge: boolean;
  metadata: boolean;
  stripGps: boolean;
  stripCreatorContact?: boolean;
  template: string;
  sequenceStart: number;
  /** Session Output folder, next to the originals, or a folder. */
  where: 'output' | 'original' | 'choose';
  /** The folder for `choose`, when the recipe remembers one. */
  folder?: string;
  /** Put the files in this folder inside the destination (any destination). */
  subfolder: string;
  /** Recreate the photographs' folders below their library root. */
  preserveFolders: boolean;
  /** Set the files' dates to the capture date. */
  preserveTimestamps: boolean;
  /** Also write each mask as an image and an alpha PNG. */
  exportMasks: boolean;
  watermark: boolean;
  watermarkPath: string | null;
  watermarkAnchor: WatermarkAnchor;
  /** Watermark width in percent of the photograph's short edge. */
  watermarkScale: number;
  /** Inset from the edges in percent of the short edge. */
  watermarkSpacing: number;
  watermarkOpacity: number;
  /** An image file, or text in an installed font. */
  watermarkKind: 'image' | 'text';
  watermarkText: string;
  /** Installed font family; null = the system's sans-serif. */
  watermarkFont: string | null;
  watermarkColor: string;
  watermarkShadow: boolean;
  border: boolean;
  borderBasis: BorderBasis;
  borderHorizontal: number;
  borderVertical: number;
  borderColor: string;
  pad: boolean;
  padWidth: number;
  padHeight: number;
  padColor: string;
  sharpening: OutputSharpeningRecipe['mode'];
  /** Percent of the detail added back. */
  sharpenAmount: number;
  /** Sigma in output pixels (for screen). */
  sharpenRadius: number;
  /** Levels of 255 below which detail is left alone. */
  sharpenThreshold: number;
  /** For print: the viewing distance and the print resolution set the radius. */
  printDistanceCm: number;
  printPpi: number;
}

/** A new form; also the value for every setting an older saved recipe lacks. */
export const DEFAULT_RECIPE_DRAFT: RecipeDraft = {
  format: 'jpg',
  quality: 90,
  depth: 16,
  colorSpace: 'srgb',
  resize: false,
  resizeMode: 'longEdge',
  resizeValue: 3000,
  resizeValue2: 2000,
  dontEnlarge: true,
  metadata: true,
  stripGps: true,
  stripCreatorContact: false,
  template: '{original_filename}-edited',
  sequenceStart: 1,
  where: 'choose',
  subfolder: '',
  preserveFolders: false,
  preserveTimestamps: true,
  exportMasks: false,
  watermark: false,
  watermarkPath: null,
  watermarkAnchor: WatermarkAnchor.BottomRight,
  watermarkScale: 10,
  watermarkSpacing: 5,
  watermarkOpacity: 75,
  watermarkKind: 'image',
  watermarkText: '© ',
  watermarkFont: null,
  watermarkColor: '#ffffff',
  watermarkShadow: true,
  border: false,
  borderBasis: BorderBasis.LongEdge,
  borderHorizontal: 2,
  borderVertical: 2,
  borderColor: '#ffffff',
  pad: false,
  padWidth: 1,
  padHeight: 1,
  padColor: '#ffffff',
  sharpening: 'off',
  sharpenAmount: 100,
  sharpenRadius: 0.8,
  sharpenThreshold: 1,
  printDistanceCm: 50,
  printPpi: 300,
};

/** The radius that sharpens the finest detail an eye resolves at a viewing
 * distance on a print (one arc minute; `print_radius` in the export crate). */
export const printRadius = (distanceCm: number, ppi: number) =>
  clamp((distanceCm * 10 * Math.tan(Math.PI / 10800) * ppi) / 25.4, 0.2, 10);

/** A sensible value when the resize mode changes to one with another unit. */
export const defaultResizeValue = (mode: ResizeMode) =>
  mode === 'megapixels' ? 12 : mode === 'percentage' ? 50 : DEFAULT_RECIPE_DRAFT.resizeValue;

/** The value range the form accepts for a resize mode. */
export const resizeRange = (mode: ResizeMode) =>
  mode === 'megapixels'
    ? { min: 0.1, max: 500, step: 0.1 }
    : mode === 'percentage'
      ? { min: 1, max: 400, step: 1 }
      : { min: 1, max: 50000, step: 1 };

/** The id the removed inherited export panel used to remember its last settings. */
export const LAST_USED_RECIPE = '__last_used__';

/** Saved recipes (Capture One's process recipes, Lightroom's export presets). */
export const savedRecipes = (presets: ExportPreset[] | undefined) =>
  (presets ?? []).filter((preset) => preset.id !== LAST_USED_RECIPE);

const formatId = (extension: string) =>
  FILE_FORMATS.find((format) => format.extensions.includes(extension))?.id ?? 'jpeg';

const pick = <T extends string>(value: string | undefined, allowed: readonly T[], fallback: T): T =>
  allowed.includes(value as T) ? (value as T) : fallback;

const finite = (value: number | undefined, fallback: number) =>
  typeof value === 'number' && Number.isFinite(value) ? value : fallback;

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

const hex = (value: string | undefined, fallback: string) =>
  value && /^#[0-9a-fA-F]{6}$/.test(value) ? value.toLowerCase() : fallback;

/** A recipe from the form. */
export function presetFromDraft(draft: RecipeDraft, id: string, name: string, base?: ExportPreset): ExportPreset {
  return {
    ...base,
    id,
    name,
    fileFormat: formatId(draft.format),
    jpegQuality: draft.quality,
    tiffBitDepth: draft.depth,
    colorSpace: draft.colorSpace,
    enableResize: draft.resize,
    resizeMode: draft.resizeMode,
    resizeValue: draft.resizeValue,
    resizeValue2: draft.resizeValue2,
    dontEnlarge: draft.dontEnlarge,
    keepMetadata: draft.metadata,
    stripGps: draft.stripGps,
    stripCreatorContact: !!draft.stripCreatorContact,
    filenameTemplate: draft.template,
    sequenceStart: draft.sequenceStart,
    destinationType:
      draft.where === 'output' ? 'sessionOutput' : draft.where === 'original' ? 'originalFolder' : 'customFolder',
    lastExportPath: draft.where === 'choose' ? (draft.folder ?? base?.lastExportPath) : base?.lastExportPath,
    subfolder: draft.subfolder,
    preserveFolders: draft.preserveFolders,
    preserveTimestamps: draft.preserveTimestamps,
    exportMasks: draft.exportMasks,
    enableWatermark: draft.watermark,
    watermarkPath: draft.watermarkPath,
    watermarkAnchor: draft.watermarkAnchor,
    watermarkScale: draft.watermarkScale,
    watermarkSpacing: draft.watermarkSpacing,
    watermarkOpacity: draft.watermarkOpacity,
    watermarkKind: draft.watermarkKind,
    watermarkText: draft.watermarkText,
    watermarkFont: draft.watermarkFont,
    watermarkColor: draft.watermarkColor,
    watermarkShadow: draft.watermarkShadow,
    enableBorder: draft.border,
    borderBasis: draft.borderBasis,
    borderHorizontalPercent: draft.borderHorizontal,
    borderVerticalPercent: draft.borderVertical,
    borderColor: draft.borderColor,
    enablePad: draft.pad,
    padRatioWidth: draft.padWidth,
    padRatioHeight: draft.padHeight,
    padColor: draft.padColor,
    outputSharpening: {
      mode: draft.sharpening,
      amount: draft.sharpenAmount,
      radius: draft.sharpenRadius,
      threshold: draft.sharpenThreshold,
      distanceCm: draft.printDistanceCm,
      ppi: draft.printPpi,
    },
  };
}

/** The form for a recipe; settings an older recipe lacks get the form's defaults. */
export function draftFromPreset(preset: ExportPreset): RecipeDraft {
  const d = DEFAULT_RECIPE_DRAFT;
  // A CUBE recipe saved by the removed inherited panel still exports LUTs.
  const format = FILE_FORMATS.find((f) => f.id === preset.fileFormat)?.extensions[0] ?? 'jpg';
  return {
    format,
    quality: finite(preset.jpegQuality, d.quality),
    depth: preset.tiffBitDepth === 8 ? 8 : 16,
    colorSpace: pick(preset.colorSpace, COLOR_SPACES, d.colorSpace),
    resize: !!preset.enableResize,
    resizeMode: pick(preset.resizeMode, RESIZE_MODES, d.resizeMode),
    resizeValue: preset.resizeValue > 0 ? preset.resizeValue : d.resizeValue,
    resizeValue2: (preset.resizeValue2 ?? 0) > 0 ? preset.resizeValue2! : d.resizeValue2,
    dontEnlarge: preset.dontEnlarge ?? d.dontEnlarge,
    metadata: !!preset.keepMetadata,
    stripGps: !!preset.stripGps,
    stripCreatorContact: !!preset.stripCreatorContact,
    template: preset.filenameTemplate || '{original_filename}',
    sequenceStart: validSequenceStart(preset.sequenceStart ?? 1) ? (preset.sequenceStart ?? 1) : 1,
    where:
      preset.destinationType === 'sessionOutput'
        ? 'output'
        : preset.destinationType === 'originalFolder'
          ? 'original'
          : 'choose',
    folder: preset.lastExportPath,
    subfolder: preset.subfolder ?? d.subfolder,
    preserveFolders: preset.preserveFolders ?? d.preserveFolders,
    preserveTimestamps: preset.preserveTimestamps ?? d.preserveTimestamps,
    exportMasks: preset.exportMasks ?? d.exportMasks,
    watermark: !!preset.enableWatermark,
    watermarkPath: preset.watermarkPath ?? null,
    watermarkAnchor: pick(preset.watermarkAnchor, Object.values(WatermarkAnchor), d.watermarkAnchor),
    watermarkScale: finite(preset.watermarkScale, d.watermarkScale),
    watermarkSpacing: finite(preset.watermarkSpacing, d.watermarkSpacing),
    watermarkOpacity: finite(preset.watermarkOpacity, d.watermarkOpacity),
    watermarkKind: pick(preset.watermarkKind, ['image', 'text'] as const, d.watermarkKind),
    watermarkText: preset.watermarkText ?? d.watermarkText,
    watermarkFont: preset.watermarkFont ?? d.watermarkFont,
    watermarkColor: hex(preset.watermarkColor, d.watermarkColor),
    watermarkShadow: preset.watermarkShadow ?? d.watermarkShadow,
    border: !!preset.enableBorder,
    borderBasis: pick(preset.borderBasis, Object.values(BorderBasis), d.borderBasis),
    borderHorizontal: finite(preset.borderHorizontalPercent, d.borderHorizontal),
    borderVertical: finite(preset.borderVerticalPercent, d.borderVertical),
    borderColor: hex(preset.borderColor, d.borderColor),
    pad: !!preset.enablePad,
    padWidth: finite(preset.padRatioWidth, d.padWidth),
    padHeight: finite(preset.padRatioHeight, d.padHeight),
    padColor: hex(preset.padColor, d.padColor),
    sharpening: pick(preset.outputSharpening?.mode, ['off', 'screen', 'print'] as const, d.sharpening),
    sharpenAmount: finite(preset.outputSharpening?.amount, d.sharpenAmount),
    sharpenRadius: finite(preset.outputSharpening?.radius, d.sharpenRadius),
    sharpenThreshold: finite(preset.outputSharpening?.threshold, d.sharpenThreshold),
    printDistanceCm: finite(preset.outputSharpening?.distanceCm, d.printDistanceCm),
    printPpi: finite(preset.outputSharpening?.ppi, d.printPpi),
  };
}

/** What `export_images` takes for the form. */
export function exportSettingsFromDraft(draft: RecipeDraft): { settings: ExportSettings; outputFormat: string } {
  const range = resizeRange(draft.resizeMode);
  const ratio = (value: number) => (value > 0 ? Math.min(value, 1000) : 1);
  return {
    outputFormat: draft.format,
    settings: {
      filenameTemplate: draft.template,
    sequenceStart: draft.sequenceStart,
      jpegQuality: clamp(Math.round(draft.quality), 1, 100),
      tiffBitDepth: draft.depth,
      colorSpace: draft.colorSpace,
      keepMetadata: draft.metadata,
      preserveTimestamps: draft.preserveTimestamps,
      stripGps: draft.stripGps,
      stripCreatorContact: !!draft.stripCreatorContact,
      resize: draft.resize
        ? {
            mode: draft.resizeMode,
            value: clamp(finite(draft.resizeValue, defaultResizeValue(draft.resizeMode)), range.min, range.max),
            dontEnlarge: draft.dontEnlarge,
            ...(draft.resizeMode === 'dimensions'
              ? { secondValue: clamp(finite(draft.resizeValue2, DEFAULT_RECIPE_DRAFT.resizeValue2), range.min, range.max) }
              : {}),
          }
        : null,
      border: draft.border
        ? {
            basis: draft.borderBasis,
            horizontalPercent: clamp(finite(draft.borderHorizontal, 0), 0, 100),
            verticalPercent: clamp(finite(draft.borderVertical, 0), 0, 100),
            color: draft.borderColor,
          }
        : null,
      pad: draft.pad
        ? { ratioWidth: ratio(draft.padWidth), ratioHeight: ratio(draft.padHeight), color: draft.padColor }
        : null,
      watermark: watermarkSettings(draft),
      exportMasks: draft.exportMasks,
      // Beside the original the photograph's own folder is already kept.
      preserveFolders: draft.where !== 'original' && draft.preserveFolders,
      destinationType: draft.where === 'original' ? 'originalFolder' : 'customFolder',
      subfolder: draft.subfolder.trim(),
      outputSharpening:
        draft.sharpening === 'off'
          ? null
          : {
              amount: clamp(finite(draft.sharpenAmount, 100), 0, 500),
              radius:
                draft.sharpening === 'print'
                  ? printRadius(clamp(finite(draft.printDistanceCm, 50), 10, 1000), clamp(finite(draft.printPpi, 300), 50, 2400))
                  : clamp(finite(draft.sharpenRadius, 0.8), 0.2, 10),
              threshold: clamp(finite(draft.sharpenThreshold, 0), 0, 50),
            },
    },
  };
}

/** Whether the form's watermark has what it needs (an image, or text). */
export const watermarkReady = (draft: RecipeDraft) =>
  draft.watermarkKind === 'text' ? draft.watermarkText.trim().length > 0 : !!draft.watermarkPath;

function watermarkSettings(draft: RecipeDraft): ExportSettings['watermark'] {
  if (!draft.watermark || !watermarkReady(draft)) return null;
  const placement = {
    anchor: draft.watermarkAnchor,
    scale: clamp(finite(draft.watermarkScale, 10), 1, 100),
    spacing: clamp(finite(draft.watermarkSpacing, 5), 0, 25),
    opacity: clamp(finite(draft.watermarkOpacity, 75), 0, 100),
  };
  return draft.watermarkKind === 'text'
    ? {
        ...placement,
        path: '',
        text: {
          text: draft.watermarkText,
          fontFamily: draft.watermarkFont,
          color: draft.watermarkColor,
          shadow: draft.watermarkShadow,
        },
      }
    : { ...placement, path: draft.watermarkPath! };
}

/** What `export_images` takes for a recipe. */
export function exportSettingsFromPreset(preset: ExportPreset): { settings: ExportSettings; outputFormat: string } {
  return exportSettingsFromDraft(draftFromPreset(preset));
}

/** Why a subfolder name cannot be used, or null. */
export function subfolderProblem(subfolder: string): "outside" | "fullPath" | null {
  const parts = subfolder.trim().replace(/^[/\\]+/, '').split(/[/\\]/);
  if (parts.some((part) => part === '..')) return 'outside';
  if (/^[a-zA-Z]:/.test(subfolder.trim())) return 'fullPath';
  return null;
}
