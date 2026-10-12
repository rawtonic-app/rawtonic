import { useEffect, useRef, useState, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEditorStore } from "../../store/useEditorStore";
import { useMultiViewStore, type CellZoomKey } from "../../store/useMultiViewStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { Invokes } from "../ui/AppProperties";
import type { TransformWrapperHandle } from "../panel/Editor";
import {
  cycleCellZoom,
  fitScaleOf,
  panCell,
  sameZoom,
  sharedZoomOf,
  transformOfZoom,
  zoomCell,
  type SharedZoom,
} from "../../utils/multiView";

/** A cell's photograph area in CSS px (window coordinates). */
export interface CellArea {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface Options {
  active: boolean;
  primary: string | null;
  /** The cells shown, in order, and their photograph areas. */
  shown: string[];
  areas: Map<string, CellArea>;
  /** The primary's shown size (full-resolution px). */
  shownSize: { width: number; height: number };
  transformWrapperRef?: RefObject<TransformWrapperHandle | null>;
}

const dprNow = () => (typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1);

/** Pointer travel (CSS px) after which a press on a zoomed cell pans it. */
const DRAG_SLOP = 4;

/** What the native side is told: the shared zoom (lock on) or each other
 * cell's own zoom (lock off). */
interface ZoomPayload {
  zoom: SharedZoom | null;
  cells: Record<string, SharedZoom> | null;
}

function samePayload(a: ZoomPayload | undefined, b: ZoomPayload): boolean {
  if (!a || !sameZoom(a.zoom, b.zoom) || (a.cells === null) !== (b.cells === null)) return false;
  if (!a.cells || !b.cells) return true;
  const keys = Object.keys(b.cells);
  return (
    keys.length === Object.keys(a.cells).length &&
    keys.every((key) => key in a.cells! && sameZoom(a.cells![key], b.cells![key]))
  );
}

/** Whether the editor's fit box belongs to a photograph of `shown` size (it
 * is measured a frame after the photograph changes). */
function fitMatches(fit: { width: number; height: number }, shown: { width: number; height: number }) {
  if (fit.width <= 0 || fit.height <= 0 || shown.width <= 0 || shown.height <= 0) return false;
  const a = fit.width / fit.height;
  const b = shown.width / shown.height;
  return Math.abs(a - b) <= 0.02 * Math.max(a, b);
}

/**
 * Zoom in the multi view. With the zoom lock on (Lightroom's lock) every
 * other cell shows the primary's zoom and pans with it. Off, each cell zooms
 * on its own: the wheel, a pinch, a drag (when zoomed) and the zoom keys act
 * on the cell under the pointer, each cell keeps its zoom and pan, and a
 * photograph that becomes the primary keeps the zoom its cell had. The
 * native side places every cell and develops its visible region at 1:1.
 * Returns each cell's zoom (null: fit) for the zoom badges, and the pointer
 * handlers of the other cells.
 */
export function useMultiViewZoom({ active, primary, shown, areas, shownSize, transformWrapperRef }: Options) {
  const zoomLock = useMultiViewStore((s) => s.zoomLock);
  const cellZooms = useMultiViewStore((s) => s.cellZooms);
  const cellSizes = useMultiViewStore((s) => s.cellSizes);
  const [primaryZoom, setPrimaryZoom] = useState<SharedZoom | null>(null);

  const live = useRef({ primary, shown, areas, shownSize, zoomLock });
  live.current = { primary, shown, areas, shownSize, zoomLock };
  /** The primary's zoom as last read from the editor (null: fit). */
  const primaryZoomRef = useRef<SharedZoom | null>(null);
  /** The primary whose zoom the editor shows (after a switch without the
   * lock: once its own zoom is applied). */
  const adopted = useRef<string | null>(primary);
  /** The secondary under the pointer and where (CSS px, window). */
  const hover = useRef<{ path: string; x: number; y: number } | null>(null);

  // The secondaries' shown sizes come from the native side as it develops them.
  useEffect(() => {
    if (!active || useSettingsStore.getState().osPlatform === 'linux') return;
    let stop: (() => void) | undefined;
    let cancelled = false;
    listen<{ path: string; width: number; height: number }[]>("multi-view-cell-sizes", (event) =>
      useMultiViewStore.getState().setCellSizes(event.payload),
    )
      .then((unlisten) => {
        if (cancelled) unlisten();
        else stop = unlisten;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      stop?.();
    };
  }, [active]);

  // Stepping to another primary without the lock: the previous one keeps its
  // zoom as its cell's; the next one shows its own (or fit).
  const previousPrimary = useRef(primary);
  useEffect(() => {
    const before = previousPrimary.current;
    previousPrimary.current = primary;
    if (!active || before === primary) return;
    if (live.current.zoomLock) {
      adopted.current = primary;
      return;
    }
    const store = useMultiViewStore.getState();
    if (before && live.current.shown.includes(before)) store.setCellZoom(before, primaryZoomRef.current);
    primaryZoomRef.current = null;
    setPrimaryZoom(null);
    if (primary && store.cellZooms[primary]) {
      adopted.current = null; // applied once the editor knows its fit
    } else {
      transformWrapperRef?.current?.resetTransform(0);
      adopted.current = primary;
    }
  }, [active, primary, transformWrapperRef]);

  // Unlocking keeps what every cell shows: each takes the shared zoom.
  const wasLocked = useRef(zoomLock);
  useEffect(() => {
    const before = wasLocked.current;
    wasLocked.current = zoomLock;
    if (!active || zoomLock || !before) return;
    adopted.current = live.current.primary;
    const shared = primaryZoomRef.current;
    if (!shared) return;
    useMultiViewStore
      .getState()
      .setCellZooms(
        Object.fromEntries(
          live.current.shown.filter((path) => path !== live.current.primary).map((path) => [path, shared]),
        ),
      );
  }, [active, zoomLock]);

  // Read the primary's zoom each frame; tell the native side when anything
  // changes.
  useEffect(() => {
    if (!active) return;
    let sent: ZoomPayload | undefined;
    let frame = 0;
    const tick = () => {
      frame = requestAnimationFrame(tick);
      const { primary, shown, shownSize, zoomLock } = live.current;
      const editor = useEditorStore.getState();
      const transform = transformWrapperRef?.current?.instance.transformState;
      const known =
        !!transform &&
        !!primary &&
        editor.selectedImage?.path === primary &&
        !!editor.selectedImage.isReady &&
        fitMatches(editor.baseRenderSize, shownSize);
      const store = useMultiViewStore.getState();
      if (known && !zoomLock && adopted.current !== primary) {
        // The new primary's own zoom, now that its fit is measured.
        const own = store.cellZooms[primary!];
        const target = own && transformOfZoom(own, editor.baseRenderSize, shownSize, dprNow());
        if (target) transformWrapperRef?.current?.setTransform(target.positionX, target.positionY, target.scale, 0);
        adopted.current = primary;
        return;
      }
      if (known && (zoomLock || adopted.current === primary)) {
        const zoom = sharedZoomOf(transform!, editor.baseRenderSize, shownSize, dprNow());
        if (!sameZoom(primaryZoomRef.current, zoom)) {
          primaryZoomRef.current = zoom;
          setPrimaryZoom(zoom);
        }
      }
      const payload: ZoomPayload = zoomLock
        ? { zoom: primaryZoomRef.current, cells: null }
        : {
            zoom: null,
            cells: Object.fromEntries(
              Object.entries(store.cellZooms).filter(([path]) => path !== primary && shown.includes(path)),
            ),
          };
      if (!samePayload(sent, payload)) {
        sent = payload;
        if (useSettingsStore.getState().osPlatform !== 'linux') {
          void invoke(Invokes.SetMultiViewZoom, { zoom: payload.zoom, cells: payload.cells })?.catch(() => {});
        }
      }
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [active, transformWrapperRef]);

  /** Zooms or pans the other cell `path` (lock off); false when it cannot
   * (its size is not known yet). */
  const changeCell = (path: string, change: (zoom: SharedZoom | null, area: CellArea, size: { width: number; height: number }) => SharedZoom | null) => {
    const area = live.current.areas.get(path);
    const size = useMultiViewStore.getState().cellSizes[path];
    if (!area || !size) return false;
    const current = useMultiViewStore.getState().cellZooms[path] ?? null;
    useMultiViewStore.getState().setCellZoom(path, change(current, area, size));
    return true;
  };
  const pointerIn = (area: CellArea, x: number, y: number): [number, number] => [
    x - (area.x + area.width / 2),
    y - (area.y + area.height / 2),
  ];

  // The zoom keys over another cell (lock off) zoom that cell.
  useEffect(() => {
    if (!active || zoomLock) return;
    const handler = (kind: CellZoomKey) => {
      const at = hover.current;
      if (!at || at.path === live.current.primary) return false;
      return changeCell(at.path, (zoom, area, size) => {
        const dpr = dprNow();
        const pointer = pointerIn(area, at.x, at.y);
        const scale = zoom?.scale ?? fitScaleOf(area, size, dpr);
        switch (kind) {
          case "cycle":
            return cycleCellZoom(zoom, pointer, area, size, dpr);
          case "fit":
            return null;
          case "actual":
            return zoomCell(zoom, 1 / scale, pointer, area, size, dpr);
          case "in":
            return zoomCell(zoom, 1.2, pointer, area, size, dpr);
          case "out":
            return zoomCell(zoom, 1 / 1.2, pointer, area, size, dpr);
          case "stepIn":
            return zoomCell(zoom, (scale + 0.1) / scale, pointer, area, size, dpr);
          case "stepOut":
            return zoomCell(zoom, Math.max(0.01, scale - 0.1) / scale, pointer, area, size, dpr);
        }
      });
    };
    useMultiViewStore.setState({ zoomHoveredCell: handler });
    return () => {
      if (useMultiViewStore.getState().zoomHoveredCell === handler) useMultiViewStore.setState({ zoomHoveredCell: null });
    };
    // changeCell reads everything through refs and the stores.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, zoomLock]);

  /** A drag in progress on a zoomed cell. */
  const drag = useRef<{ path: string; x: number; y: number; panned: boolean; id: number } | null>(null);
  /** The click that ends a pan does not make the cell the primary. */
  const swallowClick = useRef(false);

  /** Pointer and wheel handlers for another photograph's cell. */
  const cellHandlers = (path: string) => ({
    onPointerEnter: (e: React.PointerEvent) => {
      hover.current = { path, x: e.clientX, y: e.clientY };
    },
    onPointerMove: (e: React.PointerEvent) => {
      hover.current = { path, x: e.clientX, y: e.clientY };
      const d = drag.current;
      if (!d || d.path !== path || d.id !== e.pointerId) return;
      const dx = e.clientX - d.x;
      const dy = e.clientY - d.y;
      if (!d.panned && Math.hypot(dx, dy) < DRAG_SLOP) return;
      d.panned = true;
      d.x = e.clientX;
      d.y = e.clientY;
      changeCell(path, (zoom, area, size) => (zoom ? panCell(zoom, dx, dy, area, size, dprNow()) : zoom));
    },
    onPointerLeave: () => {
      if (hover.current?.path === path) hover.current = null;
    },
    onPointerDown: (e: React.PointerEvent) => {
      swallowClick.current = false;
      if (live.current.zoomLock || e.button !== 0 || !useMultiViewStore.getState().cellZooms[path]) return;
      drag.current = { path, x: e.clientX, y: e.clientY, panned: false, id: e.pointerId };
      (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
    },
    onPointerUp: () => {
      if (drag.current?.panned) swallowClick.current = true;
      drag.current = null;
    },
    /** Whether a click is the end of a pan (and must not open the cell). */
    panned: () => {
      const was = swallowClick.current;
      swallowClick.current = false;
      return was;
    },
  });

  /** The wheel over another cell (lock off): zoom (mouse wheel, pinch) or
   * pan (trackpad scroll, Shift/Option + wheel), as the editor does. */
  const onWheel = (path: string, e: WheelEvent) => {
    if (live.current.zoomLock || path === live.current.primary) return false;
    const { appSettings } = useSettingsStore.getState();
    const trackpad = appSettings?.canvasInputMode === "trackpad";
    const zoomIntent = e.ctrlKey || (!trackpad && !e.shiftKey && !e.altKey);
    const delta = e.deltaY !== 0 ? e.deltaY : e.deltaX;
    return changeCell(path, (zoom, area, size) => {
      const dpr = dprNow();
      if (zoomIntent) {
        const speed = (appSettings?.zoomSpeedMultiplier ?? 1) * (trackpad ? 5 : 1);
        return zoomCell(zoom, Math.exp(-delta * 0.002 * speed), pointerIn(area, e.clientX, e.clientY), area, size, dpr);
      }
      if (!zoom) return zoom;
      let dx = e.deltaX;
      let dy = e.deltaY;
      if (!trackpad && e.shiftKey) [dx, dy] = [delta, e.altKey ? delta : 0];
      else if (!trackpad && e.altKey) [dx, dy] = [0, delta];
      return panCell(zoom, -dx, -dy, area, size, dpr);
    });
  };

  // Each cell's zoom for the badges: the primary's own, the shared one with
  // the lock, else the cell's own.
  const zoomOf = (path: string): SharedZoom | null =>
    path === primary ? primaryZoom : zoomLock ? primaryZoom : (cellZooms[path] ?? null);

  return { zoomOf, cellHandlers, onWheel, cellSizes };
}
