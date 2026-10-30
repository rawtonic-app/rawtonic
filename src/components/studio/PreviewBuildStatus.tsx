import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import i18n from "i18next";
import { Invokes, type PreviewBuildProgress } from "../ui/AppProperties";
import { useActivityStore } from "../../store/useActivityStore";
import { useLibraryStore } from "../../store/useLibraryStore";

export const PREVIEW_BUILD_EVENT = "preview-build-progress";

const PREVIEW_ACTIVITY = "previews:build";

/** Reports a preview build to the activity centre: progress and Cancel
 * (Stop) while it runs, then what could not be built under Needs attention
 * until dismissed. Renders nothing itself. */
export function reportPreviewBuild(progress: PreviewBuildProgress) {
  const activity = useActivityStore.getState();
  if (progress.running) {
    activity.start({
      id: PREVIEW_ACTIVITY,
      source: "app",
      title: i18n.t("previews.status.building", { done: progress.done, total: progress.total }),
      progress: { done: progress.done, total: Math.max(progress.total, 1) },
      cancel: () => invoke(Invokes.StopPreviewBuild).catch(() => {}),
    });
  } else if (progress.failedCount > 0) {
    activity.report("warning", {
      id: PREVIEW_ACTIVITY,
      source: "app",
      title: i18n.t("previews.status.notBuilt", { failed: progress.failedCount }),
      detail: progress.failed.map((failure) => `${failure.path.split(/[\\/]/).pop()}: ${failure.reason}`).join("\n"),
      paths: progress.failed.map((failure) => failure.path),
    });
  } else if (progress.stopped) {
    activity.finish(PREVIEW_ACTIVITY, { title: i18n.t("previews.status.stopped", { done: progress.done }) });
  } else {
    activity.finish(PREVIEW_ACTIVITY);
  }
}

/** Applies the preview limits when a library opens and follows preview
 * builds into the activity centre. */
export default function PreviewBuildStatus() {
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
    let heard = false;
    // Promise.resolve().then: without the native side (tests) listen and
    // invoke throw at once.
    const unlisten = Promise.resolve()
      .then(() =>
        listen<PreviewBuildProgress>(PREVIEW_BUILD_EVENT, (event) => {
          heard = true;
          reportPreviewBuild(event.payload);
        }),
      )
      .catch(() => () => {});
    void Promise.resolve()
      .then(() => invoke<PreviewBuildProgress>(Invokes.PreviewBuildProgress))
      .then((current) => {
        if (active && !heard && current?.running) reportPreviewBuild(current);
      })
      .catch(() => {});
    return () => {
      active = false;
      void unlisten.then((stop) => stop());
    };
  }, []);

  return null;
}
