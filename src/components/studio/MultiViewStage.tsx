import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Flag, Pin, PinOff, Star, X } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useProcessStore } from "../../store/useProcessStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import { useMultiViewStore } from "../../store/useMultiViewStore";
import { useSessionBar } from "../session/sessionBar";
import { Invokes, type ImageFile } from "../ui/AppProperties";
import type { TransformWrapperHandle } from "../panel/Editor";
import {
  captureLine,
  captureValues,
  cellsFor,
  fileName,
  orderedSelection,
  packGrid,
  stepPrimary,
  type GridCell,
} from "../../utils/multiView";
import { useMultiViewZoom } from "./useMultiViewZoom";
import { useEditAllSelected } from "../../hooks/useEditAllSelected";
import { proofMarginPx } from "../../utils/viewerDisplay";
import BrowserViewCell from "./BrowserViewCell";
import "./multiView.css";

interface Props {
  files: ImageFile[];
  /** Makes a selected photograph the primary (opens it in Develop). */
  onOpen(path: string): void;
  onRate(rating: number, paths?: string[]): void;
  /** The live editor of the primary photograph. */
  children: ReactNode;
  /** The editor's pan/zoom: with the zoom lock every cell follows it. */
  transformWrapperRef?: RefObject<TransformWrapperHandle | null>;
}

/** Removes `path` from the selection; a removed primary hands over to the
 * next selected photograph first. */
export function removeFromSelection(
  path: string,
  selection: string[],
  onOpen: (path: string) => void,
) {
  const primary = useEditorStore.getState().selectedImage?.path ?? null;
  const rest = selection.filter((p) => p !== path);
  if (rest.length === 0) return;
  if (primary === path) {
    const next = stepPrimary(selection, path, 1);
    if (next) onOpen(next);
  }
  useLibraryStore.getState().setLibrary({ multiSelectedPaths: rest });
}

/** The primary photograph's shown size (cropped and turned) in
 * full-resolution pixels; 0 × 0 while unknown. */
function useShownSize(): { width: number; height: number } {
  return useEditorStore(
    useShallow((s) => {
      const crop = s.adjustments.crop;
      if (crop && crop.width > 0 && crop.height > 0) return { width: crop.width, height: crop.height };
      const w = s.selectedImage?.width ?? 0;
      const h = s.selectedImage?.height ?? 0;
      return (s.adjustments.orientationSteps ?? 0) % 2 === 1 ? { width: h, height: w } : { width: w, height: h };
    }),
  );
}

/** The primary photograph's shown aspect (width / height), or null while
 * it is not ready (its size and recipe are not in yet). */
function usePrimaryAspect(): number | null {
  return useEditorStore((s) => {
    if (!s.selectedImage?.isReady) return null;
    const crop = s.adjustments.crop;
    if (crop && crop.width > 0 && crop.height > 0) return crop.width / crop.height;
    const w = s.selectedImage.width;
    const h = s.selectedImage.height;
    if (!w || !h) return null;
    return (s.adjustments.orientationSteps ?? 0) % 2 === 1 ? h / w : w / h;
  });
}

interface LayoutAspect {
  aspect: number;
  selection: string;
  primary: string | null;
  /** The primary's shape when it was last seen ready. */
  seen: number;
}

/** A reshaped photograph, not a size rounded differently by its decode. */
const reshaped = (a: number, b: number) => Math.abs(a - b) > 0.01 * Math.max(a, b);

/**
 * The aspect the cells are packed for: the primary's shape when the multi
 * view starts or the selection changes, and when the primary itself is
 * reshaped (crop, rotation). Stepping to another primary keeps the layout:
 * the cells must not jump while the next photograph loads (its size is not
 * known yet, the previous recipe is still in the store) nor when it arrives.
 */
export function useLayoutAspect(selection: string, primary: string | null): number {
  const shown = usePrimaryAspect();
  const [layout, setLayout] = useState<LayoutAspect | null>(null);
  let next = layout;
  if (shown !== null) {
    if (!layout || layout.selection !== selection) {
      next = { aspect: shown, selection, primary, seen: shown };
    } else if (layout.primary !== primary) {
      // A new primary: keep the layout, follow its own reshaping from now.
      next = { ...layout, primary, seen: shown };
    } else if (reshaped(layout.seen, shown)) {
      next = { ...layout, aspect: shown, seen: shown };
    }
  }
  if (next !== layout) setLayout(next);
  return next?.aspect ?? 1.5;
}

/** The press area over another photograph's cell: a click makes it the
 * primary (not the reference), the pointer zooms and pans it (zoom lock off). */
function CellSelect({
  path,
  height,
  label,
  handlers,
  onOpen,
}: {
  path: string;
  height: number;
  /** None for the reference: only the pointer zooms it. */
  label?: string;
  handlers: ReturnType<ReturnType<typeof useMultiViewZoom>["cellHandlers"]>;
  onOpen?(path: string): void;
}) {
  const { panned, ...pointer } = handlers;
  return (
    <button
      className="studio-mv-select"
      style={{ height }}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      tabIndex={label ? undefined : -1}
      {...pointer}
      onClick={() => {
        if (!panned() && onOpen) onOpen(path);
      }}
    />
  );
}

/**
 * Develop's viewer: the open (primary) photograph alone, or with more
 * photographs selected (and the multi view on, N) all of them side by side.
 * The primary keeps the live editor in its cell; every other cell is drawn
 * by the native display from its own developed frame. Captions, frames and
 * the hover controls are HTML over the native cells.
 */
export default function MultiViewStage({ files, onOpen, onRate, children, transformWrapperRef }: Props) {
  const { t, i18n } = useTranslation();
  const stageRef = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState({ left: 0, top: 0, width: 0, height: 0 });
  const develop = useUIStore((s) => s.activeView === "editor");
  const primary = useEditorStore((s) => s.selectedImage?.path ?? null);
  const multiSelectedPaths = useLibraryStore((s) => s.multiSelectedPaths);
  const { imageRatings, imageFlags } = useLibraryStore(
    useShallow((s) => ({ imageRatings: s.imageRatings, imageFlags: s.imageFlags })),
  );
  const { enabled, editAll, unavailable, reference, setActive, setUnavailable } = useMultiViewStore(
    useShallow((s) => ({
      enabled: s.enabled,
      editAll: s.editAll,
      unavailable: s.unavailable,
      reference: s.reference,
      setActive: s.setActive,
      setUnavailable: s.setUnavailable,
    })),
  );
  const { osPlatform, useWgpu } = useSettingsStore(
    useShallow((s) => ({
      osPlatform: s.osPlatform,
      useWgpu: s.appSettings?.useWgpuRenderer !== false,
    })),
  );
  const mark = useSessionBar((s) => s.mark);
  const shownSize = useShownSize();
  const selection = useMemo(
    () => orderedSelection(files.map((file) => file.path), multiSelectedPaths),
    [files, multiSelectedPaths],
  );
  // The reference stays pinned while it is listed and not the primary.
  const pinned =
    reference && reference !== primary && files.some((file) => file.path === reference) ? reference : null;
  const count = selection.filter((path) => path !== pinned).length + (pinned ? 1 : 0);
  const aspect = useLayoutAspect([pinned ?? "", ...selection].join("\n"), primary);
  const active =
    develop &&
    enabled &&
    (osPlatform === "linux" || (useWgpu && !unavailable)) &&
    primary !== null &&
    count > 1 &&
    selection.includes(primary);
  useEffect(() => setActive(active), [active, setActive]);
  // The before/after split is single-view only: the multi view coming on
  // (N, or a second photograph selected) turns it off.
  useEffect(() => {
    if (active && useEditorStore.getState().splitView) useEditorStore.getState().setEditor({ splitView: false });
  }, [active]);
  // Edit all selected leaves the reference as it is (it is not selected).
  const editable = useMemo(() => selection.filter((path) => path !== pinned), [selection, pinned]);
  useEditAllSelected(active && editAll, editable);

  const { shown, more } = useMemo(() => cellsFor(selection, primary, pinned), [selection, primary, pinned]);
  // The proof margin (X) goes around the whole grid, not around each cell.
  const { proofMarginOn, proofMarginPercent } = useSettingsStore(
    useShallow((s) => ({
      proofMarginOn: s.appSettings?.viewerProofMargin ?? false,
      proofMarginPercent: s.appSettings?.viewerProofMarginPercent ?? undefined,
    })),
  );
  const margin = proofMarginOn ? proofMarginPx(box.width, box.height, proofMarginPercent) : 0;
  const grid = useMemo(
    () => packGrid(shown.length + (more > 0 ? 1 : 0), box.width, box.height, aspect, margin),
    [shown.length, more, box.width, box.height, aspect, margin],
  );
  const primaryIndex = primary ? shown.indexOf(primary) : -1;
  const primaryCell = active ? grid.cells[primaryIndex] : undefined;

  useLayoutEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const measure = () => {
      const r = stage.getBoundingClientRect();
      setBox((b) =>
        b.left === r.left && b.top === r.top && b.width === r.width && b.height === r.height
          ? b
          : { left: r.left, top: r.top, width: r.width, height: r.height },
      );
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(stage);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, []);

  // Any saved change of a shown photograph's recipe makes a new thumbnail;
  // the native side then develops the cells whose recipe changed.
  const thumbnails = useProcessStore((s) => shown.map((p) => s.thumbnails[p] ?? "").join("|"));
  // Edit all selected: as soon as the other recipes are written.
  const recipeRevision = useMultiViewStore((s) => s.recipeRevision);
  const revision = `${recipeRevision}|${thumbnails}`;
  const wasActive = useRef(false);
  useEffect(() => {
    if (osPlatform === "linux") return;
    if (!active) {
      if (wasActive.current) {
        wasActive.current = false;
        void invoke(Invokes.SetMultiView, { primary: null, cells: [] }).catch(() => {});
      }
      return;
    }
    if (box.width <= 0 || box.height <= 0 || grid.cells.length === 0) return;
    wasActive.current = true;
    const dpr = window.devicePixelRatio || 1;
    const cells = shown.map((path, i) => {
      const { photo } = grid.cells[i];
      return {
        path,
        area: [
          (box.left + photo.x) * dpr,
          (box.top + photo.y) * dpr,
          photo.width * dpr,
          photo.height * dpr,
        ],
      };
    });
    invoke<boolean>(Invokes.SetMultiView, { primary, cells })
      .then((shownNatively) => {
        if (shownNatively === false) setUnavailable(true);
      })
      .catch((err) => console.warn("Multi view:", err));
  }, [active, box, grid, shown, primary, revision, setUnavailable, osPlatform]);
  useEffect(
    () => () => {
      if (wasActive.current) void invoke(Invokes.SetMultiView, { primary: null, cells: [] }).catch(() => {});
    },
    [],
  );

  // The editor places its frame when its container resizes; a primary
  // that only moves (another cell removed, a new primary of the same size)
  // must be placed again too.
  const primaryX = primaryCell?.photo.x;
  const primaryY = primaryCell?.photo.y;
  useLayoutEffect(() => {
    window.dispatchEvent(new Event("resize"));
  }, [primaryX, primaryY]);

  // Zoom: locked, every cell follows the primary (Lightroom's lock); off,
  // each cell zooms and pans on its own (useMultiViewZoom).
  const areas = useMemo(
    () =>
      new Map(
        shown.map((path, i) => {
          const photo = grid.cells[i]?.photo ?? { x: 0, y: 0, width: 0, height: 0 };
          return [path, { x: box.left + photo.x, y: box.top + photo.y, width: photo.width, height: photo.height }];
        }),
      ),
    [shown, grid, box.left, box.top],
  );
  const { zoomOf, cellHandlers, onWheel } = useMultiViewZoom({
    active,
    primary,
    shown,
    areas,
    shownSize,
    transformWrapperRef,
  });
  const onWheelRef = useRef(onWheel);
  onWheelRef.current = onWheel;
  useEffect(() => {
    const stage = stageRef.current;
    if (!stage || !active) return;
    const wheel = (e: WheelEvent) => {
      const cell = (e.target as Element | null)?.closest?.<HTMLElement>(".studio-mv-cell[data-path]");
      const path = cell?.dataset.path;
      if (path && onWheelRef.current(path, e)) e.preventDefault();
    };
    stage.addEventListener("wheel", wheel, { passive: false });
    return () => stage.removeEventListener("wheel", wheel);
  }, [active]);
  const percent = useMemo(
    () => new Intl.NumberFormat(i18n.language, { style: "percent", maximumFractionDigits: 0 }),
    [i18n.language],
  );

  const exif = useMemo(() => new Map(files.map((file) => [file.path, file.exif])), [files]);
  const selectedExif = useEditorStore((s) => s.selectedImage?.exif);

  const caption = (path: string, g: GridCell) => {
    const values = captureValues(path === primary && selectedExif ? selectedExif : exif.get(path));
    const line = captureLine(g.level, values);
    const rating = imageRatings[path] || 0;
    return (
      <div className={`studio-mv-caption is-${g.level}`}>
        <div className="studio-mv-caption-row">
          {g.level === "narrow" && line && <span className="studio-mv-capture">{line}</span>}
          <span className="studio-mv-name" title={path}>
            {fileName(path)}
          </span>
          {g.level !== "narrow" && rating > 0 && (
            <span className="studio-mv-stars" aria-label={t("studio.multiView.rating", { count: rating })}>
              {"★".repeat(rating)}
            </span>
          )}
        </div>
        {g.level !== "narrow" && line && <div className="studio-mv-capture">{line}</div>}
      </div>
    );
  };

  const controls = (path: string, isReference: boolean) => {
    const rating = imageRatings[path] || 0;
    const flag = imageFlags[path] ?? 0;
    const name = fileName(path);
    return (
      <div className="studio-mv-controls" role="group" aria-label={t("studio.multiView.controls", { name })}>
        {mark && (
          <>
            <button
              aria-label={t("studio.multiView.pick", { name })}
              aria-pressed={flag === 1}
              title={t("studio.multiView.pick", { name })}
              onClick={() => void mark(flag === 1 ? 0 : 1, [path])}
            >
              <Flag size={12} fill={flag === 1 ? "currentColor" : "none"} />
            </button>
            <button
              aria-label={t("studio.multiView.reject", { name })}
              aria-pressed={flag === -1}
              title={t("studio.multiView.reject", { name })}
              onClick={() => void mark(flag === -1 ? 0 : -1, [path])}
            >
              <Flag size={12} className="is-reject" />
            </button>
          </>
        )}
        <span className="studio-mv-dots">
          {[1, 2, 3, 4, 5].map((n) => (
            <button
              key={n}
              aria-label={t("studio.multiView.rate", { count: n, name })}
              aria-pressed={rating >= n}
              onClick={() => onRate(n, [path])}
            >
              <Star size={10} fill={rating >= n ? "currentColor" : "none"} />
            </button>
          ))}
        </span>
        {isReference ? (
          <button
            className="studio-mv-remove"
            aria-label={t("studio.multiView.unpin", { name })}
            title={t("studio.multiView.unpin", { name })}
            onClick={() => useMultiViewStore.getState().setReference(null)}
          >
            <PinOff size={12} />
          </button>
        ) : (
          <button
            className="studio-mv-remove"
            aria-label={t("studio.multiView.remove", { name })}
            title={t("studio.multiView.remove", { name })}
            onClick={() => removeFromSelection(path, selection, onOpen)}
          >
            <X size={13} />
          </button>
        )}
      </div>
    );
  };

  const style = (r: { x: number; y: number; width: number; height: number }) => ({
    left: r.x,
    top: r.y,
    width: r.width,
    height: r.height,
  });

  return (
    <div ref={stageRef} className={`studio-multiview${active ? " is-active" : ""}`}>
      <div
        className="studio-mv-primary"
        style={primaryCell ? style(primaryCell.photo) : undefined}
      >
        {children}
      </div>
      {active &&
        grid.cells.map((g, i) => {
          if (i >= shown.length) {
            return (
              <div key="more" className="studio-mv-cell is-more" style={style(g.cell)}>
                <span>{t("studio.multiView.more", { count: more, n: more.toLocaleString() })}</span>
              </div>
            );
          }
          const path = shown[i];
          const isPrimary = path === primary;
          const isReference = path === pinned;
          const zoom = zoomOf(path);
          return (
            <div
              key={path}
              className={`studio-mv-cell${isPrimary ? " is-primary" : ""}${isReference ? " is-reference" : editAll ? " is-edit-all" : ""}`}
              style={style(g.cell)}
              data-path={path}
            >
              {isReference && (
                <span
                  className="studio-mv-pin"
                  role="img"
                  aria-label={t("studio.multiView.reference", { name: fileName(path) })}
                  title={t("studio.multiView.referenceTitle")}
                >
                  <Pin size={12} />
                </span>
              )}
              {!isPrimary && (
                <>
                {osPlatform === "linux" && <BrowserViewCell path={path} width={g.photo.width} height={g.photo.height} revision={revision} zoom={zoom} />}
                <CellSelect
                  path={path}
                  height={g.photo.height}
                  label={isReference ? undefined : t("studio.multiView.makePrimary", { name: fileName(path) })}
                  handlers={cellHandlers(path)}
                  onOpen={isReference ? undefined : onOpen}
                />
                </>
              )}
              {zoom && (
                <span
                  className="studio-mv-zoom"
                  style={{ top: Math.max(4, g.photo.height - 26) }}
                  title={t("studio.multiView.zoomBadge", { percent: percent.format(zoom.scale) })}
                >
                  {percent.format(zoom.scale)}
                </span>
              )}
              {controls(path, isReference)}
              {caption(path, g)}
            </div>
          );
        })}
    </div>
  );
}
