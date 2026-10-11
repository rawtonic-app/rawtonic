import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check } from "lucide-react";
import { useTranslation } from "react-i18next";
import { formatBytes } from "../../utils/previews";

/** Mirrors `session_bridge::IngestCandidate`. */
export interface IngestCandidate {
  path: string;
  size: number;
  /** The library already holds this photograph (same size and content). */
  duplicate: boolean;
}

/** A source photograph's small preview (its embedded camera JPEG) as an
 * object URL, or null when there is none. */
export async function loadIngestThumbnail(path: string): Promise<string | null> {
  const bytes = await invoke<ArrayBuffer | number[]>("session_ingest_thumbnail", { path });
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

const TILE = 112;
const NAME = 22;
const GAP = 8;
const ROW = TILE + NAME + GAP;
/** Rows drawn above and below the visible ones. */
const OVERSCAN = 2;

export const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

interface Props {
  files: IngestCandidate[];
  selected: Set<string>;
  onSelectedChange: (next: Set<string>) => void;
  /** For tests; defaults to `loadIngestThumbnail`. */
  loadThumbnail?: (path: string) => Promise<string | null>;
}

/** The import dialog's source photographs as a grid of previews with
 * checkboxes (Lightroom's and Capture One's import grid): select all or
 * none, Shift-click for a range; only checked photographs are imported.
 * Only the visible rows are drawn, so hundreds of files stay light. */
export default function IngestPreviewGrid({ files, selected, onSelectedChange, loadThumbnail }: Props) {
  const { t } = useTranslation();
  const scroller = useRef<HTMLDivElement>(null);
  const [view, setView] = useState({ top: 0, width: 0, height: 0 });
  const [thumbs, setThumbs] = useState<Record<string, string | null>>({});
  const urls = useRef(new Set<string>());
  const anchor = useRef<number | null>(null);
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
  const width = view.width || 360;
  const height = view.height || 300;
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
      </div>
      <div
        ref={scroller}
        className="session-ingest-grid"
        role="group"
        aria-label={t("session.ingest.gridLabel")}
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
              />
            ))}
          </div>
        </div>
      </div>
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
}: {
  file: IngestCandidate;
  index: number;
  checked: boolean;
  thumb: string | null | undefined;
  loaded: boolean;
  want: (path: string) => () => void;
  onToggle: (index: number, range: boolean) => void;
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
      title={`${file.path}\n${formatBytes(file.size)}${file.duplicate ? `\n${t("session.ingest.inLibraryTitle")}` : ""}`}
      className={`session-ingest-tile${checked ? " is-checked" : ""}${file.duplicate ? " is-duplicate" : ""}`}
      onClick={(e) => onToggle(index, e.shiftKey)}
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
