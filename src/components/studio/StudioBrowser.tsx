import {
  memo,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useMemo,
  type MouseEvent,
  type PointerEvent,
} from "react";
import { Image as ImageIcon, Star, SlidersHorizontal, Flag, X, Unlink } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useProcessStore } from "../../store/useProcessStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { type ImageFile } from "../ui/AppProperties";
import { imageFlag } from "../../utils/culling";
import { PerfProfiler, performanceDiagnosticsEnabled, reportForcedLayout, reportPerformance } from '../../hooks/usePerformanceDiagnostics';
import { startTileDrag } from "./tileDrag";
import { reportLibraryPresentation } from '../../utils/libraryListing';
import { offlineTitle } from '../../utils/offline';

export function visibleRange(
  count: number,
  offset: number,
  viewport: number,
  extent: number,
  columns = 1,
) {
  const start = Math.max(0, (Math.floor(offset / extent) - 1) * columns);
  return {
    start,
    end: Math.min(
      count,
      (Math.ceil((offset + viewport) / extent) + 1) * columns,
    ),
  };
}
/** Pointer travel (px) up to which a press and release is a click. */
const CLICK_SLOP = 4;

/** Whether a press at (x, y) on `target` inside the browser `el` is on its
 * empty space: not on a photograph and not on a scroll bar. */
export function isEmptySpace(el: HTMLElement, target: Element | null, x: number, y: number) {
  if (!target || !el.contains(target) || target.closest(".studio-photo")) return false;
  const rect = el.getBoundingClientRect();
  // Scroll bars lie outside the client area (classic, always-shown ones).
  return x - rect.left < el.clientLeft + el.clientWidth && y - rect.top < el.clientTop + el.clientHeight;
}

const Tile = memo(function Tile({
  file,
  selected,
  current,
  onSelect,
  onOpen,
  onRate,
  onContextMenu,
  film,
}: {
  file: ImageFile;
  selected: boolean;
  current: boolean;
  onSelect(path: string, event: MouseEvent): void;
  onOpen(path: string): void;
  onRate(rating: number, paths: string[]): void;
  onContextMenu(event: MouseEvent, path: string): void;
  film: boolean;
}) {
  const { t } = useTranslation();
  const url = useProcessStore((s) => s.thumbnails[file.path]);
  const firstLoad = useRef({ started: performanceDiagnosticsEnabled ? performance.now() : 0, cached: Boolean(url), reported: false });
  const rating = useLibraryStore(
    (s) => s.imageRatings[file.path] ?? file.rating,
  );
  const flag = useLibraryStore((s) => imageFlag(s.imageFlags, file.path));
  const color = file.tags?.find((tag) => tag.startsWith("color:"))?.slice(6).toLowerCase();
  // A virtual copy's path carries "?vc=<id>"; show the file name and say it is a copy.
  const [fileName, copyId] = (file.path.split("/").pop() ?? "").split("?vc=");
  const label = copyId ? t("studio.library.browser.virtualCopyName", { name: fileName }) : fileName;
  const colorLabels = {
    red: t("studio.library.colorLabels.red"),
    yellow: t("studio.library.colorLabels.yellow"),
    green: t("studio.library.colorLabels.green"),
    blue: t("studio.library.colorLabels.blue"),
    purple: t("studio.library.colorLabels.purple"),
  } as Record<string, string>;
  return (
    <div
      className={`studio-photo ${selected ? "is-selected" : ""} ${current ? "is-current" : ""} ${flag === -1 ? "is-rejected" : ""} ${file.is_offline ? "is-offline" : ""}`}
      role="option"
      aria-selected={selected}
      aria-label={label}
      onContextMenu={(e) => onContextMenu(e, file.path)}
      onPointerDown={(e) => startTileDrag(e, file.path)}
    >
      <button
        className="studio-photo-open"
        // Tab on a selected photograph hides the panels (Lightroom); the
        // arrow keys move between photographs.
        data-panel-shortcut="true"
        onClick={(e) => onSelect(file.path, e)}
        onDoubleClick={() => onOpen(file.path)}
        title={label}
      >
        <span className="studio-photo-image">
          {url ? (
            <img src={url} alt="" draggable={false} decoding="async" onLoad={() => {
              if (!performanceDiagnosticsEnabled || firstLoad.current.reported) return;
              firstLoad.current.reported = true;
              reportPerformance('library_thumbnail_load', {
                ms: performance.now() - firstLoad.current.started,
                cached: Number(firstLoad.current.cached), film: Number(film),
              });
            }} />
          ) : (
            <ImageIcon size={24} />
          )}
          <span className="studio-photo-type">
            {file.is_raw ? "RAW" : fileName.split(".").pop()?.toUpperCase()}
          </span>
          {file.is_edited && (
            <SlidersHorizontal className="studio-photo-edited" size={12} />
          )}
          {file.is_offline && (
            // The file is missing or its drive is disconnected (Lightroom's
            // "!" badge); right-click › Locate photo relinks it.
            <span className="studio-photo-offline" role="img" aria-label={t("offline.badge")}
              title={offlineTitle(t, file)}>
              <Unlink size={11} />
              {!film && <span>{t("offline.badge")}</span>}
            </span>
          )}
          {flag !== 0 && (
            <span className="studio-photo-flag" role="img" aria-label={flag === 1 ? t("studio.library.browser.picked") : t("studio.library.browser.rejected")}
              title={flag === 1 ? `${t("studio.library.browser.picked")} · P` : `${t("studio.library.browser.rejected")} · X`}>
              {flag === 1 ? <Flag size={12} fill="currentColor" /> : <X size={13} />}
            </span>
          )}
          {film && rating > 0 && (
            // The filmstrip shows thumbnails only (Lightroom); a rating stays
            // visible as a small badge.
            <span className="studio-photo-stars" role="img" aria-label={t("studio.library.browser.stars", { count: rating })}>
              {"★".repeat(rating)}
            </span>
          )}
          {film && color && colorLabels[color] && (
            <span className={`studio-photo-color is-${color}`} role="img" aria-label={colorLabels[color]} />
          )}
        </span>
        {!film && (
          <span className="studio-photo-name">
            {fileName}
            {copyId && <span className="studio-photo-copy">{` · ${t("studio.library.browser.virtualCopy")}`}</span>}
          </span>
        )}
      </button>
      {!film && (
      <div className="studio-photo-rating" aria-label={t("studio.library.browser.rating")}>
        {[1, 2, 3, 4, 5].map((n) => (
          <button
            key={n}
            aria-label={t("studio.library.browser.rate", { name: label, count: n })}
            aria-pressed={rating >= n}
            onClick={() => onRate(rating === n ? 0 : n, [file.path])}
          >
            <Star
              size={film ? 9 : 11}
              fill={rating >= n ? "currentColor" : "none"}
            />
          </button>
        ))}
        {color && colorLabels[color] && (
          <span className={`studio-photo-color is-${color}`} role="img" aria-label={colorLabels[color]} title={colorLabels[color]} />
        )}
      </div>
      )}
    </div>
  );
});
export default function StudioBrowser({
  files,
  film = false,
  currentPath,
  onSelect,
  onOpen,
  onRate,
  onContextMenu,
  requestThumbnails,
  onClearSelection,
  size = 190,
  listing,
}: {
  files: ImageFile[];
  film?: boolean;
  /** Which listing `files` is (folder or album, search, filter, sort): a new
   * one starts at the top; photos added or removed keep the place. */
  listing?: string;
  currentPath?: string;
  onSelect(path: string, event: MouseEvent): void;
  onOpen(path: string): void;
  onRate(rating: number, paths: string[]): void;
  onContextMenu(event: MouseEvent, path: string): void;
  requestThumbnails(paths: string[]): void;
  /** A plain click on empty space (not a photograph, not a scroll bar)
   * clears the selection (Lightroom, Capture One). */
  onClearSelection?(): void;
  size?: number;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState({ width: 800, height: 500 });
  const [offset, setOffset] = useState(0);
  const selected = useLibraryStore((s) => s.multiSelectedPaths);
  const selectedSet = useMemo(() => new Set(selected), [selected]);
  // Where a press on empty space started; a click (press and release
  // there, no drag) clears the selection.
  const emptyPress = useRef<{ x: number; y: number } | null>(null);
  const onBackgroundDown = (e: PointerEvent<HTMLDivElement>) => {
    emptyPress.current = null;
    const el = ref.current;
    if (!onClearSelection || !el || e.button !== 0) return;
    if (e.shiftKey || e.metaKey || e.ctrlKey || e.altKey) return;
    if (isEmptySpace(el, e.target as Element, e.clientX, e.clientY))
      emptyPress.current = { x: e.clientX, y: e.clientY };
  };
  const onBackgroundUp = (e: PointerEvent<HTMLDivElement>) => {
    const press = emptyPress.current;
    emptyPress.current = null;
    const el = ref.current;
    if (!press || !el || !onClearSelection) return;
    if (Math.hypot(e.clientX - press.x, e.clientY - press.y) > CLICK_SLOP) return;
    if (!isEmptySpace(el, e.target as Element, e.clientX, e.clientY)) return;
    onClearSelection();
  };
  useEffect(() => {
    if (!performanceDiagnosticsEnabled || film) return;
    const source = useLibraryStore.getState().imageList;
    const frame = requestAnimationFrame(() => {
      if (source === useLibraryStore.getState().imageList) reportLibraryPresentation(source, files.length);
    });
    return () => cancelAnimationFrame(frame);
  }, [files, film]);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) =>
      setBox({
        width: entry.contentRect.width,
        height: entry.contentRect.height,
      }),
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  const columns = film ? 1 : Math.max(1, Math.floor(box.width / size));
  const extent = film ? 112 : Math.round((box.width / columns) * 0.72) + 57;
  const range = visibleRange(
    files.length,
    offset,
    film ? box.width : box.height,
    extent,
    columns,
  );
  const mountedTiles = range.end - range.start;
  const measuredTiles = useRef(0);
  useLayoutEffect(() => {
    if (!performanceDiagnosticsEnabled || film || Math.abs(mountedTiles - measuredTiles.current) < 20) return;
    measuredTiles.current = mountedTiles;
    reportForcedLayout("grid_layout", { tiles: mountedTiles });
  }, [film, mountedTiles]);
  useEffect(() => {
    requestThumbnails(files.slice(range.start, range.end).map((f) => f.path));
  }, [files, range.start, range.end, requestThumbnails]);
  useEffect(() => {
    if (!film || !currentPath || !ref.current) return;
    const index = files.findIndex((f) => f.path === currentPath);
    if (index < 0) return;
    const el = ref.current;
    const x = index * extent;
    if (x < el.scrollLeft || x + extent > el.scrollLeft + el.clientWidth)
      el.scrollLeft = Math.max(0, x - (el.clientWidth - extent) / 2);
  }, [currentPath, film, files, extent, box.width]);
  useEffect(() => {
    if (!ref.current || film) return;
    ref.current.scrollTop = 0;
    ref.current.scrollLeft = 0;
    setOffset(0);
  }, [film, listing]);
  return (
    <PerfProfiler id={film ? "filmstrip" : "grid"}>
    <div
      ref={ref}
      className={`studio-browser ${film ? "is-filmstrip" : "is-grid"}`}
      role="listbox"
      aria-label={film ? t("studio.library.browser.filmstrip") : t("studio.library.browser.photographs")}
      aria-multiselectable="true"
      onPointerDown={onBackgroundDown}
      onPointerUp={onBackgroundUp}
      onScroll={(e) =>
        setOffset(film ? e.currentTarget.scrollLeft : e.currentTarget.scrollTop)
      }
    >
      {!files.length ? (
        <div className="studio-empty">
          <ImageIcon size={30} />
          <strong>{t("studio.library.browser.emptyTitle")}</strong>
          <span>{t("studio.library.browser.emptyHint")}</span>
        </div>
      ) : (
        <div
          style={{
            position: "relative",
            width: film ? files.length * extent : "100%",
            height: film ? "100%" : Math.ceil(files.length / columns) * extent,
          }}
        >
          {files.slice(range.start, range.end).map((file, i) => {
            const index = range.start + i;
            return (
              <div
                key={file.path}
                style={{
                  position: "absolute",
                  left: film
                    ? index * extent
                    : `${((index % columns) * 100) / columns}%`,
                  top: film ? 0 : Math.floor(index / columns) * extent,
                  width: film ? extent : `${100 / columns}%`,
                  height: film ? "100%" : extent,
                  padding: film ? "6px 2px" : "8px",
                }}
              >
                <Tile
                  file={file}
                  // In the grid the open photograph is outlined only while
                  // selected: an emptied selection shows no photograph marked.
                  current={file.path === currentPath && (film || selectedSet.has(file.path))}
                  selected={selectedSet.has(file.path)}
                  onSelect={onSelect}
                  onOpen={onOpen}
                  onRate={onRate}
                  onContextMenu={onContextMenu}
                  film={film}
                />
              </div>
            );
          })}
        </div>
      )}
    </div>
    </PerfProfiler>
  );
}
