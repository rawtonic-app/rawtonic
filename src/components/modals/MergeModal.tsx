import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { CheckCircle, Loader2, XCircle } from "lucide-react";
import { useEscapeToClose } from "../../hooks/useEscapeToClose";
import type { MergeKind, MergeModalState } from "../../store/useUIStore";

export type Deghost = "off" | "low" | "medium" | "high";
export type Projection =
  "auto" | "spherical" | "cylindrical" | "perspective" | "panini";

export interface MergeOptions {
  hdr: { align: boolean; deghost: Deghost };
  panorama: { projection: Projection; boundaryWarp: number; autoCrop: boolean };
  focusStack: { align: boolean };
}

export const DEFAULT_MERGE_OPTIONS: MergeOptions = {
  hdr: { align: true, deghost: "off" },
  panorama: { projection: "auto", boundaryWarp: 0, autoCrop: true },
  focusStack: { align: true },
};

interface MergeModalProps {
  state: MergeModalState;
  thumbnail: string | null;
  onClose(): void;
  onOpenFile(path: string): void;
  onRun(kind: MergeKind, options: unknown): void;
  /** Panoramas: new boundary warp or crop on the stitched canvas. */
  onRefinish(options: MergeOptions["panorama"]): Promise<void>;
  onSave(autoSettings: boolean, fillEdges: boolean): Promise<string>;
}

interface HdrFrame {
  path: string;
  relativeEv: number;
  shiftPx: number;
  alignFailed: boolean;
  ghostFraction: number;
}

interface PanoramaFrame {
  path: string;
  used: boolean;
  gainEv: number;
}

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** One dialog for the three merges: options, a preview of the merge (the
 * real rendering is Develop's, after saving), and saving as a linear DNG. */
export default function MergeModal({
  state,
  thumbnail,
  onClose,
  onOpenFile,
  onRun,
  onRefinish,
  onSave,
}: MergeModalProps) {
  const { t } = useTranslation();
  const {
    isOpen,
    kind,
    sourcePaths,
    isProcessing,
    progressMessage,
    previewBase64,
    overlayBase64,
    report,
    error,
  } = state;
  const [options, setOptions] = useState<MergeOptions>(DEFAULT_MERGE_OPTIONS);
  const [ranWith, setRanWith] = useState<string | null>(null);
  const [showOverlay, setShowOverlay] = useState(false);
  // Lightroom's "Auto Settings": on for HDR and panoramas, off for stacks.
  const [autoSettings, setAutoSettings] = useState<Record<MergeKind, boolean>>({
    hdr: true,
    panorama: true,
    hdrPanorama: true,
    focusStack: false,
  });
  // Fill edges (AI): only for panoramas kept uncropped; a saving option.
  const [fillEdges, setFillEdges] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [savedPath, setSavedPath] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const started = useRef(false);

  const isPano = kind === "panorama" || kind === "hdrPanorama";
  const hasHdr = kind === "hdr" || kind === "hdrPanorama";
  // An HDR panorama takes both the HDR and the panorama options.
  const current: unknown =
    kind === "hdrPanorama"
      ? { hdr: options.hdr, panorama: options.panorama }
      : options[kind];
  const currentKey = JSON.stringify(current);
  const stale =
    previewBase64 !== null && ranWith !== null && ranWith !== currentKey;

  const run = useCallback(() => {
    const previous = ranWith
      ? (JSON.parse(ranWith) as Record<string, unknown>)
      : null;
    setRanWith(currentKey);
    setSavedPath(null);
    setSaveError(null);
    // Boundary warp and crop apply to the stitched canvas: no new stitch
    // (an HDR panorama also needs its HDR settings unchanged).
    const previousPano = (
      kind === "hdrPanorama" ? previous?.panorama : previous
    ) as Partial<MergeOptions["panorama"]> | undefined;
    const hdrUnchanged =
      kind !== "hdrPanorama" ||
      JSON.stringify(previous?.hdr) === JSON.stringify(options.hdr);
    if (
      isPano &&
      previewBase64 !== null &&
      hdrUnchanged &&
      previousPano?.projection === options.panorama.projection
    ) {
      onRefinish(options.panorama).catch(() => onRun(kind, current));
      return;
    }
    onRun(kind, current);
  }, [
    current,
    currentKey,
    kind,
    onRefinish,
    onRun,
    options.panorama,
    previewBase64,
    ranWith,
  ]);

  // The first preview starts as soon as the dialog opens (Lightroom does
  // the same); later ones on request.
  useEffect(() => {
    if (isOpen && !started.current) {
      started.current = true;
      run();
    }
    if (!isOpen) {
      started.current = false;
      setRanWith(null);
      setSavedPath(null);
      setSaveError(null);
      setShowOverlay(false);
    }
  }, [isOpen, run]);

  const handleClose = useCallback(() => {
    if (!isSaving) onClose();
  }, [isSaving, onClose]);
  useEscapeToClose(isOpen, handleClose);

  const save = async () => {
    setIsSaving(true);
    setSaveError(null);
    try {
      setSavedPath(
        await onSave(
          autoSettings[kind],
          isPano && !options.panorama.autoCrop && fillEdges,
        ),
      );
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setIsSaving(false);
    }
  };

  const set = <K extends keyof MergeOptions>(
    k: K,
    patch: Partial<MergeOptions[K]>,
  ) => setOptions((o) => ({ ...o, [k]: { ...o[k], ...patch } }));

  const title = t(`modals.merge.title.${kind}`);
  const overlayLabel =
    kind === "hdr" ? t("modals.merge.showGhosts") : t("modals.merge.showDepth");

  const summary = useMemo(() => {
    if (!report) return null;
    if (kind === "hdr") {
      const frames = (report.frames as HdrFrame[] | undefined) ?? [];
      return frames.map((f) => (
        <li key={f.path}>
          <span>{fileName(f.path)}</span>
          <span>
            {f.relativeEv >= 0 ? "+" : ""}
            {f.relativeEv.toFixed(2)} EV
            {f.alignFailed
              ? ` · ${t("modals.merge.notAligned")}`
              : f.shiftPx > 0
                ? ` · ${f.shiftPx.toFixed(1)} px`
                : ""}
            {f.ghostFraction > 0.001
              ? ` · ${t("modals.merge.ghosts")} ${(f.ghostFraction * 100).toFixed(1)} %`
              : ""}
          </span>
        </li>
      ));
    }
    if (isPano) {
      const frames = (report.frames as PanoramaFrame[] | undefined) ?? [];
      const fov = report.fovDeg as [number, number] | undefined;
      return (
        <>
          {fov && (
            <li>
              <span>
                {t(
                  `modals.merge.projection.${report.projection as Projection}`,
                )}
              </span>
              <span>
                {fov[0].toFixed(0)}° × {fov[1].toFixed(0)}°
              </span>
            </li>
          )}
          {frames.map((f) => (
            <li key={f.path}>
              <span>{fileName(f.path)}</span>
              <span>
                {f.used
                  ? `${f.gainEv >= 0 ? "+" : ""}${f.gainEv.toFixed(2)} EV`
                  : t("modals.merge.notUsed")}
              </span>
            </li>
          ))}
        </>
      );
    }
    return null;
  }, [kind, report, t]);

  if (!isOpen) return null;

  const busy = isProcessing || isSaving;
  const segments = <T extends string>(
    values: readonly T[],
    value: T,
    onChange: (v: T) => void,
    label: (v: T) => string,
  ) => (
    <div className="studio-segments merge-segments" role="group">
      {values.map((v) => (
        <button
          key={v}
          type="button"
          aria-pressed={v === value}
          disabled={busy}
          onClick={() => onChange(v)}
        >
          {label(v)}
        </button>
      ))}
    </div>
  );

  return (
    <div
      className="merge-backdrop"
      onMouseDown={(e) => e.target === e.currentTarget && handleClose()}
    >
      <div
        className="merge-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={title}
      >
        <div className="merge-preview">
          {previewBase64 ? (
            <div className="merge-preview-frame">
              <img src={previewBase64} alt={title} />
              {showOverlay && overlayBase64 && (
                <img
                  className={
                    kind === "hdr"
                      ? "merge-overlay"
                      : "merge-overlay merge-overlay-depth"
                  }
                  src={overlayBase64}
                  alt=""
                />
              )}
              {(isProcessing || stale) && (
                <div className="merge-preview-state">
                  {isProcessing
                    ? progressMessage || t("modals.merge.working")
                    : t("modals.merge.stale")}
                </div>
              )}
            </div>
          ) : error ? (
            <div className="merge-message" role="alert">
              <XCircle size={28} />
              <p>{error}</p>
            </div>
          ) : (
            <div className="merge-message">
              {thumbnail && (
                <img className="merge-source" src={thumbnail} alt="" />
              )}
              <Loader2 className="animate-spin" size={20} />
              <p>{progressMessage || t("modals.merge.working")}</p>
            </div>
          )}
        </div>

        <aside className="merge-side">
          <h2>{title}</h2>
          <p className="merge-count">
            {t("modals.merge.photos", { count: sourcePaths.length })}
          </p>

          {hasHdr && (
            <>
              <label className="studio-check merge-check">
                <input
                  type="checkbox"
                  checked={options.hdr.align}
                  disabled={busy}
                  onChange={(e) => set("hdr", { align: e.target.checked })}
                />
                {t("modals.merge.align")}
              </label>
              <span className="merge-label">{t("modals.merge.deghost")}</span>
              {segments(
                ["off", "low", "medium", "high"] as const,
                options.hdr.deghost,
                (deghost) => set("hdr", { deghost }),
                (v) => t(`modals.merge.amount.${v}`),
              )}
            </>
          )}

          {isPano && (
            <>
              <span className="merge-label">
                {t("modals.merge.projectionLabel")}
              </span>
              <div className="merge-projections">
                {segments(
                  ["auto", "spherical", "cylindrical"] as const,
                  options.panorama.projection === "perspective" ||
                    options.panorama.projection === "panini"
                    ? ("" as "auto")
                    : options.panorama.projection,
                  (projection) => set("panorama", { projection }),
                  (v) => t(`modals.merge.projection.${v}`),
                )}
                {segments(
                  ["perspective", "panini"] as const,
                  options.panorama.projection === "perspective" ||
                    options.panorama.projection === "panini"
                    ? options.panorama.projection
                    : ("" as "panini"),
                  (projection) => set("panorama", { projection }),
                  (v) => t(`modals.merge.projection.${v}`),
                )}
              </div>
              <label className="merge-slider">
                <span>
                  {t("modals.merge.boundaryWarp")}
                  <output>{options.panorama.boundaryWarp}</output>
                </span>
                <input
                  type="range"
                  min={0}
                  max={100}
                  value={options.panorama.boundaryWarp}
                  disabled={busy}
                  onChange={(e) =>
                    set("panorama", { boundaryWarp: Number(e.target.value) })
                  }
                />
              </label>
              <label className="studio-check merge-check">
                <input
                  type="checkbox"
                  checked={options.panorama.autoCrop}
                  disabled={busy}
                  onChange={(e) =>
                    set("panorama", { autoCrop: e.target.checked })
                  }
                />
                {t("modals.merge.autoCrop")}
              </label>
              {!options.panorama.autoCrop && (
                <label className="studio-check merge-check">
                  <input
                    type="checkbox"
                    checked={fillEdges}
                    disabled={isSaving}
                    onChange={(e) => setFillEdges(e.target.checked)}
                  />
                  {t("modals.merge.fillEdges")}
                </label>
              )}
            </>
          )}

          {kind === "focusStack" && (
            <label className="studio-check merge-check">
              <input
                type="checkbox"
                checked={options.focusStack.align}
                disabled={busy}
                onChange={(e) => set("focusStack", { align: e.target.checked })}
              />
              {t("modals.merge.align")}
            </label>
          )}

          {overlayBase64 && !isPano && (
            <label className="studio-check merge-check">
              <input
                type="checkbox"
                checked={showOverlay}
                onChange={(e) => setShowOverlay(e.target.checked)}
              />
              {overlayLabel}
            </label>
          )}

          <label className="studio-check merge-check">
            <input
              type="checkbox"
              checked={autoSettings[kind]}
              disabled={isSaving}
              onChange={(e) =>
                setAutoSettings((a) => ({ ...a, [kind]: e.target.checked }))
              }
            />
            {t("modals.merge.autoSettings")}
          </label>

          {summary && <ul className="merge-summary">{summary}</ul>}
          {typeof report?.width === "number" && (
            <p className="merge-size">
              {report.width as number} × {report.height as number} px · DNG
            </p>
          )}

          <div className="merge-actions">
            {savedPath ? (
              <>
                <p className="merge-saved">
                  <CheckCircle size={14} /> {fileName(savedPath)}
                </p>
                <button type="button" onClick={handleClose}>
                  {t("modals.merge.close")}
                </button>
                <button
                  type="button"
                  className="merge-primary"
                  onClick={() => {
                    onOpenFile(savedPath);
                    handleClose();
                  }}
                >
                  {t("modals.merge.open")}
                </button>
              </>
            ) : (
              <>
                {saveError && (
                  <p className="merge-error" role="alert">
                    {saveError}
                  </p>
                )}
                <button type="button" onClick={handleClose} disabled={isSaving}>
                  {t("modals.merge.cancel")}
                </button>
                <button
                  type="button"
                  onClick={run}
                  disabled={
                    busy || (!stale && !error && previewBase64 !== null)
                  }
                >
                  {t("modals.merge.update")}
                </button>
                <button
                  type="button"
                  className="merge-primary"
                  onClick={save}
                  disabled={busy || stale || previewBase64 === null}
                >
                  {isSaving ? (
                    <Loader2 className="animate-spin" size={12} />
                  ) : null}
                  {t("modals.merge.merge")}
                </button>
              </>
            )}
          </div>
        </aside>
      </div>
    </div>
  );
}
