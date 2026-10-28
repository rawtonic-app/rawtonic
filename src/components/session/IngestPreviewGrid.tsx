import { Invokes } from '../ui/AppProperties';
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check } from "lucide-react";
import { useTranslation } from "react-i18next";
import { formatBytes } from "../../utils/previews";
import type { Medium } from "./ingestMode";
import IngestLoupe, { type LargePreview } from "./IngestLoupe";

/** Mirrors `session_bridge::IngestCandidate`. */
export interface IngestCandidate {
  path: string;
  size: number;
  /** The library already holds this photograph (same size and content). */
  duplicate: boolean;
  /** On a memory card or a drive (decides copy or add for a catalog). */
  medium?: Medium;
}

/** A source photograph's small preview (its embedded camera JPEG) as an
 * object URL, or null when there is none. */
export async function loadIngestThumbnail(path: string): Promise<string | null> {
  const bytes = await invoke<ArrayBuffer | number[]>(Invokes.SessionIngestThumbnail, { path });
  const data = bytes instanceof ArrayBuffer ? bytes : new Uint8Array(bytes ?? []).buffer;
  if (!data.byteLength) return null;
  return URL.createObjectURL(new Blob([data], { type: "image/jpeg" }));
}

/** Previews are made a few at a time (each reads one file), visible tiles
 * first: a tile scrolled away before its turn is dropped from the queue. */
export function createThumbnailQueue(load: (path: string) => Promise<string | null>, concurrency = 4) {
  const waiting: { path: string; resolve: (url: string | null) => void }[] = [];
  let active = 0;
  const next = () => {
    while (active < concurrency && waiting.length) {
      const job = waiting.shift()!;
      active++;
      load(job.path)
        .catch(() => null)
        .then((url) => {
          active--;
          job.resolve(url);
          next();
        });
    }
  };
  return {
    request(path: string): Promise<string | null> {
      return new Promise((resolve) => {
        waiting.push({ path, resolve });
        next();
      });
    },
    /** Forgets a request that has not started. */
    release(path: string) {
      const index = waiting.findIndex((job) => job.path === path);
      if (index >= 0) waiting.splice(index, 1);
    },
  };
}

/** Tile sizes of the thumbnail size slider (small, medium, large). */
export const TILE_SIZES = [88, 112, 168] as const;
const TILE_SIZE_KEY = "rawtonic.ingestTileSize";
const NAME = 22;
const GAP = 8;
/** Rows drawn above and below the visible ones. */
const OVERSCAN = 2;

export const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

interface Props {
  files: IngestCandidate[];
  selected: Set<string>;
  onSelectedChange: (next: Set<string>) => void;
  /** For tests; defaults to `loadIngestThumbnail`. */
  loadThumbnail?: (path: string) => Promise<string | null>;
  /** For tests; defaults to `loadIngestPreview`. */
  loadPreview?: (path: string) => Promise<LargePreview>;
}

const storedTileSize = () => {
  try {
    const stored = Number(window.localStorage?.getItem(TILE_SIZE_KEY) ?? 1);
    return Number.isInteger(stored) && stored >= 0 && stored < TILE_SIZES.length ? stored : 1;
  } catch {
    return 1;
  }
};

/** The import dialog's source photographs as a grid of previews with
 * checkboxes (Lightroom's and Capture One's import grid): select all or
 * none, Shift-click for a range; only checked photographs are imported.
 * Arrow keys move between photographs; a double-click or Enter opens the
 * focused one large (`IngestLoupe`). Only the visible rows are drawn, so
 * hundreds of files stay light. */
export default function IngestPreviewGrid({
  files,
  selected,
  onSelectedChange,
  loadThumbnail,
  loadPreview,
}: Props) {
  const { t } = useTranslation();
  const scroller = useRef<HTMLDivElement>(null);
  const [view, setView] = useState({ top: 0, width: 0, height: 0 });
  const [thumbs, setThumbs] = useState<Record<string, string | null>>({});
  const urls = useRef(new Set<string>());
  const anchor = useRef<number | null>(null);
  const [tileSize, setTileSize] = useState(storedTileSize);
  const [loupe, setLoupe] = useState<number | null>(null);
  /** A tile to focus once it is drawn (after scrolling to it). */
  const pendingFocus = useRef<number | null>(null);
  const [, redraw] = useState(0);
  const queue = useMemo(
    () => createThumbnailQueue(loadThumbnail ?? loadIngestThumbnail),
    [loadThumbnail],
  );

  useLayoutEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const measure = () =>
      setView({ top: element.scrollTop, width: element.clientWidth, height: element.clientHeight });
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    const owned = urls.current;
    return () => {
      for (const url of owned) URL.revokeObjectURL(url);
      owned.clear();
    };
  }, []);

  // Without layout (tests, a hidden dialog) a typical dialog's size.
  const width = view.width ? Math.max(0, view.width - 16) : 360;
  const height = view.height || 300;
  const TILE = TILE_SIZES[tileSize];
  const ROW = TILE + NAME + GAP;
  const columns = Math.max(1, Math.floor((width + GAP) / (TILE + GAP)));
  const rows = Math.ceil(files.length / columns);
  const first = Math.max(0, Math.floor(view.top / ROW) - OVERSCAN);
  const last = Math.min(rows, Math.ceil((view.top + height) / ROW) + OVERSCAN);
  const shown = files.slice(first * columns, last * columns);

  const wanted = useCallback(
    (path: string) => {
      // A preview that arrives after its tile scrolled away is kept for when
      // it comes back; one whose turn had not come is dropped.
      void queue.request(path).then((url) => {
        if (url) urls.current.add(url);
        setThumbs((current) => (path in current ? current : { ...current, [path]: url }));
      });
      return () => queue.release(path);
    },
    [queue],
  );

  useLayoutEffect(() => {
    const index = pendingFocus.current;
    if (index === null || loupe !== null) return;
    const tile = scroller.current?.querySelector<HTMLElement>(`[data-index="${index}"]`);
    if (tile) {
      pendingFocus.current = null;
      tile.focus({ preventScroll: true });
    }
  });

  /** Scrolls a photograph's row into view and focuses its tile. */
  const focusTile = (index: number) => {
    const element = scroller.current;
    if (element) {
      const top = Math.floor(index / columns) * ROW;
      let scrollTop = element.scrollTop;
      if (top < scrollTop) scrollTop = top;
      else if (top + ROW > scrollTop + height) scrollTop = top + ROW - height;
      if (scrollTop !== element.scrollTop) {
        element.scrollTop = scrollTop;
        setView((current) => ({ ...current, top: scrollTop }));
      }
    }
    pendingFocus.current = index;
    // Draws again so the effect above can focus a tile that is already drawn.
    redraw((n) => n + 1);
  };

  const closeLoupe = () => {
    const index = loupe;
    setLoupe(null);
    if (index !== null) focusTile(index);
  };

  const toggle = (index: number, range: boolean) => {
    const next = new Set(selected);
    const path = files[index].path;
    const on = !selected.has(path);
    if (range && anchor.current !== null) {
      const [from, to] = [Math.min(anchor.current, index), Math.max(anchor.current, index)];
      for (let i = from; i <= to; i++) {
        if (on) next.add(files[i].path);
        else next.delete(files[i].path);
      }
    } else if (on) next.add(path);
    else next.delete(path);
    anchor.current = index;
    onSelectedChange(next);
  };

  const chosen = files.filter((file) => selected.has(file.path));
  const bytes = chosen.reduce((sum, file) => sum + file.size, 0);
  const duplicates = files.filter((file) => file.duplicate).length;

  return (
    <div className="session-ingest-preview">
      <div className="session-ingest-preview-bar">
        <span role="status">
          {t("session.ingest.selectedOf", {
            count: files.length,
            selected: chosen.length.toLocaleString(),
            total: files.length.toLocaleString(),
            size: formatBytes(bytes),
          })}
          {duplicates > 0 && ` · ${t("session.ingest.duplicates", { count: duplicates })}`}
        </span>
        <button
          type="button"
          className="session-link-button"
          onClick={() => onSelectedChange(new Set(files.map((file) => file.path)))}
        >
          {t("session.ingest.selectAll")}
        </button>
        <button type="button" className="session-link-button" onClick={() => onSelectedChange(new Set())}>
          {t("session.ingest.selectNone")}
        </button>
        <input
          type="range"
          className="session-ingest-size"
          min={0}
          max={TILE_SIZES.length - 1}
          step={1}
          value={tileSize}
          aria-label={t("session.ingest.thumbnailSize")}
          title={t("session.ingest.thumbnailSize")}
          onChange={(e) => {
            const size = Number(e.target.value);
            setTileSize(size);
            try {
              window.localStorage?.setItem(TILE_SIZE_KEY, String(size));
            } catch {
              // Not kept; the size still applies now.
            }
          }}
        />
      </div>
      <div
        ref={scroller}
        className="session-ingest-grid"
        role="group"
        aria-label={t("session.ingest.gridLabel")}
        style={{ ["--ingest-tile" as string]: `${TILE}px` }}
        onKeyDown={(e) => {
          const tile = (e.target as HTMLElement).closest<HTMLElement>("[data-index]");
          if (!tile || e.metaKey || e.ctrlKey || e.altKey) return;
          const index = Number(tile.dataset.index);
          if (e.key === "Enter") {
            e.preventDefault();
            setLoupe(index);
            return;
          }
          const by = (
            { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -columns, ArrowDown: columns } as Record<string, number>
          )[e.key];
          if (by === undefined) return;
          e.preventDefault();
          const next = index + by;
          if (next >= 0 && next < files.length) focusTile(next);
        }}
        onScroll={(e) => {
          const top = e.currentTarget.scrollTop;
          setView((current) => (current.top === top ? current : { ...current, top }));
        }}
      >
        <div style={{ height: rows * ROW, position: "relative" }}>
          <div
            className="session-ingest-tiles"
            style={{
              transform: `translateY(${first * ROW}px)`,
              gridTemplateColumns: `repeat(${columns}, ${TILE}px)`,
            }}
          >
            {shown.map((file, i) => (
              <IngestTile
                key={file.path}
                file={file}
                index={first * columns + i}
                checked={selected.has(file.path)}
                thumb={thumbs[file.path]}
                loaded={file.path in thumbs}
                want={wanted}
                onToggle={toggle}
                onOpen={setLoupe}
              />
            ))}
          </div>
        </div>
      </div>
      {loupe !== null && files[loupe] && (
        <IngestLoupe
          files={files}
          index={loupe}
          selected={selected}
          thumbnail={thumbs[files[loupe].path]}
          load={loadPreview}
          onIndexChange={setLoupe}
          onToggle={(path) => {
            const next = new Set(selected);
            if (next.has(path)) next.delete(path);
            else next.add(path);
            onSelectedChange(next);
          }}
          onClose={closeLoupe}
        />
      )}
    </div>
  );
}

function IngestTile({
  file,
  index,
  checked,
  thumb,
  loaded,
  want,
  onToggle,
  onOpen,
}: {
  file: IngestCandidate;
  index: number;
  checked: boolean;
  thumb: string | null | undefined;
  loaded: boolean;
  want: (path: string) => () => void;
  onToggle: (index: number, range: boolean) => void;
  onOpen: (index: number) => void;
}) {
  const { t } = useTranslation();
  const name = fileName(file.path);
  useEffect(() => (loaded ? undefined : want(file.path)), [file.path, loaded, want]);
  const extension = name.includes(".") ? name.split(".").pop()!.toUpperCase() : "";
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      aria-label={name}
      data-index={index}
      title={`${file.path}\n${formatBytes(file.size)}${file.duplicate ? `\n${t("session.ingest.inLibraryTitle")}` : ""}\n${t("session.ingest.viewLargeHint")}`}
      className={`session-ingest-tile${checked ? " is-checked" : ""}${file.duplicate ? " is-duplicate" : ""}`}
      onClick={(e) => onToggle(index, e.shiftKey)}
      onDoubleClick={() => onOpen(index)}
    >
      <span className="session-ingest-thumb">
        {thumb ? <img src={thumb} alt="" draggable={false} /> : <span>{loaded ? extension : ""}</span>}
        <span className="session-ingest-check" aria-hidden>
          {checked && <Check size={12} strokeWidth={3} />}
        </span>
        {file.duplicate && <span className="session-ingest-badge">{t("session.ingest.inLibrary")}</span>}
      </span>
      <span className="session-ingest-name">{name}</span>
    </button>
  );
}
