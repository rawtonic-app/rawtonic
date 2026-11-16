/**
 * Where Develop's filmstrip sits: under the photograph (the strip) or as a
 * vertical grid in a column beside it, on the left or the right. Remembered
 * with the app settings (`uiVisibility.filmstripDock`).
 */
export type DockSide = 'bottom' | 'left' | 'right';

export interface FilmstripDock {
  side: DockSide;
  /** File names under the thumbnails of the docked grid. */
  names: boolean;
  /** Width of a docked thumbnail cell (px): Ctrl/⌘ + wheel. */
  size: number;
  /** Width of the docked column (px). */
  width: number;
}

export const DOCK_SIZE = { min: 56, max: 200, step: 12 } as const;
export const DOCK_WIDTH = { min: 150, max: 440 } as const;
export const DEFAULT_DOCK: FilmstripDock = { side: 'bottom', names: false, size: 88, width: 230 };

const clamp = (value: number, min: number, max: number) => Math.max(min, Math.min(max, value));

/** The stored dock; missing or unreadable parts take the defaults. */
export function readFilmstripDock(stored: unknown): FilmstripDock {
  const entry = stored && typeof stored === 'object' ? (stored as Record<string, unknown>) : {};
  const number = (value: unknown, fallback: number, min: number, max: number) =>
    typeof value === 'number' && Number.isFinite(value) ? clamp(Math.round(value), min, max) : fallback;
  return {
    side: entry.side === 'left' || entry.side === 'right' || entry.side === 'bottom' ? entry.side : DEFAULT_DOCK.side,
    names: typeof entry.names === 'boolean' ? entry.names : DEFAULT_DOCK.names,
    size: number(entry.size, DEFAULT_DOCK.size, DOCK_SIZE.min, DOCK_SIZE.max),
    width: number(entry.width, DEFAULT_DOCK.width, DOCK_WIDTH.min, DOCK_WIDTH.max)
  };
}

/** The thumbnail size after a wheel turn: a turn up (negative `deltaY`)
 * enlarges, in `DOCK_SIZE.step` per notch of about 100 px, at least one step. */
export function wheelThumbnailSize(size: number, deltaY: number): number {
  if (!deltaY) return size;
  const notches = Math.max(1, Math.round(Math.abs(deltaY) / 100));
  return clamp(size + (deltaY < 0 ? 1 : -1) * notches * DOCK_SIZE.step, DOCK_SIZE.min, DOCK_SIZE.max);
}

/** Where a header dropped at (x, y) in `area` docks: the outer quarter on a
 * side takes the left or right column, the lower fifth the strip; elsewhere
 * (the photograph) `null` keeps the place. */
export function dockSideAt(x: number, y: number, area: { left: number; top: number; width: number; height: number }) {
  if (area.width <= 0 || area.height <= 0) return null;
  const fx = (x - area.left) / area.width;
  const fy = (y - area.top) / area.height;
  if (fx < 0 || fx > 1 || fy < 0 || fy > 1) return null;
  if (fy > 0.8) return 'bottom' as const;
  if (fx < 0.25) return 'left' as const;
  if (fx > 0.75) return 'right' as const;
  return null;
}

/** A tile row's height for a docked cell of `width` px. */
export function dockExtent(width: number, names: boolean): number {
  return Math.round(width * 0.75) + (names ? 20 : 0) + 8;
}
