import { memo, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import i18n from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { notify } from "../../activity/notify";
import { ChevronDown, ChevronRight, X } from "lucide-react";
import {
  currentExportEventSeq,
  nextExportRead,
  useProcessStore,
  type ExportItem,
  type ExportItemLive,
  type ExportJob,
} from "../../store/useProcessStore";
import { outputFolder } from "../../utils/exportFeedback";
import { renderErrorMessage } from "../../utils/renderErrors";
import { exportTimeLeft, splitTimeLeft } from "../../utils/exportTimeLeft";
import { Invokes } from "../ui/AppProperties";
import { useSettingsStore } from "../../store/useSettingsStore";

/** What a queued job's state means for the photographer. */
export function describeExportJob(job: ExportJob): string {
  const counts = { done: job.done, total: job.total };
  switch (job.state) {
    case "queued":
      return job.done
        ? i18n.t("studio.export.queue.waitingProgress", counts)
        : i18n.t("studio.export.queue.waiting");
    case "running":
      return i18n.t("studio.export.queue.running", counts);
    case "interrupted":
      return i18n.t("studio.export.queue.interrupted", counts);
    case "cancelled":
      return i18n.t("studio.export.queue.cancelled", counts);
    case "paused":
      return i18n.t("studio.export.queue.paused", counts);
    case "failed":
      return i18n.t("studio.export.queue.failed", {
        failed: job.failed,
        total: job.total,
      });
    default:
      return i18n.t("studio.export.queue.done");
  }
}

/** What one photo of a job is doing or did. */
export function describeExportItem(item: ExportItemLive): string {
  switch (item.state) {
    case "pending":
      return i18n.t("studio.export.queue.photo.waiting");
    case "failed":
      return renderErrorMessage(item.error ?? "");
    case "done":
      return item.startedMs != null && item.finishedMs != null
        ? i18n.t("studio.export.queue.photo.doneIn", {
            seconds: ((item.finishedMs - item.startedMs) / 1000).toFixed(1),
          })
        : i18n.t("studio.export.queue.done");
    default:
      switch (item.stage) {
        case "denoising":
          return i18n.t("studio.export.queue.photo.denoising", {
            percent: Math.floor((item.fraction ?? 0) * 100),
          });
        case "rendering":
          return i18n.t("studio.export.queue.photo.rendering");
        case "masks":
          return i18n.t("studio.export.queue.photo.masks");
        default:
          return i18n.t("studio.export.queue.photo.loading");
      }
  }
}

/** "… left" for a running job, or "Estimating…" before a photo finished. */
export function describeTimeLeft(ms: number | null): string {
  if (ms == null) return i18n.t("studio.export.queue.eta.estimating");
  const { hours, minutes, seconds } = splitTimeLeft(ms);
  if (hours) return i18n.t("studio.export.queue.eta.hours", { hours, minutes });
  if (minutes) return i18n.t("studio.export.queue.eta.minutes", { minutes });
  return i18n.t("studio.export.queue.eta.seconds", { seconds });
}

/** The file name of an export source (a virtual copy's path ends in `?vc=…`). */
function exportFileName(path: string): string {
  const file = path.split("?vc=")[0];
  return file.slice(Math.max(file.lastIndexOf("/"), file.lastIndexOf("\\")) + 1);
}

function act(command: Invokes, args: Record<string, unknown>) {
  return invoke(command, args).catch((error) => notify.error(String(error)));
}

const readsInFlight = new Map<string, number>();

/** Whether the store follows a job's photos: read, or a read on its way. */
export function followsExportItems(job: string): boolean {
  return !!useProcessStore.getState().exportItems[job] || (readsInFlight.get(job) ?? 0) > 0;
}

/** Reads a job's photos into the store (the queue panel's per-photo rows). */
export function loadExportItems(job: string) {
  const since = currentExportEventSeq();
  const read = nextExportRead();
  readsInFlight.set(job, (readsInFlight.get(job) ?? 0) + 1);
  return invoke<ExportItem[]>(Invokes.ExportQueueItems, { job })
    .then((items) => {
      if (Array.isArray(items)) useProcessStore.getState().setExportItems(job, items, since, read);
    })
    .catch(() => {})
    .finally(() => {
      const left = (readsInFlight.get(job) ?? 1) - 1;
      if (left > 0) readsInFlight.set(job, left);
      else readsInFlight.delete(job);
    });
}

/** Shows the first exported file of a job in the file manager. */
async function revealJob(job: ExportJob) {
  let items = useProcessStore.getState().exportItems[job.id];
  if (!items) {
    await loadExportItems(job.id);
    items = useProcessStore.getState().exportItems[job.id];
  }
  const output = items?.find((item) => item.state === "done" && item.output)?.output;
  if (output) await act(Invokes.ShowInFinder, { path: output });
}

/** Ticks once a second while `on`, for the time left. */
function useNow(on: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!on) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [on]);
  return now;
}

// A photo's row re-renders only when that photo changes (200-photo jobs).
const ExportItemRow = memo(function ExportItemRow({ item }: { item: ExportItemLive }) {
  const { t } = useTranslation();
  const denoising = item.state === "running" && item.stage === "denoising";
  return (
    <li className={`is-${item.state}`}>
      <strong>{exportFileName(item.path)}</strong>
      <span>{describeExportItem(item)}</span>
      {denoising && (
        <progress
          max={1}
          value={item.fraction ?? 0}
          aria-label={t("studio.export.queue.photo.denoiseProgress")}
        />
      )}
      {item.state === "done" && item.output && (
        <button type="button" onClick={() => void act(Invokes.ShowInFinder, { path: item.output })}>
          {t("studio.export.queue.show")}
        </button>
      )}
    </li>
  );
});

function ExportJobRow({ job, held, full }: { job: ExportJob; held: boolean; full: boolean }) {
  const { t } = useTranslation();
  const isMac = useSettingsStore((s) => s.osPlatform === "macos");
  const [open, setOpen] = useState(false);
  const items = useProcessStore((s) => s.exportItems[job.id]);
  const pausing = useProcessStore((s) => s.exportPausing.includes(job.id));
  useEffect(() => {
    if (full && job.state === "done" && !items && !followsExportItems(job.id)) void loadExportItems(job.id);
  }, [full, job.id, job.state, items]);
  const folders = [...new Set((items ?? []).filter((item) => item.state === "done" && item.output).map((item) => outputFolder(item.output!)))];
  const running = job.state === "running";
  const now = useNow(running && full);
  const finished = items ? items.filter((item) => item.state === "done").length : job.done;
  const failed = items ? items.filter((item) => item.state === "failed").length : job.failed;
  const toggle = () => {
    if (!open) void loadExportItems(job.id);
    setOpen(!open);
  };
  const id = { job: job.id };
  const resumeLabel =
    job.state === "failed"
      ? t("studio.export.queue.retry")
      : job.state === "queued"
        ? t("studio.export.queue.start")
        : t("studio.export.queue.resume");
  // A waiting job starts by itself unless the queue is held.
  // An Edit With render is never resumed: nothing would hand it over.
  const canResume =
    !job.editWith && job.state !== "running" && job.state !== "done" && (job.state !== "queued" || held);
  // Nor paused: a paused Edit With render would wait for a resume it never gets
  // (its Edit With waits for every render before it hands them over).
  const canPause = !job.editWith && ((job.state === "running" && !pausing) || job.state === "queued");
  const canCancel = job.state === "running" || job.state === "queued" || job.state === "paused";
  return (
    <li className={`studio-export-job is-${job.state}`}>
      <div className="studio-export-job-head">
        <span>
          {job.label}
          <small>
            {/* The job's counts change only between runs; its photos live. */}
            {pausing
              ? t("studio.export.queue.pausing")
              : describeExportJob(items ? { ...job, done: finished, failed } : job)}
            {running && full && ` · ${describeTimeLeft(items ? exportTimeLeft(items, now) : null)}`}
            {folders.map((folder) => <span key={folder} style={{ display: "block", overflowWrap: "anywhere" }}>{folder}</span>)}
          </small>
        </span>
        {canResume && (
          <button type="button" onClick={() => void act(Invokes.ExportQueueResume, id)}>
            {resumeLabel}
          </button>
        )}
        {full && canPause && (
          <button type="button" onClick={() => void act(Invokes.ExportQueuePause, id)}>
            {t("studio.export.queue.pause")}
          </button>
        )}
        {full && canCancel && (
          <button type="button" onClick={() => void act(Invokes.ExportQueueCancel, id)}>
            {t("studio.export.queue.cancel")}
          </button>
        )}
        {full && finished > 0 && (
          <button type="button" onClick={() => void revealJob(job)}>
            {t(isMac ? "studio.library.folders.menu.showInFinder" : "studio.library.folders.menu.showInFileManager")}
          </button>
        )}
        {!running && (
          <button type="button" onClick={() => void act(Invokes.ExportQueueRemove, id)}>
            {t("studio.export.queue.remove")}
          </button>
        )}
      </div>
      {full && job.state !== "done" && (
        <progress
          className="studio-export-job-progress"
          max={job.total}
          value={finished + failed}
          aria-label={t("studio.export.queue.progress", { done: finished + failed, total: job.total })}
        />
      )}
      {!open && job.failures.length > 0 && (
        <ul className="studio-export-failures" aria-label={t("studio.export.queue.failures")}>
          {job.failures.map((failure, index) => (
            <li key={index}>
              <strong>{exportFileName(failure.path)}</strong>
              {renderErrorMessage(failure.error)}
            </li>
          ))}
        </ul>
      )}
      {full && (
        <button type="button" className="studio-export-job-photos" aria-expanded={open} onClick={toggle}>
          {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
          {t("studio.export.queue.photos", { count: job.total })}
        </button>
      )}
      {open && items && (
        <ul className="studio-export-items" aria-label={t("studio.export.queue.photosOf", { label: job.label })}>
          {items.map((item) => (
            <ExportItemRow key={item.index} item={item} />
          ))}
        </ul>
      )}
    </li>
  );
}

/**
 * Export jobs with their state and actions. `full` (the queue panel) adds
 * progress, time left, pause, cancel, reveal and per-photo rows; the Export
 * dialog lists only what waits for the photographer.
 */
export function ExportQueueJobs({ full = false }: { full?: boolean }) {
  // Re-renders the list (and describeExportJob) when the language changes.
  const { t } = useTranslation();
  const all = useProcessStore((s) => s.exportQueue);
  const held = useProcessStore((s) => s.exportQueueHeld);
  const jobs = full ? all : all.filter((job) => job.state !== "done" && job.state !== "running");
  if (!jobs.length)
    return full ? <p className="studio-export-queue-empty">{t("studio.export.queue.empty")}</p> : null;
  return (
    <section className="studio-export-queue" aria-label={t("studio.export.queue.title")}>
      {!full && <h4>{t("studio.export.queue.title")}</h4>}
      <ul>
        {/* Newest first in the panel; the dialog keeps queue order. */}
        {(full ? jobs.slice().reverse() : jobs).map((job) => (
          <ExportJobRow key={job.id} job={job} held={held} full={full} />
        ))}
      </ul>
    </section>
  );
}

/**
 * The export queue beside the work (not a dialog): jobs, their photos,
 * progress and actions, reachable from the top bar at any time.
 */
export function ExportQueuePanel({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const jobs = useProcessStore((s) => s.exportQueue);
  const held = useProcessStore((s) => s.exportQueueHeld);
  const waiting = jobs.some((job) => job.state === "queued");
  const finished = jobs.filter((job) => job.state === "done");
  return (
    <aside
      className="studio-export-queue-panel"
      aria-labelledby="studio-export-queue-title"
      onKeyDown={(e) => {
        // A button's Enter and Space press it, not an app shortcut (the
        // crop tool's Enter would take the press).
        if ((e.key === "Enter" || e.key === " ") && (e.target as HTMLElement).closest("button")) {
          e.stopPropagation();
          return;
        }
        if (e.key !== "Escape") return;
        // Handled here: the app's shortcuts would also revert a crop or
        // leave Develop.
        e.preventDefault();
        onClose();
      }}
    >
      <header>
        <strong id="studio-export-queue-title">{t("studio.export.queue.title")}</strong>
        {(held || waiting) && (
          <button type="button" onClick={() => void act(Invokes.ExportQueueHold, { held: !held })}>
            {held ? t("studio.export.queue.startQueue") : t("studio.export.queue.holdQueue")}
          </button>
        )}
        {finished.length > 0 && (
          <button
            type="button"
            onClick={() => finished.forEach((job) => void act(Invokes.ExportQueueRemove, { job: job.id }))}
          >
            {t("studio.export.queue.clearFinished")}
          </button>
        )}
        <button type="button" aria-label={t("studio.export.queue.close")} onClick={onClose}>
          <X size={15} />
        </button>
      </header>
      {held && waiting && <p className="studio-help">{t("studio.export.queue.held")}</p>}
      <ExportQueueJobs full />
    </aside>
  );
}
