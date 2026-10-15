import { createElement, Fragment, Profiler, type ProfilerOnRenderCallback, type ReactNode, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useEditorStore } from "../store/useEditorStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { useUIStore } from "../store/useUIStore";
import type { LatestPreviewQueue } from "../utils/latestPreviewQueue";

// Explicit profiling builds only. No frame loop, subscriptions or IPC in normal builds.
export const performanceDiagnosticsEnabled =
  import.meta.env.VITE_RAWTONIC_PERF === "1";

const browserFrames = new Map<string, { returned: number; input: number; job: number }>();
export function noteBrowserPreview(url: string, returned: number, input: number, job: number) {
  if (!performanceDiagnosticsEnabled) return;
  browserFrames.set(url, { returned, input, job });
  if (browserFrames.size > 64) browserFrames.delete(browserFrames.keys().next().value!);
}
/** SVG load at full opacity: decoded image availability, not display scanout. */
export function reportBrowserPreviewLoad(url: string, opaque: boolean) {
  const frame = browserFrames.get(url);
  if (!performanceDiagnosticsEnabled || !frame) return;
  const now = performance.now();
  reportPerformance('browser_preview_load', {
    job: frame.job, opaque: opaque ? 1 : 0,
    return_to_load_ms: now - frame.returned,
    ...(frame.input > 0 ? { input_to_load_ms: now - frame.input } : {}),
  });
}

/** Wall-clock time (ms since the epoch) of a `performance.now()` value, for
 * joining webview records with native ones. `performance.timeOrigin` is not
 * used: WebKit's monotonic clock stops while the Mac sleeps, so after a sleep
 * `timeOrigin + now` trailed the native clock (by ~16 ms in one run). */
export function epochOf(time: number) {
  return Date.now() - performance.now() + time;
}

let placeholderSelection: { path: string; started: number } | null = null;
export function reportImagePlaceholderLoad(path: string, url: string) {
  if (!performanceDiagnosticsEnabled || placeholderSelection?.path !== path) return;
  const current = useEditorStore.getState();
  if (current.selectedImage?.path !== path || current.hasRenderedFirstFrame ||
      current.selectedImage.thumbnailUrl !== url) return;
  reportPerformance('selection_to_placeholder_loaded', {
    ms: performance.now() - placeholderSelection.started,
    priority_jpeg: Number(current.selectedImage.thumbnailSource === 'embedded'),
  });
  placeholderSelection = null;
}

const pendingOverlayLoads = new Map<string, number>();
export function trackMaskOverlayLoad(url: string, requestedAt: number) {
  if (performanceDiagnosticsEnabled) pendingOverlayLoads.set(url, requestedAt);
}
export function forgetMaskOverlayLoad(url: string) {
  if (performanceDiagnosticsEnabled) pendingOverlayLoads.delete(url);
}
export function reportMaskOverlayLoad(image: HTMLImageElement) {
  if (!performanceDiagnosticsEnabled) return;
  const started = pendingOverlayLoads.get(image.src);
  pendingOverlayLoads.delete(image.src);
  // Target and current (mid-transition) opacity show whether the overlay is
  // actually displayed while a stroke or shape drag is in progress.
  if (started !== undefined) reportPerformance('mask_overlay_image_loaded', {
    ms: performance.now() - started,
    opacity: Number(image.style.opacity || 1),
    computed_opacity: Number(getComputedStyle(image).opacity),
  });
}

// Time stamp (performance.now() base) of the newest pointer event, recorded in
// the capture phase so that handlers of the same event already see it.
let latestPointerInput = 0;
// When the capture listener ran for that event: the gap to its time stamp is
// input delivery (WebKit and main-thread delays), not our handlers.
let latestPointerDispatch = 0;
/** The event's own time stamp, or its arrival when that is implausible
 * (synthetic input can carry a zero time stamp). */
function eventTime(event: Event) {
  const now = performance.now();
  return event.timeStamp > 0 && event.timeStamp <= now && now - event.timeStamp < 1000
    ? event.timeStamp
    : now;
}
export function latestPointerInputTime() {
  return latestPointerInput;
}
export function latestPointerDispatchTime() {
  return latestPointerDispatch;
}

// Render queues whose counters a pointer session reports (profiling builds).
type QueueName = "preview" | "overlay";
type QueueStats = Pick<LatestPreviewQueue<unknown>, "stats">;
const diagnosticQueues = new Map<QueueName, QueueStats>();
export function registerDiagnosticQueue(name: QueueName, queue: QueueStats) {
  if (!performanceDiagnosticsEnabled) return;
  diagnosticQueues.set(name, queue);
  return () => {
    if (diagnosticQueues.get(name) === queue) diagnosticQueues.delete(name);
  };
}

/** One press–drag–release on the canvas or a slider with real input: event rates,
 * animation-frame gaps and how many render requests were dropped in favour of
 * newer pointer states. */
interface PointerSession {
  pointerType: string;
  maskType: string;
  started: number;
  ended: number;
  moveTimes: number[];
  dispatchTimes: number[];
  coalesced: number;
  frameIntervals: number[];
  movesPerFrame: number[];
  movesThisFrame: number;
  queueStart: Map<QueueName, { submitted: number; superseded: number; dispatched: number }>;
  inputMs: Record<QueueName, number[]>;
}
let pointerSession: PointerSession | null = null;

/** A live preview or mask overlay completed for the current pointer session. */
export function notePointerSessionFrame(queue: QueueName, inputMs: number) {
  if (pointerSession && !pointerSession.ended && inputMs > 0) pointerSession.inputMs[queue].push(inputMs);
}

const percentile = (sorted: number[], fraction: number) =>
  sorted.length ? sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * fraction))] : 0;

function summarize(prefix: string, values: number[]) {
  const sorted = [...values].sort((a, b) => a - b);
  return {
    [`${prefix}_p50_ms`]: percentile(sorted, 0.5),
    [`${prefix}_p95_ms`]: percentile(sorted, 0.95),
    [`${prefix}_max_ms`]: sorted[sorted.length - 1] ?? 0,
  };
}

function finishPointerSession(session: PointerSession) {
  if (pointerSession === session) pointerSession = null;
  if (session.moveTimes.length < 3) return;
  const intervals = (times: number[]) => times.slice(1).map((time, index) => time - times[index]);
  const moveSpan = session.moveTimes[session.moveTimes.length - 1] - session.moveTimes[0];
  const frameP50 = percentile([...session.frameIntervals].sort((a, b) => a - b), 0.5);
  const queues: Record<string, number> = {};
  for (const name of ["preview", "overlay"] as const) {
    const stats = diagnosticQueues.get(name)?.stats;
    const start = session.queueStart.get(name);
    queues[`${name}_submitted`] = stats && start ? stats.submitted - start.submitted : 0;
    queues[`${name}_superseded`] = stats && start ? stats.superseded - start.superseded : 0;
    queues[`${name}_dispatched`] = stats && start ? stats.dispatched - start.dispatched : 0;
    queues[`${name}_frames`] = session.inputMs[name].length;
    Object.assign(queues, summarize(`${name}_input`, session.inputMs[name]));
  }
  reportPerformance("pointer_session", {
    pointer_type: session.pointerType,
    mask_type: session.maskType,
    start_epoch_ms: epochOf(session.started),
    end_epoch_ms: epochOf(session.ended),
    duration_ms: session.ended - session.started,
    moves: session.moveTimes.length,
    // Hardware samples merged into the dispatched moves; -1 if unsupported.
    coalesced_moves: session.coalesced,
    move_hz: moveSpan > 0 ? ((session.moveTimes.length - 1) * 1000) / moveSpan : 0,
    ...summarize("move_interval", intervals(session.moveTimes)),
    // When the handlers ran: input delivery including main-thread delays.
    ...summarize("dispatch_interval", intervals(session.dispatchTimes)),
    frames: session.frameIntervals.length,
    ...summarize("frame", session.frameIntervals),
    long_frames: session.frameIntervals.filter((ms) => ms > frameP50 * 1.5).length,
    max_moves_per_frame: Math.max(0, ...session.movesPerFrame),
    frames_without_moves: session.movesPerFrame.filter((moves) => moves === 0).length,
    ...queues,
  });
}

function activeMaskType() {
  const { adjustments, activeMaskId, activeMaskContainerId } = useEditorStore.getState();
  if (!activeMaskContainerId) return "none";
  const container = adjustments.masks?.find((mask) => mask.id === activeMaskContainerId);
  const subMask = container?.subMasks?.find((mask) => mask.id === activeMaskId);
  return String(subMask?.type ?? "layer");
}

export function reportPerformance(
  stage: string,
  values: Record<string, number | string | number[][]>,
) {
  if (!performanceDiagnosticsEnabled) return;
  void invoke("frontend_log", {
    level: "info",
    // at_epoch_ms: when the record was made, to order it against native
    // records and long frames.
    message: `[PERF] ${JSON.stringify({ stage, ...values, at_epoch_ms: Date.now() })}`,
  }).catch(() => {});
}

// React commits that rendered for at least this long are logged, to split
// long frames into React rendering and the rest (DOM commit, style, layout,
// paint). Profiling builds use react-dom's profiling build (vite.config.mjs).
const SLOW_COMMIT_MS = 8;
const reportSlowCommit: ProfilerOnRenderCallback = (id, phase, actualDuration, baseDuration, startTime, commitTime) => {
  if (actualDuration < SLOW_COMMIT_MS) return;
  reportPerformance("react_commit", {
    id,
    phase,
    render_ms: Math.round(actualDuration * 10) / 10,
    base_ms: Math.round(baseDuration * 10) / 10,
    start_epoch_ms: Math.round(epochOf(startTime)),
    commit_epoch_ms: Math.round(epochOf(commitTime)),
  });
};

/** Profiling builds: forces style and layout now and logs how long it took,
 * which splits a long frame into React, style/layout and paint. Call from a
 * layout effect right after the DOM changed. */
export function reportForcedLayout(stage: string, values: Record<string, number> = {}) {
  if (!performanceDiagnosticsEnabled) return;
  const started = performance.now();
  void document.documentElement.getBoundingClientRect().height;
  void document.body.offsetHeight;
  const ms = performance.now() - started;
  reportPerformance(stage, { ...values, ms: Math.round(ms * 10) / 10, start_epoch_ms: Math.round(epochOf(started)) });
}

/** Logs slow React commits of `children` in profiling builds; a plain
 * fragment otherwise. */
export function PerfProfiler({ id, children }: { id: string; children: ReactNode }) {
  return performanceDiagnosticsEnabled
    ? createElement(Profiler, { id, onRender: reportSlowCommit }, children)
    : createElement(Fragment, null, children);
}

export function usePerformanceDiagnostics() {
  useEffect(() => {
    if (!performanceDiagnosticsEnabled) return;
    let selectionStart = 0;
    let pendingPath: string | null = null;
    let pendingDeveloped: string | null = null;
    const unsubscribe = useEditorStore.subscribe((state, previous) => {
      if (state.selectedImage?.path !== previous.selectedImage?.path) {
        selectionStart = performance.now();
        pendingPath = state.selectedImage?.path ?? null;
        pendingDeveloped = pendingPath;
        placeholderSelection = pendingPath ? { path: pendingPath, started: selectionStart } : null;
        // Joined natively with the display's next new-photo frame.
        if (pendingPath) reportPerformance("photo_selected", { ms: 0 });
      }
      // The first developed frame, from a preview proxy or the full decode.
      if (
        pendingDeveloped &&
        state.hasRenderedFirstFrame &&
        (state.selectedImage?.isPreviewReady || state.selectedImage?.isReady)
      ) {
        reportPerformance("selection_to_developed_frame", {
          ms: performance.now() - selectionStart,
          proxy: state.selectedImage?.isReady ? 0 : 1,
          width: state.originalSize.width,
          height: state.originalSize.height,
        });
        pendingDeveloped = null;
      }
      if (
        pendingPath &&
        state.hasRenderedFirstFrame &&
        state.selectedImage?.isReady
      ) {
        reportPerformance("selection_to_first_frame", {
          ms: performance.now() - selectionStart,
          width: state.originalSize.width,
          height: state.originalSize.height,
        });
        pendingPath = null;
        placeholderSelection = null;
      }
    });

    let lastInput = 0;
    const interacted = () => {
      lastInput = performance.now();
    };
    const events = ["pointerdown", "pointermove", "wheel", "keydown"] as const;
    events.forEach((event) =>
      window.addEventListener(event, interacted, { passive: true }),
    );
    const pointerInput = (event: PointerEvent) => {
      latestPointerInput = eventTime(event);
      latestPointerDispatch = performance.now();
      const session = pointerSession;
      if (event.type === "pointermove" && session && !session.ended) {
        session.moveTimes.push(latestPointerInput);
        session.dispatchTimes.push(performance.now());
        session.movesThisFrame++;
        if (session.coalesced >= 0) session.coalesced += event.getCoalescedEvents().length;
      }
    };
    let finishTimer = 0;
    const pointerDown = (event: PointerEvent) => {
      pointerInput(event);
      if (pointerSession) {
        window.clearTimeout(finishTimer);
        finishPointerSession(pointerSession);
      }
      // Sessions cover drags on the canvas and on Develop's sliders.
      const slider = event.target instanceof HTMLInputElement && event.target.type === "range";
      if (event.button !== 0 || !(slider || event.target instanceof HTMLCanvasElement) ||
          useUIStore.getState().activeView !== "editor") return;
      pointerSession = {
        pointerType: event.pointerType,
        maskType: slider ? "slider" : activeMaskType(),
        started: performance.now(),
        ended: 0,
        moveTimes: [],
        dispatchTimes: [],
        coalesced: typeof event.getCoalescedEvents === "function" ? 0 : -1,
        frameIntervals: [],
        movesPerFrame: [],
        movesThisFrame: 0,
        queueStart: new Map(Array.from(diagnosticQueues, ([name, queue]) => [name, { ...queue.stats }])),
        inputMs: { preview: [], overlay: [] },
      };
    };
    const pointerUp = () => {
      const session = pointerSession;
      if (!session || session.ended) return;
      session.ended = performance.now();
      // Frames already in flight at release still report their latency.
      finishTimer = window.setTimeout(() => finishPointerSession(session), 250);
    };
    window.addEventListener("pointermove", pointerInput, { capture: true, passive: true });
    window.addEventListener("pointerdown", pointerDown, { capture: true, passive: true });
    window.addEventListener("pointerup", pointerUp, { capture: true, passive: true });
    window.addEventListener("pointercancel", pointerUp, { capture: true, passive: true });
    let previousFrame = performance.now();
    reportPerformance("frame_loop_start", { epoch_ms: Math.round(epochOf(previousFrame)) });
    let windowStart = previousFrame;
    let intervals: number[] = [];
    let longFrames: [number, number][] = [];
    let activeFrames = 0;
    let frame = 0;
    const sample = (now: number) => {
      if (!document.hidden) {
        intervals.push(now - previousFrame);
        // End time (wall clock) and length of frames over 50 ms, to join
        // them with native and library events in the log.
        if (now - previousFrame > 50) longFrames.push([epochOf(now), Math.round(now - previousFrame)]);
        if (now - lastInput < 1000) activeFrames++;
        const session = pointerSession;
        if (session && !session.ended) {
          session.frameIntervals.push(now - previousFrame);
          session.movesPerFrame.push(session.movesThisFrame);
          session.movesThisFrame = 0;
        }
      }
      previousFrame = now;
      if (now - windowStart >= 5000 && intervals.length) {
        intervals.sort((a, b) => a - b);
        reportPerformance("ui_frames", {
          count: intervals.length,
          active_frames: activeFrames,
          p50_ms: intervals[Math.floor(intervals.length * 0.5)],
          p95_ms: intervals[Math.floor(intervals.length * 0.95)],
          max_ms: intervals[intervals.length - 1],
          over_50ms: intervals.filter((ms) => ms > 50).length,
          assets: useLibraryStore.getState().imageList.length,
          mounted_tiles: document.querySelectorAll(".studio-photo").length,
          thumbnails_ready: Array.from(
            document.querySelectorAll<HTMLImageElement>(
              ".studio-photo-image img",
            ),
          ).filter((image) => image.complete && image.naturalWidth > 0).length,
          develop: Number(useUIStore.getState().activeView === "editor"),
          long_frames: longFrames,
        });
        intervals = [];
        longFrames = [];
        activeFrames = 0;
        windowStart = now;
      }
      frame = requestAnimationFrame(sample);
    };
    frame = requestAnimationFrame(sample);
    return () => {
      unsubscribe();
      placeholderSelection = null;
      cancelAnimationFrame(frame);
      events.forEach((event) => window.removeEventListener(event, interacted));
      window.clearTimeout(finishTimer);
      pointerSession = null;
      window.removeEventListener("pointermove", pointerInput, { capture: true });
      window.removeEventListener("pointerdown", pointerDown, { capture: true });
      window.removeEventListener("pointerup", pointerUp, { capture: true });
      window.removeEventListener("pointercancel", pointerUp, { capture: true });
    };
  }, []);
}
