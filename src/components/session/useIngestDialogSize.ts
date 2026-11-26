import { useEffect, useState, type PointerEvent } from 'react';
import { useSettingsStore } from '../../store/useSettingsStore';

export interface IngestDialogSize {
  width: number;
  height: number;
  maximized: boolean;
}
/** A resizable dialog's limits: its smallest size and its default (a
 * fraction of the window up to a cap). */
export interface DialogSizeLimits {
  minWidth: number;
  minHeight: number;
  maxDefaultWidth: number;
  maxDefaultHeight: number;
  defaultFraction: number;
}
export const INGEST_DIALOG_LIMITS: DialogSizeLimits = {
  minWidth: 760,
  minHeight: 520,
  maxDefaultWidth: 1600,
  maxDefaultHeight: 1000,
  defaultFraction: 0.9
};
/** Export opens at its former fixed size (860 × 790) and can grow. */
export const EXPORT_DIALOG_LIMITS: DialogSizeLimits = {
  minWidth: 640,
  minHeight: 480,
  maxDefaultWidth: 860,
  maxDefaultHeight: 790,
  defaultFraction: 0.9
};
export function clampIngestSize(
  size: IngestDialogSize | undefined,
  viewport: { width: number; height: number },
  limits: DialogSizeLimits = INGEST_DIALOG_LIMITS
): IngestDialogSize {
  const maxWidth = Math.max(1, viewport.width - 24);
  const maxHeight = Math.max(1, viewport.height - 24);
  return {
    width: Math.min(
      maxWidth,
      Math.max(
        Math.min(limits.minWidth, maxWidth),
        Number.isFinite(size?.width)
          ? size!.width
          : Math.min(limits.maxDefaultWidth, viewport.width * limits.defaultFraction)
      )
    ),
    height: Math.min(
      maxHeight,
      Math.max(
        Math.min(limits.minHeight, maxHeight),
        Number.isFinite(size?.height)
          ? size!.height
          : Math.min(limits.maxDefaultHeight, viewport.height * limits.defaultFraction)
      )
    ),
    maximized: size?.maximized === true
  };
}
export const INGEST_RESIZE_EDGES = ['n', 's', 'e', 'w', 'ne', 'nw', 'se', 'sw'] as const;
export const useIngestDialogSize = () => useDialogSize('ingestDialogSize', INGEST_DIALOG_LIMITS);
/** A dialog's size, resized from its edges and maximized from its title bar,
 * remembered in the settings under `key`. */
export function useDialogSize(
  key: 'ingestDialogSize' | 'exportDialogSize',
  limits: DialogSizeLimits = INGEST_DIALOG_LIMITS
) {
  const stored = useSettingsStore((s) => s.appSettings?.[key]);
  const [viewport, setViewport] = useState({ width: window.innerWidth, height: window.innerHeight });
  const [dragSize, setDragSize] = useState<IngestDialogSize>();
  useEffect(() => {
    const resize = () => setViewport({ width: window.innerWidth, height: window.innerHeight });
    window.addEventListener('resize', resize);
    return () => window.removeEventListener('resize', resize);
  }, []);
  const size = clampIngestSize(dragSize ?? stored, viewport, limits);
  const save = (next: IngestDialogSize) => {
    const state = useSettingsStore.getState();
    if (state.appSettings) void state.handleSettingsChange({ ...state.appSettings, [key]: next });
  };
  const toggle = () => save({ ...(stored ?? size), maximized: !size.maximized });
  const start = (event: PointerEvent<HTMLElement>, edge: string) => {
    if (size.maximized || event.button !== 0) return;
    event.preventDefault();
    const element = event.currentTarget;
    element.setPointerCapture(event.pointerId);
    const x = event.clientX,
      y = event.clientY;
    let next = size;
    const move = (e: globalThis.PointerEvent) => {
      next = clampIngestSize(
        {
          ...size,
          width:
            size.width + (edge.includes('e') ? 2 * (e.clientX - x) : edge.includes('w') ? -2 * (e.clientX - x) : 0),
          height:
            size.height + (edge.includes('s') ? 2 * (e.clientY - y) : edge.includes('n') ? -2 * (e.clientY - y) : 0)
        },
        viewport,
        limits
      );
      setDragSize(next);
    };
    const end = () => {
      element.removeEventListener('pointermove', move);
      element.removeEventListener('pointerup', end);
      element.removeEventListener('pointercancel', end);
      if (next !== size) save(next);
      setDragSize(undefined);
    };
    element.addEventListener('pointermove', move);
    element.addEventListener('pointerup', end);
    element.addEventListener('pointercancel', end);
  };
  return {
    size,
    toggle,
    start,
    style: {
      width: size.maximized ? viewport.width - 24 : size.width,
      height: size.maximized ? viewport.height - 24 : size.height
    }
  };
}
