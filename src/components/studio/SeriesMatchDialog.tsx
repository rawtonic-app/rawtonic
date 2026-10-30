import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import type { ParseKeys } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { notify } from "../../activity/notify";
import { create } from "zustand";
import { Star } from "lucide-react";
import { ImageMetadata, Invokes } from "../ui/AppProperties";
import { useProcessStore } from "../../store/useProcessStore";
import { recipeAction, type RecipeValues } from "../../utils/appUndo";

/** One photo's proposal (`rawtonic_engine::series_match::Proposal`). */
export interface WhiteBalanceValue {
  temperature: number;
  tint: number;
}
export type MatchStatus = "reference" | "match" | "review" | "keep";
export type MatchReason =
  | "bracket"
  | "silhouette"
  | "cameraCompensation"
  | "colouredLight"
  | "littleInCommon"
  | "anchorsDisagree"
  | "largeChange"
  | "noWhiteBalance"
  | "alreadyMatches"
  | "unreadable"
  | "noReference";
export interface MatchProposal {
  path: string;
  status: MatchStatus;
  reasons: MatchReason[];
  exposure: number | null;
  whiteBalance: WhiteBalanceValue | null;
  confidence: number;
  exposureAnchor: "scene" | "skin" | "skinAndScene" | null;
  whiteBalanceAnchor: "scene" | "skin" | "skinAndScene" | null;
  currentExposure: number;
  currentWhiteBalance: WhiteBalanceValue | null;
}
interface MatchResult {
  proposals: MatchProposal[];
  faceModels: "used" | "missing" | "off";
  elapsedMs: number;
  cancelled: boolean;
}

interface MatchRequest {
  paths: string[];
  references: string[];
}
interface MatchDialogState {
  request: MatchRequest | null;
  open(request: MatchRequest): void;
  close(): void;
}
export const useSeriesMatchDialog = create<MatchDialogState>((set) => ({
  request: null,
  open: (request) => set({ request }),
  close: () => set({ request: null }),
}));

/** The recipe values a proposal writes (only what changes). */
export function proposalValues(proposal: MatchProposal): Record<string, unknown> {
  const values: Record<string, unknown> = {};
  if (proposal.exposure !== null) values.exposure = Math.round(proposal.exposure * 100) / 100;
  if (proposal.whiteBalance) {
    values.whiteBalance = "custom";
    values.wbTemperature = proposal.whiteBalance.temperature;
    values.wbTint = proposal.whiteBalance.tint;
  }
  return values;
}

const fileName = (path: string) => path.split("?vc=")[0].split(/[\\/]/).pop() ?? path;
const signed = (value: number, digits = 2) => `${value >= 0 ? "+" : "−"}${Math.abs(value).toFixed(digits)}`;

export function SeriesMatchDialogHost() {
  const request = useSeriesMatchDialog((s) => s.request);
  if (!request) return null;
  return <SeriesMatchDialog key={request.paths.join("\n")} request={request} />;
}

/** Renders the photo with a proposal applied, as the Library would
 * (`snapshot_thumbnail`), once its row is on screen. */
function MatchPreview({ proposal }: { proposal: MatchProposal }) {
  const [url, setUrl] = useState<string | null>(null);
  const holder = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const element = holder.current;
    if (!element) return;
    let gone = false;
    let made: string | null = null;
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      observer.disconnect();
      void previewQueue(async () => {
        if (gone) return;
        const meta = await invoke<ImageMetadata>(Invokes.LoadMetadata, { path: proposal.path });
        const adjustments = { ...((meta.adjustments && !meta.adjustments.is_null ? meta.adjustments : {}) as object), ...proposalValues(proposal) };
        const bytes = await invoke<ArrayBuffer>(Invokes.SnapshotThumbnail, { path: proposal.path, adjustments });
        if (gone) return;
        made = URL.createObjectURL(new Blob([bytes], { type: "image/jpeg" }));
        setUrl(made);
      }).catch((error) => console.error("Match preview failed:", error));
    });
    observer.observe(element);
    return () => {
      gone = true;
      observer.disconnect();
      if (made) URL.revokeObjectURL(made);
    };
  }, [proposal]);
  return (
    <div ref={holder} className="studio-series-match-thumb">
      {url ? <img src={url} alt="" /> : <span className="studio-series-match-pending" />}
    </div>
  );
}

/** Two previews develop at a time. */
let previewsRunning = 0;
const previewWaiting: (() => void)[] = [];
async function previewQueue(work: () => Promise<void>) {
  if (previewsRunning >= 2) await new Promise<void>((resolve) => previewWaiting.push(resolve));
  previewsRunning++;
  try {
    await work();
  } finally {
    previewsRunning--;
    previewWaiting.shift()?.();
  }
}

/**
 * Match light across a series: each photo's Exposure and white balance
 * normalised to the reference photos (`rawtonic_engine::series_match`), so
 * one look can then be synced across it. Preview per photo before applying;
 * applying is one undo step for all photos.
 */
function SeriesMatchDialog({ request }: { request: MatchRequest }) {
  const { t } = useTranslation();
  const close = useSeriesMatchDialog((s) => s.close);
  const thumbnails = useProcessStore((s) => s.thumbnails);
  const dialog = useRef<HTMLDivElement>(null);
  const [references, setReferences] = useState<string[]>(request.references);
  const [exposure, setExposure] = useState(true);
  const [whiteBalance, setWhiteBalance] = useState(true);
  const [faces, setFaces] = useState(true);
  const [download, setDownload] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [result, setResult] = useState<MatchResult | null>(null);
  const [ticked, setTicked] = useState<Set<string>>(new Set());
  const [applying, setApplying] = useState(false);
  // Said in the dialog too: the activity centre is behind this modal.
  const [failure, setFailure] = useState<string | null>(null);
  const fail = useCallback(
    (error: unknown) => {
      const text = t("studio.seriesMatch.failed", { error: String(error) });
      setFailure(text);
      notify.error(text);
    },
    [t],
  );

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    dialog.current?.focus();
    return () => {
      before?.focus?.();
      void invoke(Invokes.CancelSeriesMatch).catch(() => undefined);
      void invoke(Invokes.ClearSeriesMatchCache).catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    let gone = false;
    let unlisten: (() => void) | null = null;
    listen<{ done: number; total: number }>("series-match-progress", (event) => {
      if (!gone) setProgress(event.payload);
    }).then((stop) => (gone ? stop() : (unlisten = stop)));
    return () => {
      gone = true;
      unlisten?.();
    };
  }, []);

  // Analysed again whenever the references or options change.
  useEffect(() => {
    if (!exposure && !whiteBalance) {
      setResult(null);
      return;
    }
    let current = true;
    setProgress({ done: 0, total: request.paths.length });
    void invoke(Invokes.CancelSeriesMatch)
      .catch(() => undefined)
      .then(() =>
        invoke<MatchResult>(Invokes.AnalyzeSeriesMatch, {
          paths: request.paths,
          references,
          options: { exposure, whiteBalance },
          useFaces: faces,
          downloadFaceModels: download,
        }),
      )
      .then((next) => {
        if (!current || next.cancelled) return;
        setResult(next);
        setTicked(new Set(next.proposals.filter((p) => p.status === "match").map((p) => p.path)));
        setProgress(null);
      })
      .catch((error) => {
        if (!current) return;
        setProgress(null);
        fail(error);
      });
    return () => {
      current = false;
    };
  }, [request.paths, references, exposure, whiteBalance, faces, download, fail]);

  const proposals = useMemo(() => result?.proposals ?? [], [result]);
  const byPath = useMemo(() => new Map(proposals.map((p) => [p.path, p])), [proposals]);
  const counts = useMemo(
    () => ({
      match: proposals.filter((p) => p.status === "match").length,
      review: proposals.filter((p) => p.status === "review").length,
      keep: proposals.filter((p) => p.status === "keep").length,
    }),
    [proposals],
  );

  const toggleReference = useCallback(
    (path: string) =>
      setReferences((refs) =>
        refs.includes(path) ? (refs.length > 1 ? refs.filter((r) => r !== path) : refs) : [...refs, path],
      ),
    [],
  );

  const apply = async () => {
    const values: RecipeValues[] = proposals
      .filter((p) => ticked.has(p.path) && (p.status === "match" || p.status === "review"))
      .map((p) => ({ path: p.path, values: proposalValues(p) }))
      .filter((v) => Object.keys(v.values).length > 0);
    if (values.length === 0) return;
    setApplying(true);
    try {
      const written = await recipeAction(values);
      // Nothing written: the error was shown; the list stays to try again.
      if (written.length === 0) {
        setApplying(false);
        return;
      }
      notify.success(t("studio.seriesMatch.applied", { count: written.length }));
      close();
    } catch (error) {
      fail(error);
      setApplying(false);
    }
  };

  const keys = (e: KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "Escape") close();
  };
  const title = t("studio.seriesMatch.title", { count: request.paths.length });
  const busy = progress !== null;
  const chosen = proposals.filter((p) => ticked.has(p.path) && (p.status === "match" || p.status === "review")).length;

  return createPortal(
    <div className="studio-settings studio-smart-album-backdrop" onKeyDown={keys}>
      <div
        ref={dialog}
        className="studio-series-match"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
      >
        <h2>{title}</h2>
        <p className="studio-help">{t("studio.seriesMatch.intro")}</p>
        <div className="studio-series-match-options">
          <label>
            <input type="checkbox" checked={exposure} onChange={(e) => setExposure(e.target.checked)} />
            {t("studio.seriesMatch.exposure")}
          </label>
          <label>
            <input type="checkbox" checked={whiteBalance} onChange={(e) => setWhiteBalance(e.target.checked)} />
            {t("studio.seriesMatch.whiteBalance")}
          </label>
          <label title={t("studio.seriesMatch.facesTitle")}>
            <input type="checkbox" checked={faces} onChange={(e) => setFaces(e.target.checked)} />
            {t("studio.seriesMatch.faces")}
          </label>
          {result?.faceModels === "missing" && faces && !download && (
            <button type="button" onClick={() => setDownload(true)}>
              {t("studio.seriesMatch.downloadFaces")}
            </button>
          )}
        </div>
        {failure && (
          <p className="studio-dialog-failure" role="alert">
            {failure}
          </p>
        )}
        {busy && (
          <div className="studio-series-match-progress" role="status">
            {t("studio.seriesMatch.analysing", { done: progress.done, total: progress.total })}
            <progress max={progress.total || 1} value={progress.done} />
          </div>
        )}
        {!busy && result && (
          <p className="studio-series-match-summary" role="status">
            {t("studio.seriesMatch.summary", {
              match: counts.match,
              review: counts.review,
              keep: counts.keep,
              seconds: (result.elapsedMs / 1000).toFixed(1),
            })}
          </p>
        )}
        <div className="studio-series-match-list" role="table" aria-label={t("studio.seriesMatch.results")}>
          <div className="studio-series-match-row is-head" role="row">
            <span role="columnheader" />
            <span role="columnheader">{t("studio.seriesMatch.reference")}</span>
            <span role="columnheader">{t("studio.seriesMatch.before")}</span>
            <span role="columnheader">{t("studio.seriesMatch.after")}</span>
            <span role="columnheader">{t("studio.seriesMatch.change")}</span>
            <span role="columnheader">{t("studio.seriesMatch.confidence")}</span>
            <span role="columnheader">{t("studio.seriesMatch.note")}</span>
          </div>
          {request.paths.map((path) => {
            const proposal = byPath.get(path);
            const isReference = references.includes(path);
            const changes = proposal && (proposal.status === "match" || proposal.status === "review");
            return (
              <div key={path} className={`studio-series-match-row is-${proposal?.status ?? "pending"}`} role="row">
                <span role="cell">
                  <input
                    type="checkbox"
                    aria-label={t("studio.seriesMatch.applyTo", { name: fileName(path) })}
                    disabled={!changes}
                    checked={!!changes && ticked.has(path)}
                    onChange={(e) =>
                      setTicked((set) => {
                        const next = new Set(set);
                        if (e.target.checked) next.add(path);
                        else next.delete(path);
                        return next;
                      })
                    }
                  />
                </span>
                <span role="cell">
                  <button
                    type="button"
                    className={isReference ? "studio-series-match-ref is-on" : "studio-series-match-ref"}
                    aria-pressed={isReference}
                    title={t("studio.seriesMatch.referenceTitle")}
                    onClick={() => toggleReference(path)}
                  >
                    <Star size={14} />
                  </button>
                </span>
                <span role="cell" className="studio-series-match-thumb">
                  {thumbnails[path] ? <img src={thumbnails[path]} alt="" /> : null}
                  <small>{fileName(path)}</small>
                </span>
                <span role="cell">{changes && proposal ? <MatchPreview proposal={proposal} /> : null}</span>
                <span role="cell" className="studio-series-match-change">
                  {proposal?.exposure != null && (
                    <span>
                      {t("studio.seriesMatch.exposureChange", {
                        value: signed(proposal.exposure - proposal.currentExposure),
                      })}
                    </span>
                  )}
                  {proposal?.whiteBalance && (
                    <span>
                      {proposal.currentWhiteBalance
                        ? t("studio.seriesMatch.whiteBalanceChange", {
                            from: Math.round(proposal.currentWhiteBalance.temperature),
                            to: proposal.whiteBalance.temperature,
                            tint: signed(proposal.whiteBalance.tint - proposal.currentWhiteBalance.tint, 0),
                          })
                        : t("studio.seriesMatch.whiteBalanceTo", { to: proposal.whiteBalance.temperature })}
                    </span>
                  )}
                </span>
                <span role="cell">
                  {proposal && proposal.status !== "reference" && proposal.status !== "keep" && (
                    <span className="studio-series-match-confidence" title={`${Math.round(proposal.confidence * 100)} %`}>
                      <meter min={0} max={1} low={0.4} high={0.7} optimum={1} value={proposal.confidence} />
                      {Math.round(proposal.confidence * 100)} %
                    </span>
                  )}
                </span>
                <span role="cell" className="studio-series-match-note">
                  {proposal?.status === "reference" && t("studio.seriesMatch.isReference")}
                  {proposal?.reasons
                    .map((reason) => t(`studio.seriesMatch.reasons.${reason}` as ParseKeys))
                    .join(" · ")}
                  {proposal?.exposureAnchor === "skinAndScene" && proposal.status !== "keep" && (
                    <> {t("studio.seriesMatch.facesUsed")}</>
                  )}
                </span>
              </div>
            );
          })}
        </div>
        <p className="studio-help">{t("studio.seriesMatch.thenSync")}</p>
        <footer>
          <button onClick={close}>{t("studio.seriesMatch.cancel")}</button>
          <button className="is-primary" disabled={busy || applying || chosen === 0} onClick={() => void apply()}>
            {t("studio.seriesMatch.apply", { count: chosen })}
          </button>
        </footer>
      </div>
    </div>,
    document.body,
  );
}
