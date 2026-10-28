import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { LoaderCircle, Square, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Invokes, type PreviewBuildProgress } from "../ui/AppProperties";
import { useLibraryStore } from "../../store/useLibraryStore";

export const PREVIEW_BUILD_EVENT = "preview-build-progress";

/** The top bar's report of a preview build: progress and Stop while it
 * runs, then what could not be built until dismissed. */
export default function PreviewBuildStatus() {
  const { t } = useTranslation();
  const [progress, setProgress] = useState<PreviewBuildProgress | null>(null);
  const [dismissed, setDismissed] = useState(false);
  const folder = useLibraryStore((s) => s.sessionDirectory ?? s.currentFolderPath);

  // The preferences' limits for previews apply when a library opens
  // (Lightroom discards expired 1:1 previews at launch): 1:1 previews unused
  // for too long and Standard previews beyond their size limit go.
  useEffect(() => {
    void Promise.resolve()
      .then(() => invoke(Invokes.ApplyPreviewLimits, { folder }))
      .catch(() => {});
  }, [folder]);

  useEffect(() => {
    let active = true;
    // Promise.resolve().then: without the native side (tests) listen and
    // invoke throw at once.
    const unlisten = Promise.resolve()
      .then(() =>
        listen<PreviewBuildProgress>(PREVIEW_BUILD_EVENT, (event) => {
          setProgress(event.payload);
          setDismissed(false);
        }),
      )
      .catch(() => () => {});
    void Promise.resolve()
      .then(() => invoke<PreviewBuildProgress>(Invokes.PreviewBuildProgress))
      .then((current) => active && setProgress((shown) => shown ?? current))
      .catch(() => {});
    return () => {
      active = false;
      void unlisten.then((stop) => stop());
    };
  }, []);

  const finishedWithReport =
    progress && !progress.running && (progress.failedCount > 0 || progress.stopped);


  if (!progress || dismissed || (!progress.running && !finishedWithReport)) return null;

  if (progress.running) {
    // Compact: the top bar is shared with the session's and the workspace's
    // controls.
    return (
      <div
        className="studio-preview-build"
        role="status"
        title={t("previews.status.building", { done: progress.done, total: progress.total })}
      >
        <LoaderCircle size={13} className="animate-spin" />
        <span>{t("previews.status.short", { done: progress.done, total: progress.total })}</span>
        <progress value={progress.done} max={Math.max(progress.total, 1)} />
        <button
          aria-label={t("previews.status.stop")}
          title={t("previews.status.stop")}
          onClick={() => void invoke(Invokes.StopPreviewBuild).catch(() => {})}
        >
          <Square size={11} fill="currentColor" />
        </button>
      </div>
    );
  }

  return (
    <div className="studio-preview-build" role="status">
      {progress.failedCount > 0 ? (
        <details className="studio-preview-failures">
          <summary>{t("previews.status.notBuilt", { failed: progress.failedCount })}</summary>
          <ul>{progress.failed.map((failure, index) => (
            <li key={index}>{failure.path.split("/").pop()}: {failure.reason}</li>
          ))}</ul>
        </details>
      ) : <span>{t("previews.status.stopped", { done: progress.done })}</span>}
      <button aria-label={t("previews.status.dismiss")} onClick={() => setDismissed(true)}>
        <X size={12} />
      </button>
    </div>
  );
}
