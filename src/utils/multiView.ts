/**
 * Develop's multi view: the selected photographs side by side (Capture One's
 * Multi view, Lightroom's Survey). Layout maths, captions and navigation; the
 * native display draws the photographs (src-tauri/src/multi_view.rs).
 */
import { COPYABLE_ADJUSTMENT_KEYS, sameAdjustmentValue, type Adjustments } from './adjustments';

/** At most this many cells; with more photographs selected the last cell
 * says how many more there are (Capture One shows 12). */
export const MAX_CELLS = 12;
/** Space between cells and around the grid (CSS px). */
export const CELL_GAP = 12;
export const GRID_PADDING = 8;

export type CaptionLevel = 'narrow' | 'medium' | 'wide';

/** Captions grow with the cell: one capture value and the file name in a
 * narrow cell; ISO, shutter, aperture, name and rating in a medium one (a
 * 2-up); the focal length as well in a wide one (Capture One). */
export function captionLevel(cellWidth: number): CaptionLevel {
  if (cellWidth < 260) return 'narrow';
  if (cellWidth < 520) return 'medium';
  return 'wide';
}

/** Caption height under the photograph (CSS px). */
export function captionHeight(level: CaptionLevel): number {
  return level === 'narrow' ? 20 : 36;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface GridCell {
  /** The whole cell: photograph and caption (the frame goes around it). */
  cell: Rect;
  /** Where the photograph is shown, contained and centred. */
  photo: Rect;
  level: CaptionLevel;
}

export interface Grid {
  columns: number;
  rows: number;
  cells: GridCell[];
}

function fitArea(width: number, height: number, aspect: number): number {
  if (width <= 0 || height <= 0) return 0;
  const fitWidth = Math.min(width, height * aspect);
  return fitWidth * (fitWidth / aspect);
}

/**
 * Packs `count` cells into a `width` × `height` viewer: the rows and
 * columns that show the photographs (of `aspect`, width / height) largest,
 * the last row centred.
 */
export function packGrid(count: number, width: number, height: number, aspect = 1.5): Grid {
  const n = Math.max(0, Math.min(count, MAX_CELLS));
  if (n === 0 || width <= 0 || height <= 0) return { columns: 0, rows: 0, cells: [] };
  const innerW = Math.max(0, width - 2 * GRID_PADDING);
  const innerH = Math.max(0, height - 2 * GRID_PADDING);
  let best = { columns: 1, rows: n, area: -1 };
  for (let columns = 1; columns <= n; columns++) {
    const rows = Math.ceil(n / columns);
    // A column that would stay empty is no better than one fewer.
    if ((columns - 1) * rows >= n) continue;
    const cellW = (innerW - (columns - 1) * CELL_GAP) / columns;
    const cellH = (innerH - (rows - 1) * CELL_GAP) / rows;
    const area = fitArea(cellW, cellH - captionHeight(captionLevel(cellW)), aspect);
    if (area > best.area + 1e-6) best = { columns, rows, area };
  }
  const { columns, rows } = best;
  const cellW = Math.max(0, (innerW - (columns - 1) * CELL_GAP) / columns);
  const cellH = Math.max(0, (innerH - (rows - 1) * CELL_GAP) / rows);
  const level = captionLevel(cellW);
  const caption = captionHeight(level);
  // Each cell hugs a photograph of `aspect` with its caption right below,
  // centred in its slot (another shape is contained in the photo area).
  const areaH = Math.max(0, cellH - caption);
  const photoW = Math.min(cellW, areaH * aspect);
  const photoH = aspect > 0 ? photoW / aspect : 0;
  const cells: GridCell[] = [];
  for (let i = 0; i < n; i++) {
    const row = Math.floor(i / columns);
    const inRow = row === rows - 1 ? n - row * columns : columns;
    const indent = ((columns - inRow) * (cellW + CELL_GAP)) / 2;
    const x = GRID_PADDING + indent + (i % columns) * (cellW + CELL_GAP) + (cellW - photoW) / 2;
    const y = GRID_PADDING + row * (cellH + CELL_GAP) + (cellH - photoH - caption) / 2;
    cells.push({
      cell: { x, y, width: photoW, height: photoH + caption },
      photo: { x, y, width: photoW, height: photoH },
      level,
    });
  }
  return { columns, rows, cells };
}

export interface MultiViewCells {
  /** The photographs given a cell, in selection order. */
  shown: string[];
  /** Photographs selected but not shown (the "+N more" cell). */
  more: number;
}

/**
 * The photographs given a cell: the pinned `reference` first (Capture One's
 * compare variant, not part of the selection), then the selection up to
 * {@link MAX_CELLS}; with more, the first ones and a last cell for the rest,
 * the primary always among those shown.
 */
export function cellsFor(
  selection: string[],
  primary: string | null,
  reference: string | null = null,
): MultiViewCells {
  const rest = reference ? selection.filter((path) => path !== reference) : selection;
  const pinned = reference ? [reference] : [];
  const room = MAX_CELLS - pinned.length;
  if (rest.length <= room) return { shown: [...pinned, ...rest], more: 0 };
  const slots = room - 1;
  let shown = rest.slice(0, slots);
  if (primary && !shown.includes(primary) && rest.includes(primary)) {
    shown = [...rest.slice(0, slots - 1), primary];
  }
  return { shown: [...pinned, ...shown], more: rest.length - shown.length };
}

/** The photograph `step` places from `current` in `selection`, wrapping. */
export function stepPrimary(selection: string[], current: string | null, step: number): string | null {
  if (selection.length === 0) return null;
  const index = current ? selection.indexOf(current) : -1;
  if (index < 0) return selection[0];
  const n = selection.length;
  return selection[(((index + step) % n) + n) % n];
}

/** The selected photographs in the order the browser lists them. */
export function orderedSelection(listed: string[], selected: string[]): string[] {
  const wanted = new Set(selected);
  const ordered = listed.filter((path) => wanted.has(path));
  const known = new Set(ordered);
  return [...ordered, ...selected.filter((path) => !known.has(path))];
}

/** Capture values for captions, formatted as the Metadata tool shows them. */
export interface CaptureValues {
  iso?: string;
  shutter?: string;
  aperture?: string;
  focal?: string;
}

export function captureValues(exif: Record<string, string> | null | undefined): CaptureValues {
  if (!exif) return {};
  const iso = exif.PhotographicSensitivity || exif.ISOSpeedRatings || exif.ISO;
  const shutter = exif.ExposureTime;
  const aperture = exif.FNumber;
  const focal = exif.FocalLength || exif.FocalLengthIn35mmFilm;
  return {
    iso: iso ? `ISO ${iso}` : undefined,
    shutter: shutter ? (String(shutter).endsWith('s') ? String(shutter) : `${shutter} s`) : undefined,
    aperture: aperture ? (String(aperture).toLowerCase().startsWith('f') ? String(aperture) : `f/${aperture}`) : undefined,
    focal: focal ? String(focal).replace(/(\d)$/, '$1 mm') : undefined,
  };
}

/** The capture line of a caption at `level`. */
export function captureLine(level: CaptionLevel, values: CaptureValues): string {
  const parts =
    level === 'narrow'
      ? [values.shutter ?? values.iso ?? values.aperture]
      : level === 'medium'
        ? [values.iso, values.shutter, values.aperture]
        : [values.iso, values.shutter, values.aperture, values.focal];
  return parts.filter(Boolean).join(' · ');
}

export function fileName(path: string): string {
  const name = path.split('?vc=')[0].split(/[\\/]/).pop() ?? path;
  return path.includes('?vc=') ? `${name} ⧉` : name;
}

/**
 * "Edit all selected": what an edit of the primary changed, to apply to the
 * other selected photographs the way pasting adjustments does (the same
 * values, only the copyable keys that changed).
 */
export function changedCopyableKeys(previous: Adjustments, next: Adjustments): Partial<Adjustments> {
  const changed: Record<string, unknown> = {};
  const before = previous as unknown as Record<string, unknown>;
  const after = next as unknown as Record<string, unknown>;
  for (const key of COPYABLE_ADJUSTMENT_KEYS) {
    if (!sameAdjustmentValue(before[key], after[key])) changed[key] = after[key];
  }
  return changed as Partial<Adjustments>;
}

/** The zoom all cells share (Lightroom's locked zoom): device pixels per
 * full-resolution pixel of the shown photograph and the photograph point
 * (0–1 of its shown width and height) at the centre of each cell. */
export interface SharedZoom {
  scale: number;
  center: [number, number];
}

/**
 * The primary's zoom as the editor shows it in its cell: `transform` is the
 * editor's pan/zoom (scale relative to fit, position in CSS px), `fit` the
 * photograph's fit-size placement in its container (CSS px) and `shown` the
 * shown photograph's size in full-resolution pixels. None at fit.
 */
export function sharedZoomOf(
  transform: { scale: number; positionX: number; positionY: number },
  fit: { width: number; height: number; offsetX: number; offsetY: number; containerWidth: number; containerHeight: number },
  shown: { width: number; height: number },
  dpr: number,
): SharedZoom | null {
  if (transform.scale <= 1.01 || fit.width <= 0 || fit.height <= 0 || shown.width <= 0 || shown.height <= 0) {
    return null;
  }
  const visibleX = (fit.containerWidth / 2 - transform.positionX) / transform.scale;
  const visibleY = (fit.containerHeight / 2 - transform.positionY) / transform.scale;
  const clamp = (v: number) => Math.min(1, Math.max(0, v));
  return {
    scale: (fit.width * transform.scale * dpr) / shown.width,
    center: [clamp((visibleX - fit.offsetX) / fit.width), clamp((visibleY - fit.offsetY) / fit.height)],
  };
}

export function sameZoom(a: SharedZoom | null, b: SharedZoom | null): boolean {
  if (a === null || b === null) return a === b;
  return (
    Math.abs(a.scale - b.scale) < 1e-4 &&
    Math.abs(a.center[0] - b.center[0]) < 1e-4 &&
    Math.abs(a.center[1] - b.center[1]) < 1e-4
  );
}
