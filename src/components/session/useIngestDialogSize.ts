import { useEffect, useState, type PointerEvent } from 'react';
import { useSettingsStore } from '../../store/useSettingsStore';

export interface IngestDialogSize {
  width: number;
  height: number;
  maximized: boolean;
}
export function clampIngestSize(
  size: IngestDialogSize | undefined,
  viewport: { width: number; height: number }
): IngestDialogSize {
  const maxWidth = Math.max(1, viewport.width - 24);
  const maxHeight = Math.max(1, viewport.height - 24);
  return {
    width: Math.min(
      maxWidth,
      Math.max(
        Math.min(760, maxWidth),
        Number.isFinite(size?.width) ? size!.width : Math.min(1600, viewport.width * 0.9)
      )
    ),
    height: Math.min(
      maxHeight,
      Math.max(
        Math.min(520, maxHeight),
        Number.isFinite(size?.height) ? size!.height : Math.min(1000, viewport.height * 0.9)
      )
    ),
    maximized: size?.maximized === true
  };
}
export const INGEST_RESIZE_EDGES = ['n', 's', 'e', 'w', 'ne', 'nw', 'se', 'sw'] as const;
export function useIngestDialogSize() {
  const stored = useSettingsStore((s) => s.appSettings?.ingestDialogSize);
  const [viewport, setViewport] = useState({ width: window.innerWidth, height: window.innerHeight });
  const [dragSize, setDragSize] = useState<IngestDialogSize>();
  useEffect(() => {
    const resize = () => setViewport({ width: window.innerWidth, height: window.innerHeight });
    window.addEventListener('resize', resize);
    return () => window.removeEventListener('resize', resize);
  }, []);
  const size = clampIngestSize(dragSize ?? stored, viewport);
  const save = (next: IngestDialogSize) => {
    const state = useSettingsStore.getState();
    if (state.appSettings) void state.handleSettingsChange({ ...state.appSettings, ingestDialogSize: next });
  };
  const toggle = () => save({ ...size, maximized: !size.maximized });
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
        viewport
      );
      setDragSize(next);
    };
    const end = () => {
      element.removeEventListener('pointermove', move);
      element.removeEventListener('pointerup', end);
      element.removeEventListener('pointercancel', end);
      save(next);
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
