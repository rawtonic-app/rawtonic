import { Invokes } from '../ui/AppProperties';
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Check, ChevronLeft, ChevronRight, LoaderCircle, Star, X } from 'lucide-react';
import { reviewKey, type ReviewKey } from './ingestReview';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useTranslation } from 'react-i18next';
import { formatBytes } from '../../utils/previews';
import type { IngestCandidate } from './IngestPreviewGrid';

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
          (window.devicePixelRatio || 1)
      )
    )
  );

/** A source photograph's large preview and capture info, both read from the
 * file off the UI thread by the backend. */
export async function loadIngestPreview(path: string): Promise<LargePreview> {
  const [bytes, info] = await Promise.all([
    invoke<ArrayBuffer | number[]>(Invokes.SessionIngestPreview, { path, maxSize: previewSize() }),
    invoke<CaptureInfo>(Invokes.SessionIngestCaptureInfo, { path }).catch(() => null)
  ]);
  const data = bytes instanceof ArrayBuffer ? bytes : new Uint8Array(bytes ?? []).buffer;
  return {
    url: data.byteLength ? URL.createObjectURL(new Blob([data], { type: 'image/jpeg' })) : null,
    info
  };
}

/** A source photograph at its full size for the 100 % view: the camera's
 * embedded JPEG when it has the sensor's size, else the RAW decoded at full
 * size by the backend (`session_ingest_full_preview`); url null when it
 * could not be made. Never an upscale. */
export async function loadIngestFullPreview(path: string): Promise<LargePreview> {
  const bytes = await invoke<ArrayBuffer | number[]>(Invokes.SessionIngestFullPreview, { path });
  const data = bytes instanceof ArrayBuffer ? bytes : new Uint8Array(bytes ?? []).buffer;
  return { url: data.byteLength ? URL.createObjectURL(new Blob([data], { type: 'image/jpeg' })) : null, info: null };
}

/** Large previews of the shown photograph and its neighbours, a couple at a
 * time, the shown one first. Previews that are no longer wanted are dropped
 * from the queue before they start, and discarded (URL revoked) when they
 * arrive after the view moved on. */
export function createLargePreviewCache(
  load: (path: string) => Promise<LargePreview>,
  onLoaded: (path: string) => void,
  concurrency = 2
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
    clear: () => want([])
  };
}

/** Neighbours kept ready for stepping, in the order they are loaded. */
const NEIGHBOURS = [0, 1, -1, 2];
/** At 100 %: the shown photograph and the next one (a full RAW decode takes
 * a second or two, so stepping forward through a series finds it ready). */
const FULL_NEIGHBOURS = [0, 1];
/** Pointer travel (px) below which a press is a click, not a pan. */
const CLICK_SLOP = 4;

/** Where the 100 % view is centred, as a share of the image's width and
 * height; kept while stepping, so a series is checked at the same spot. */
export interface ZoomFocus {
  x: number;
  y: number;
}

/** The image's offset in the view at 100 %: `focus` at the view's centre,
 * the image kept covering the view (centred where it is smaller). */
export function zoomOffset(
  focus: ZoomFocus,
  image: { width: number; height: number },
  view: { width: number; height: number }
) {
  const axis = (share: number, size: number, room: number) =>
    size <= room ? (room - size) / 2 : Math.min(0, Math.max(room - size, room / 2 - share * size));
  return { x: axis(focus.x, image.width, view.width), y: axis(focus.y, image.height, view.height) };
}

const clampShare = (v: number) => Math.min(1, Math.max(0, v));

interface Props {
  files: IngestCandidate[];
  index: number;
  selected: Set<string>;
  /** Star ratings given here, by path (applied to the imported copies). */
  ratings?: Record<string, number>;
  /** The grid's small preview, shown until the large one arrives. */
  thumbnail?: string | null;
  onIndexChange: (index: number) => void;
  onToggle: (path: string) => void;
  /** P, X or 0–5 on the shown photograph (`ingestReview.reviewKey`). */
  onReview?: (path: string, key: ReviewKey) => void;
  onClose: () => void;
  load?: (path: string) => Promise<LargePreview>;
  /** For tests; defaults to `loadIngestFullPreview`. */
  loadFull?: (path: string) => Promise<LargePreview>;
}

/** The import dialog's large view of one source photograph (the importer's
 * viewer in Capture One and Lightroom): its embedded camera preview fitted
 * to the view, filename and capture info, its import checkbox and rating.
 * A click or Z shows it at 100 % around the pointer (the real full-size
 * pixels, `loadIngestFullPreview`) and back; at 100 % a drag pans and the
 * arrow keys keep the zoom and spot for the next photograph (a focus check
 * through a series). P includes, X leaves out, 0–5 rate; Space, Escape or
 * the close button go back to the grid. */
export default function IngestLoupe({
  files,
  index,
  selected,
  ratings = {},
  thumbnail,
  onIndexChange,
  onToggle,
  onReview,
  onClose,
  load,
  loadFull
}: Props) {
  const { t } = useTranslation();
  const root = useRef<HTMLDivElement>(null);
  const stage = useRef<HTMLDivElement>(null);
  const [, setLoaded] = useState(0);
  const cache = useMemo(
    () => createLargePreviewCache(load ?? loadIngestPreview, () => setLoaded((n) => n + 1)),
    [load]
  );
  const fullCache = useMemo(
    () => createLargePreviewCache(loadFull ?? loadIngestFullPreview, () => setLoaded((n) => n + 1), 1),
    [loadFull]
  );
  const [zoomed, setZoomed] = useState(false);
  const [focus, setFocus] = useState<ZoomFocus>({ x: 0.5, y: 0.5 });
  const [natural, setNatural] = useState<Record<string, { width: number; height: number }>>({});
  const [viewSize, setViewSize] = useState({ width: 0, height: 0 });
  const press = useRef<{ x: number; y: number; focus: ZoomFocus; moved: boolean } | null>(null);
  // The Library's auto-advance (Caps Lock, Photo › Auto-advance) applies
  // here too: a P, X or rating moves on to the next photograph.
  const autoAdvance = useSettingsStore((state) => state.appSettings?.autoAdvance === true);
  const file = files[index];

  useEffect(() => {
    const paths = NEIGHBOURS.map((offset) => files[index + offset]?.path).filter((path): path is string => !!path);
    cache.want(paths);
  }, [cache, files, index]);
  useEffect(() => {
    fullCache.want(
      zoomed
        ? FULL_NEIGHBOURS.map((offset) => files[index + offset]?.path).filter((path): path is string => !!path)
        : []
    );
  }, [fullCache, files, index, zoomed]);
  useEffect(() => () => cache.clear(), [cache]);
  useEffect(() => () => fullCache.clear(), [fullCache]);

  useLayoutEffect(() => {
    const element = stage.current;
    if (!element) return;
    const measure = () => setViewSize({ width: element.clientWidth, height: element.clientHeight });
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    root.current?.focus();
    // Escape belongs to the large view while it is open: the dialog must not
    // take it as its own cancel. The key's default is prevented below; this
    // also stops a cancel the webview might still send for it (kept a moment
    // after closing, as that cancel can arrive after the view is gone).
    const dialog = root.current?.closest('dialog');
    if (!dialog) return;
    const swallow = (e: Event) => {
      e.preventDefault();
      e.stopImmediatePropagation();
    };
    dialog.addEventListener('cancel', swallow, true);
    return () => {
      window.setTimeout(() => dialog.removeEventListener('cancel', swallow, true), 250);
    };
  }, []);

  if (!file) return null;
  const preview = cache.get(file.path);
  const full = zoomed ? fullCache.get(file.path) : undefined;
  const fullSize = full?.url ? natural[full.url] : undefined;
  // 100 % is shown (and said) only once the full-size pixels are here.
  const atFull = Boolean(zoomed && full?.url && fullSize);
  const checked = selected.has(file.path);
  const rating = ratings[file.path] ?? 0;
  const name = file.path.split(/[\\/]/).pop() ?? file.path;
  const info = preview?.info;
  const capture = [info?.iso, info?.exposure, info?.aperture, info?.focalLength].filter(Boolean).join(' · ');
  const step = (by: number) => {
    const next = Math.min(files.length - 1, Math.max(0, index + by));
    if (next !== index) onIndexChange(next);
  };
  const image = preview?.url ?? thumbnail;
  const dpr = window.devicePixelRatio || 1;
  // At 100 % one image pixel is one screen pixel.
  const shown = fullSize ? { width: fullSize.width / dpr, height: fullSize.height / dpr } : null;
  const offset = shown ? zoomOffset(focus, shown, viewSize) : null;

  /** The share of the fitted image under the pointer, for zooming there. */
  const shareAt = (e: React.PointerEvent | React.MouseEvent): ZoomFocus | null => {
    const img = stage.current?.querySelector<HTMLImageElement>('img.session-ingest-loupe-fit');
    const box = img?.getBoundingClientRect();
    if (!img || !box || !box.width || !box.height) return null;
    // The image is fitted inside its box (object-fit: contain).
    const scale =
      img.naturalWidth && img.naturalHeight
        ? Math.min(box.width / img.naturalWidth, box.height / img.naturalHeight)
        : 0;
    const width = scale ? img.naturalWidth * scale : box.width;
    const height = scale ? img.naturalHeight * scale : box.height;
    const left = box.left + (box.width - width) / 2;
    const top = box.top + (box.height - height) / 2;
    return { x: clampShare((e.clientX - left) / width), y: clampShare((e.clientY - top) / height) };
  };
  const toggleZoom = (at?: ZoomFocus | null) => {
    if (zoomed) setZoomed(false);
    else {
      if (at) setFocus(at);
      setZoomed(true);
    }
  };

  return (
    <div
      ref={root}
      className="session-ingest-loupe"
      role="region"
      aria-label={t('session.ingest.largeView')}
      tabIndex={-1}
      onKeyDown={(e) => {
        const review = reviewKey(e);
        if (review) {
          if (review.kind === 'include' && !checked) onToggle(file.path);
          else if (review.kind === 'exclude' && checked) onToggle(file.path);
          else if (review.kind === 'rate') onReview?.(file.path, review);
          if (autoAdvance) step(1);
          e.preventDefault();
          e.stopPropagation();
          return;
        }
        if (e.metaKey || e.ctrlKey || e.altKey) return;
        switch (e.key) {
          case 'ArrowRight':
          case 'ArrowDown':
            step(1);
            break;
          case 'ArrowLeft':
          case 'ArrowUp':
            step(-1);
            break;
          case 'Home':
            step(-files.length);
            break;
          case 'End':
            step(files.length);
            break;
          case 'z':
          case 'Z':
            toggleZoom();
            break;
          case ' ':
          case 'Escape':
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
        if (e.key === ' ') e.preventDefault();
      }}
    >
      <div className="session-ingest-loupe-bar">
        <button
          type="button"
          role="checkbox"
          aria-checked={checked}
          className={`session-ingest-loupe-check${checked ? ' is-checked' : ''}`}
          onClick={() => onToggle(file.path)}
        >
          <span className="session-ingest-check" aria-hidden>
            {checked && <Check size={12} strokeWidth={3} />}
          </span>
          {t('session.ingest.include')}
        </button>
        <div className="session-ingest-loupe-stars" role="group" aria-label={t('session.ingest.rating')}>
          {[1, 2, 3, 4, 5].map((n) => (
            <button
              key={n}
              type="button"
              className={`session-icon-button${n <= rating ? ' is-on' : ''}`}
              aria-label={t('session.ingest.rateStars', { stars: n })}
              aria-pressed={n <= rating}
              title={t('session.ingest.rateHint')}
              onClick={() => onReview?.(file.path, { kind: 'rate', rating: n === rating ? 0 : n })}
            >
              <Star size={13} fill={n <= rating ? 'currentColor' : 'none'} />
            </button>
          ))}
        </div>
        <div className="session-ingest-loupe-title">
          <strong title={file.path}>{name}</strong>
          <span>
            {[capture, formatBytes(file.size)].filter(Boolean).join(' · ')}
            {file.duplicate && ` · ${t('session.ingest.inLibrary')}`}
          </span>
        </div>
        <button
          type="button"
          className={`session-ingest-loupe-zoom${zoomed ? ' is-on' : ''}`}
          aria-pressed={zoomed}
          title={t('session.ingest.zoomHint')}
          onClick={() => toggleZoom()}
        >
          {atFull ? '100 %' : zoomed ? t('session.ingest.zoomLoading') : t('session.ingest.zoomFit')}
        </button>
        <span className="session-ingest-loupe-count">
          {(index + 1).toLocaleString()} / {files.length.toLocaleString()}
        </span>
        <button
          type="button"
          className="session-icon-button"
          aria-label={t('session.ingest.previous')}
          disabled={index === 0}
          onClick={() => step(-1)}
        >
          <ChevronLeft size={16} />
        </button>
        <button
          type="button"
          className="session-icon-button"
          aria-label={t('session.ingest.next')}
          disabled={index === files.length - 1}
          onClick={() => step(1)}
        >
          <ChevronRight size={16} />
        </button>
        <button
          type="button"
          className="session-icon-button"
          aria-label={t('session.ingest.backToGrid')}
          title={t('session.ingest.backToGrid')}
          onClick={onClose}
        >
          <X size={16} />
        </button>
      </div>
      <div
        ref={stage}
        className={`session-ingest-loupe-image${atFull ? ' is-zoomed' : ''}`}
        onPointerDown={(e) => {
          if (e.button !== 0) return;
          press.current = { x: e.clientX, y: e.clientY, focus, moved: false };
          e.currentTarget.setPointerCapture?.(e.pointerId);
        }}
        onPointerMove={(e) => {
          const start = press.current;
          if (!start || !atFull || !shown) return;
          const dx = e.clientX - start.x;
          const dy = e.clientY - start.y;
          if (!start.moved && Math.hypot(dx, dy) < CLICK_SLOP) return;
          start.moved = true;
          setFocus({
            x: clampShare(start.focus.x - dx / shown.width),
            y: clampShare(start.focus.y - dy / shown.height)
          });
        }}
        onPointerUp={(e) => {
          const start = press.current;
          press.current = null;
          if (!start || start.moved) return;
          toggleZoom(zoomed ? null : shareAt(e));
        }}
        onPointerCancel={() => {
          press.current = null;
        }}
      >
        {atFull && full?.url && shown && offset ? (
          <img
            key={full.url}
            src={full.url}
            alt={name}
            draggable={false}
            className="session-ingest-loupe-full"
            style={{ width: shown.width, height: shown.height, transform: `translate(${offset.x}px, ${offset.y}px)` }}
          />
        ) : image ? (
          <img
            key={image}
            src={image}
            alt={name}
            draggable={false}
            className={`session-ingest-loupe-fit${preview?.url ? '' : ' is-placeholder'}`}
          />
        ) : preview ? (
          <span>{t('session.ingest.noPreview')}</span>
        ) : null}
        {/* Measures the full-size image once it arrives; shown only then. */}
        {zoomed && full?.url && !fullSize && (
          <img
            src={full.url}
            alt=""
            hidden
            onLoad={(e) => {
              const img = e.currentTarget;
              const url = full.url!;
              setNatural((current) => ({ ...current, [url]: { width: img.naturalWidth, height: img.naturalHeight } }));
            }}
          />
        )}
        {zoomed && full && !full.url && (
          <span className="session-ingest-loupe-note">{t('session.ingest.noFullSize')}</span>
        )}
        {(!preview || (zoomed && !full)) && (
          <LoaderCircle size={18} className="session-ingest-loupe-spinner animate-spin" aria-hidden />
        )}
      </div>
    </div>
  );
}
