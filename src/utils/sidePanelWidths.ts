import { DOCK_WIDTH } from './filmstripDock';

/** The tool column's narrowest width (its own, Interface Size zoomed px). */
export const TOOLS_MIN_WIDTH = 270;

/** The photograph's room beside the side columns: 320 px, or 35 % of a
 * smaller window. */
export function viewerMinWidth(windowWidth: number): number {
  return Math.min(320, Math.round(windowWidth * 0.35));
}

/**
 * The widths the tool column and a docked filmstrip are drawn at, and the
 * widest each may take now, so the photograph keeps `viewerMinWidth`. The
 * tool column comes first (down to the dock's minimum); the dock takes what
 * is left. Each shows its stored width while it fits, and keeps it for a
 * wider window. Tool widths are in the column's zoomed px (`uiScale`), the
 * dock's and the window's in CSS px; `beside` is other side columns' width.
 */
export function sidePanelWidths({
  windowWidth,
  uiScale,
  tools,
  toolsMax,
  dock,
  beside = 0,
  toolsInFlow = true
}: {
  windowWidth: number;
  uiScale: number;
  tools: number;
  toolsMax: number;
  /** The docked filmstrip's stored width, or null without a side dock. */
  dock: number | null;
  beside?: number;
  /** False while the tool column is hidden or shown over the photograph. */
  toolsInFlow?: boolean;
}) {
  const scale = uiScale || 1;
  const room = windowWidth - viewerMinWidth(windowWidth) - beside;
  const dockMin = dock === null ? 0 : DOCK_WIDTH.min;
  const toolsCap = Math.max(TOOLS_MIN_WIDTH, Math.min(toolsMax, Math.floor((room - dockMin) / scale)));
  const toolsShown = Math.max(TOOLS_MIN_WIDTH, Math.min(toolsCap, tools));
  const dockCap = Math.max(
    DOCK_WIDTH.min,
    Math.min(DOCK_WIDTH.max, Math.floor(room - (toolsInFlow ? toolsShown * scale : 0)))
  );
  const dockShown = dock === null ? null : Math.max(DOCK_WIDTH.min, Math.min(dockCap, dock));
  return { tools: toolsShown, toolsCap, dock: dockShown, dockCap };
}

/** The stored width after a separator step of `delta` from the `shown`
 * width (at most `cap`): a step that cannot move it keeps `stored`, so a
 * width chosen in a wider window comes back there. */
export function steppedWidth(stored: number, shown: number, delta: number, min: number, cap: number): number {
  const next = Math.max(min, Math.min(cap, shown + delta));
  return next === shown ? stored : next;
}
