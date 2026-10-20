import { Invokes } from '../ui/AppProperties';
import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check, ChevronLeft, ChevronRight, LoaderCircle, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { formatBytes } from "../../utils/previews";
import type { IngestCandidate } from "./IngestPreviewGrid";

/** Mirrors `file_management::SourceCaptureInfo`. */
export interface CaptureInfo {
  iso?: string | null;
  exposure?: string | null;
  aperture?: string | null;
  focalLength?: string | null;
}

export interface LargePreview {
  /** Object URL of the photograph's largest embedded camera JPEG (or the
   * file itself for a JPEG); null when there is none. */
  url: string | null;
  info: CaptureInfo | null;
}

/** The long side the backend scales a preview to when it has to make one
 * (an embedded camera JPEG is passed through at its own size). */
const previewSize = () =>
  Math.min(
    8192,
    Math.max(
      1280,
      Math.round(
        Math.max(window.screen?.width ?? 0, window.screen?.height ?? 0, window.innerWidth) *
          (window.devicePixelRatio || 1),
      ),
    ),
  );

/** A source photograph's large preview and capture info, both read from the
 * file off the UI thread by the backend. */
export async function loadIngestPreview(path: string): Promise<LargePreview> {
  const [bytes, info] = await Promise.all([
    invoke<ArrayBuffer | number[]>(Invokes.SessionIngestPreview, { path, maxSize: previewSize() }),
    invoke<CaptureInfo>(Invokes.SessionIngestCaptureInfo, { path }).catch(() => null),
  ]);
  const data = bytes instanceof ArrayBuffer ? bytes : new Uint8Array(bytes ?? []).buffer;
  return {
    url: data.byteLength ? URL.createObjectURL(new Blob([data], { type: "image/jpeg" })) : null,
    info,
  };
}

/** Large previews of the shown photograph and its neighbours, a couple at a
 * time, the shown one first. Previews that are no longer wanted are dropped
 * from the queue before they start, and discarded (URL revoked) when they
 * arrive after the view moved on. */
export function createLargePreviewCache(
  load: (path: string) => Promise<LargePreview>,
  onLoaded: (path: string) => void,
  concurrency = 2,
) {
  const done = new Map<string, LargePreview>();
  const running = new Set<string>();
  let waiting: string[] = [];
  let wanted = new Set<string>();
  const revoke = (preview: LargePreview) => {
    if (preview.url) URL.revokeObjectURL(preview.url);
  };
  const pump = () => {
    while (running.size < concurrency && waiting.length) {
      const path = waiting.shift()!;
      running.add(path);
      load(path)
        .catch((): LargePreview => ({ url: null, info: null }))
        .then((preview) => {
          running.delete(path);
          if (wanted.has(path)) {
            done.set(path, preview);
            onLoaded(path);
          } else revoke(preview);
          pump();
        });
    }
  };
  const want = (paths: string[]) => {
    wanted = new Set(paths);
    for (const [path, preview] of done) {
      if (!wanted.has(path)) {
        revoke(preview);
        done.delete(path);
      }
    }
    waiting = paths.filter((path) => !done.has(path) && !running.has(path));
    pump();
  };
  return {
    /** The paths to keep or load, most important first. */
    want,
    get: (path: string) => done.get(path),
    /** Drops everything; previews still loading are discarded on arrival. */
    clear: () => want([]),
  };
}

/** Neighbours kept ready for stepping, in the order they are loaded. */
const NEIGHBOURS = [0, 1, -1, 2];

interface Props {
  files: IngestCandidate[];
  index: number;
  selected: Set<string>;
  /** The grid's small preview, shown until the large one arrives. */
  thumbnail?: string | null;
  onIndexChange: (index: number) => void;
  onToggle: (path: string) => void;
  onClose: () => void;
  load?: (path: string) => Promise<LargePreview>;
}

/** The import dialog's large view of one source photograph (the importer's
 * viewer in Capture One and Lightroom): its embedded camera preview scaled to
 * the view, filename and capture info, and its import checkbox. Arrow keys
 * step, Space, Escape or a double-click go back to the grid. */
export default function IngestLoupe({
  files,
  index,
  selected,
  thumbnail,
  onIndexChange,
  onToggle,
  onClose,
  load,
}: Props) {
  const { t } = useTranslation();
  const root = useRef<HTMLDivElement>(null);
  const [, setLoaded] = useState(0);
  const cache = useMemo(
    () => createLargePreviewCache(load ?? loadIngestPreview, () => setLoaded((n) => n + 1)),
    [load],
  );
  const file = files[index];

  useEffect(() => {
    const paths = NEIGHBOURS.map((offset) => files[index + offset]?.path).filter(
      (path): path is string => !!path,
    );
    cache.want(paths);
  }, [cache, files, index]);
  useEffect(() => () => cache.clear(), [cache]);

  useEffect(() => {
    root.current?.focus();
    // Escape belongs to the large view while it is open: the dialog must not
    // take it as its own cancel. The key's default is prevented below; this
    // also stops a cancel the webview might still send for it (kept a moment
    // after closing, as that cancel can arrive after the view is gone).
    const dialog = root.current?.closest("dialog");
    if (!dialog) return;
    const swallow = (e: Event) => {
      e.preventDefault();
      e.stopImmediatePropagation();
    };
    dialog.addEventListener("cancel", swallow, true);
    return () => {
      window.setTimeout(() => dialog.removeEventListener("cancel", swallow, true), 250);
    };
  }, []);

  if (!file) return null;
  const preview = cache.get(file.path);
  const checked = selected.has(file.path);
  const name = file.path.split(/[\\/]/).pop() ?? file.path;
  const info = preview?.info;
  const capture = [info?.iso, info?.exposure, info?.aperture, info?.focalLength].filter(Boolean).join(" · ");
  const step = (by: number) => {
    const next = Math.min(files.length - 1, Math.max(0, index + by));
    if (next !== index) onIndexChange(next);
  };
  const image = preview?.url ?? thumbnail;

  return (
    <div
      ref={root}
      className="session-ingest-loupe"
      role="region"
      aria-label={t("session.ingest.largeView")}
      tabIndex={-1}
      onKeyDown={(e) => {
        if (e.metaKey || e.ctrlKey || e.altKey) return;
        switch (e.key) {
          case "ArrowRight":
          case "ArrowDown":
            step(1);
            break;
          case "ArrowLeft":
          case "ArrowUp":
            step(-1);
            break;
          case "Home":
            step(-files.length);
            break;
          case "End":
            step(files.length);
            break;
          case " ":
          case "Escape":
            onClose();
            break;
          default:
            return;
        }
        e.preventDefault();
        e.stopPropagation();
      }}
      onKeyUp={(e) => {
        // Space would otherwise click whichever button has the focus.
        if (e.key === " ") e.preventDefault();
      }}
    >
      <div className="session-ingest-loupe-bar">
        <button
          type="button"
          role="checkbox"
          aria-checked={checked}
          className={`session-ingest-loupe-check${checked ? " is-checked" : ""}`}
          onClick={() => onToggle(file.path)}
        >
          <span className="session-ingest-check" aria-hidden>
            {checked && <Check size={12} strokeWidth={3} />}
          </span>
          {t("session.ingest.include")}
        </button>
        <div className="session-ingest-loupe-title">
          <strong title={file.path}>{name}</strong>
          <span>
            {[capture, formatBytes(file.size)].filter(Boolean).join(" · ")}
            {file.duplicate && ` · ${t("session.ingest.inLibrary")}`}
          </span>
        </div>
        <span className="session-ingest-loupe-count">
          {(index + 1).toLocaleString()} / {files.length.toLocaleString()}
        </span>
        <button
          type="button"
          className="session-icon-button"
          aria-label={t("session.ingest.previous")}
          disabled={index === 0}
          onClick={() => step(-1)}
        >
          <ChevronLeft size={16} />
        </button>
        <button
          type="button"
          className="session-icon-button"
          aria-label={t("session.ingest.next")}
          disabled={index === files.length - 1}
          onClick={() => step(1)}
        >
          <ChevronRight size={16} />
        </button>
        <button
          type="button"
          className="session-icon-button"
          aria-label={t("session.ingest.backToGrid")}
          title={t("session.ingest.backToGrid")}
          onClick={onClose}
        >
          <X size={16} />
        </button>
      </div>
      <div className="session-ingest-loupe-image" onDoubleClick={onClose}>
        {image ? (
          <img
            key={image}
            src={image}
            alt={name}
            draggable={false}
            className={preview?.url ? undefined : "is-placeholder"}
          />
        ) : preview ? (
          <span>{t("session.ingest.noPreview")}</span>
        ) : null}
        {!preview && (
          <LoaderCircle size={18} className="session-ingest-loupe-spinner animate-spin" aria-hidden />
        )}
      </div>
    </div>
  );
}
