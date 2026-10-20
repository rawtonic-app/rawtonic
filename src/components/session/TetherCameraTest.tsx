import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { arch, platform, version } from "@tauri-apps/plugin-os";
import { open } from "@tauri-apps/plugin-shell";
import { Check, ClipboardCopy, ExternalLink, LoaderCircle, Minus, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Invokes } from "../ui/AppProperties";
import {
  STEPS,
  buildReport,
  describe,
  settingToChange,
  type ReportInfo,
  type StepId,
  type StepResult,
} from "./cameraTest";

/** What the test needs of the connected camera (`TetherStatus`). */
export interface TestedCamera {
  model: string;
  firmware?: string | null;
  serial?: string | null;
  transferQueue?: boolean;
  library?: string | null;
  transport?: string;
}

interface Setting {
  key: string;
  label: string;
  value: string;
  choices: string[];
  readOnly: boolean;
}

interface Captured {
  cameraName: string;
  connection?: number | null;
  exposure?: number | null;
  destination: string | null;
  assetId: string | null;
  skipped: boolean;
  error: string | null;
  requested: boolean;
}

/** Something the backend said, with when the test heard it. */
interface Heard {
  name: string;
  payload: unknown;
  at: number;
  /** A photo of a new exposure (not the JPEG or RAW of one already in). */
  exposure?: boolean;
}

const EVENTS = ["tether-captured", "tether-capture-failed", "tether-state", "tether-released", "wgpu-frame-ready"];
/** The helper reports a shot that did not arrive after 60 s. */
const SHOT_WAIT_MS = 75_000;
const SCREEN_WAIT_MS = 10_000;
const BODY_SHOT_WAIT_MS = 30_000;
const UNPLUG_WAIT_MS = 60_000;
const REPLUG_WAIT_MS = 180_000;
const RELEASE_WAIT_MS = 10_000;
const BURST = 5;
const SHOT_COUNTS = [5, 10, 20];
export const ISSUE_URL = "https://github.com/rawtonic-app/rawtonic/issues/new?template=camera-report.yml";

const now = () => performance.now();
const sameFile = (a: string, b: string) => a === b || a.startsWith(`${b}?`);
const captured = (h: Heard) => h.payload as Captured;
/** A shot requested from the app: its first file in (RAW+JPEG come as two). */
const appExposure = (h: Heard) => h.name === "tether-captured" && captured(h).requested && h.exposure === true;
/** Thrown into a running step when the test is closed. */
const CLOSED = "the camera test was closed";

/** Why something failed: `reason` is the test's own words (it goes into
 * the report), `detail` the camera's or the app's message (shown here
 * only: it may name folders or files). */
interface Failure {
  reason: string;
  detail?: string;
}
const refused = (e: unknown): Failure => ({ reason: "refused by the camera or the helper", detail: String(e) });
const notDelivered = (payload: unknown): Failure => {
  const { kind, message } = payload as { kind?: string; message?: string };
  return { reason: `shot not delivered (${/^[a-zA-Z]+$/.test(kind ?? "") ? kind : "other"})`, detail: message };
};
const notAdded = (error: string): Failure => ({ reason: "photo not added to the library", detail: error });
const NO_PHOTO: Failure = { reason: "no photo arrived" };
/** Steps that take photos, and how long after one its last files may come. */
const PHOTO_STEPS: StepId[] = ["shots", "burst", "bodyShot", "replug"];
const SETTLE_MS = 1000;
/** A file not added to the library or a shot the helper reports as not
 * delivered (also the second file of a RAW+JPEG exposure). */
const isFailure = (h: Heard) =>
  (h.name === "tether-captured" && Boolean(captured(h).error)) || h.name === "tether-capture-failed";
const LATE_FAILURE = "a file of its photos failed after the step";

const systemName = () => {
  try {
    return `${platform()} ${version()} (${arch()})`;
  } catch {
    return "unknown";
  }
};

/** The guided camera test (G46): the user runs each step with their own
 * camera; the test measures, and at the end shows the anonymous report the
 * user can copy into a camera report. Nothing is sent anywhere. It frees or
 * deletes nothing itself: transfer-queue releases are the connection's own,
 * after each verified commit. */
export default function TetherCameraTest({
  camera,
  connectMs,
  freeCameraCopies,
  onClose,
}: {
  camera: TestedCamera;
  /** How long the panel's connect took, when the panel connected it. */
  connectMs: number | null;
  freeCameraCopies: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [results, setResults] = useState<Partial<Record<StepId, StepResult>>>(() => ({
    connect: connectMs === null ? { state: "passed" } : { state: "passed", ms: connectMs },
  }));
  const [running, setRunning] = useState<StepId | null>(null);
  const [prompt, setPrompt] = useState("");
  const [shotCount, setShotCount] = useState(10);
  const [appVersion, setAppVersion] = useState("unknown");
  const [copied, setCopied] = useState(false);
  // Files not added to the library and shots not delivered during the test.
  const [failures, setFailures] = useState(0);
  // The photo step running, and the last one that ran: a failure heard
  // after its end (a late RAW or JPEG of its last exposure) fails it.
  const runningStep = useRef<StepId | null>(null);
  const lastPhotoStep = useRef<StepId | null>(null);

  // Everything heard during the test, so a wait never misses what came
  // before it was set up.
  const heard = useRef<Heard[]>([]);
  const waiters = useRef(new Set<(h: Heard | null) => boolean>());
  // Closed: no step asks the camera for anything more.
  const closed = useRef(false);
  // The exposures heard (helper connection and exposure numbers).
  const exposures = useRef(new Set<string>());
  // Committed photos waiting for their release, and what the releases said.
  // (`releasedEarly`: released before their commit was heard.)
  const releases = useRef({
    pending: [] as string[],
    releasedEarly: [] as string[],
    freed: 0,
    notFreed: 0,
    early: 0,
    errors: [] as string[],
  });

  useEffect(() => {
    void getVersion()
      .then(setAppVersion)
      .catch(() => undefined);
    const hear = (name: string, payload: unknown) => {
      const h: Heard = { name, payload, at: now() };
      if (name === "tether-captured") {
        // One exposure as RAW and JPEG counts once: the helper gives both
        // its number.
        const { connection, exposure } = payload as Captured;
        if (connection == null || exposure == null) h.exposure = true;
        else {
          const key = `${connection}/${exposure}`;
          h.exposure = !exposures.current.has(key);
          exposures.current.add(key);
        }
      }
      heard.current.push(h);
      if (isFailure(h)) {
        setFailures((n) => n + 1);
        const late = runningStep.current === null ? lastPhotoStep.current : null;
        if (late)
          setResults((current) => {
            const result = current[late];
            if (!result || result.state === "failed") return current;
            return { ...current, [late]: { ...result, state: "failed", reason: LATE_FAILURE } };
          });
      }
      if (heard.current.length > 500) heard.current.splice(0, 250);
      if (name === "tether-captured") {
        const c = payload as Captured;
        const r = releases.current;
        const early = r.releasedEarly.indexOf(c.cameraName);
        if (early >= 0) r.releasedEarly.splice(early, 1);
        else if (!c.error && (c.assetId || c.skipped)) r.pending.push(c.cameraName);
      } else if (name === "tether-released") {
        const r = payload as { cameraName: string; freed: boolean; error: string | null };
        const at = releases.current.pending.indexOf(r.cameraName);
        if (at < 0) {
          releases.current.early += 1;
          releases.current.releasedEarly.push(r.cameraName);
        } else releases.current.pending.splice(at, 1);
        if (r.error) releases.current.errors.push(r.error);
        else if (r.freed) releases.current.freed += 1;
        else releases.current.notFreed += 1;
      }
      for (const waiter of [...waiters.current]) if (waiter(h)) waiters.current.delete(waiter);
    };
    // Set up again after a cleanup (React's StrictMode runs both once more).
    closed.current = false;
    const stops = EVENTS.map((name) => listen(name, (event) => hear(name, event.payload)));
    const pending = waiters.current;
    return () => {
      closed.current = true;
      // Every wait ends now (null), so no step goes on to the camera.
      for (const waiter of [...pending]) waiter(null);
      pending.clear();
      stops.forEach((stop) => void stop.then((fn) => fn()));
    };
  }, []);

  /** The first event since `since` that `match`es, or null after `ms`. */
  const next = useCallback(
    (match: (h: Heard) => boolean, ms: number, since: number) =>
      new Promise<Heard | null>((resolve) => {
        if (closed.current) return resolve(null);
        const early = heard.current.find((h) => h.at >= since && match(h));
        if (early) return resolve(early);
        const waiter = (h: Heard | null) => {
          if (h !== null && !match(h)) return false;
          window.clearTimeout(timer);
          resolve(h);
          return true;
        };
        const timer = window.setTimeout(() => {
          waiters.current.delete(waiter);
          resolve(null);
        }, ms);
        waiters.current.add(waiter);
      }),
    [],
  );

  /** Asks the camera for something, unless the test was closed. */
  const ask = useCallback(<T,>(command: string, args?: Record<string, unknown>) => {
    if (closed.current) return Promise.reject(new Error(CLOSED));
    return invoke<T>(command, args);
  }, []);


  /** One shot from the app: request → photo in the library → its first
   * Develop frame (when `screen`: Develop shows the captures). */
  const shoot = useCallback(async (screen: boolean) => {
    const since = now();
    try {
      await ask(Invokes.TetherCapture);
    } catch (e) {
      return { error: refused(e) };
    }
    const photo = await next(
      (h) => appExposure(h) || h.name === "tether-capture-failed",
      SHOT_WAIT_MS,
      since,
    );
    if (!photo) return { error: NO_PHOTO };
    if (photo.name === "tether-capture-failed") return { error: notDelivered(photo.payload) };
    const c = captured(photo);
    if (c.error) return { error: notAdded(c.error) };
    const inLibrary = Math.round(photo.at - since);
    const destination = c.destination;
    const frame =
      screen && destination
        ? await next(
            (h) => h.name === "wgpu-frame-ready" && sameFile((h.payload as { path: string }).path, destination),
            SCREEN_WAIT_MS,
            photo.at,
          )
        : null;
    return { inLibrary, onScreen: frame ? Math.round(frame.at - since) : undefined };
  }, [ask, next]);

  const runners: Record<StepId, () => Promise<StepResult>> = {
    connect: async () => ({ state: "passed" }),
    settings: async () => {
      const started = now();
      const settings = await ask<Setting[]>(Invokes.TetherSettings);
      const ms = Math.round(now() - started);
      const changeable = settings.filter((s) => !s.readOnly && s.choices.length > 1).length;
      return settings.length > 0
        ? { state: "passed", ms, settings: settings.length, changeable }
        : { state: "failed", ms, settings: 0, changeable: 0, reason: "no settings read" };
    },
    setSetting: async () => {
      const pick = settingToChange(await ask<Setting[]>(Invokes.TetherSettings));
      if (!pick) return { state: "skipped", reason: "no changeable setting with two values" };
      const { setting, other } = pick;
      const valueOf = async () =>
        (await ask<Setting[]>(Invokes.TetherSettings)).find((s) => s.key === setting.key)?.value;
      const started = now();
      await ask(Invokes.TetherSetSetting, { key: setting.key, value: other });
      const ms = Math.round(now() - started);
      const changed = await valueOf()
        .then((value) => value === other)
        .finally(() => invoke(Invokes.TetherSetSetting, { key: setting.key, value: setting.value }));
      const back = (await valueOf()) === setting.value;
      return changed && back
        ? { state: "passed", ms, setting: setting.label }
        : { state: "failed", ms, reason: `${setting.label}: ${changed ? "not set back" : "the camera kept its value"}` };
    },
    autofocus: async () => {
      const started = now();
      await ask(Invokes.TetherAutofocus);
      return { state: "passed", ms: Math.round(now() - started) };
    },
    shots: async () => {
      const inLibrary: number[] = [];
      const onScreen: number[] = [];
      const errors: Failure[] = [];
      // Develop not showing the first photo: the others are not waited for.
      let screen = true;
      for (let i = 1; i <= shotCount && !closed.current; i += 1) {
        setPrompt(t("session.tether.test.shotOf", { n: i, total: shotCount }));
        const shot = await shoot(screen);
        if (shot.onScreen === undefined) screen = false;
        if (shot.error) errors.push(shot.error);
        if (shot.inLibrary !== undefined) inLibrary.push(shot.inLibrary);
        if (shot.onScreen !== undefined) onScreen.push(shot.onScreen);
      }
      return {
        state: errors.length === 0 ? "passed" : "failed",
        asked: shotCount,
        arrived: inLibrary.length,
        inLibrary,
        onScreen,
        ...errors[0],
      };
    },
    burst: async () => {
      const since = now();
      const errors: Failure[] = [];
      setPrompt(t("session.tether.test.bursting", { shots: BURST }));
      for (let i = 0; i < BURST; i += 1) {
        try {
          await ask(Invokes.TetherCapture);
        } catch (e) {
          errors.push(refused(e));
        }
      }
      const fired = BURST - errors.length;
      // Exposures in, and every fired shot ended: in, or reported as never
      // delivered (a failed file of an exposure already in ends nothing).
      let arrived = 0;
      let ended = 0;
      let last = since;
      while (ended < fired) {
        const photo = await next(
          (h) =>
            h.at > last &&
            (appExposure(h) || h.name === "tether-capture-failed"),
          SHOT_WAIT_MS,
          last,
        );
        if (!photo) {
          errors.push(NO_PHOTO);
          break;
        }
        last = photo.at;
        if (photo.name === "tether-capture-failed") {
          errors.push(notDelivered(photo.payload));
          if ((photo.payload as { kind?: string }).kind === "timeout") ended += 1;
        } else {
          ended += 1;
          if (captured(photo).error) errors.push(notAdded(captured(photo).error ?? ""));
          else arrived += 1;
        }
      }
      return {
        state: errors.length === 0 && arrived === BURST ? "passed" : "failed",
        asked: BURST,
        arrived,
        ms: Math.round(last - since),
        ...errors[0],
      };
    },
    bodyShot: async () => {
      const since = now();
      setPrompt(t("session.tether.test.pressShutter"));
      const photo = await next(
        (h) => h.name === "tether-captured" && !captured(h).requested && h.exposure === true,
        BODY_SHOT_WAIT_MS,
        since,
      );
      if (!photo) return { state: "failed", asked: 1, arrived: 0, reason: "no photo within 30 s" };
      const c = captured(photo);
      return c.error
        ? { state: "failed", asked: 1, arrived: 0, ...notAdded(c.error) }
        : { state: "passed", asked: 1, arrived: 1 };
    },
    replug: async () => {
      setPrompt(t("session.tether.test.pullCable"));
      const gone = await next(
        (h) => h.name === "tether-state" && !(h.payload as { connected: boolean }).connected,
        UNPLUG_WAIT_MS,
        now(),
      );
      if (!gone) return { state: "failed", reason: "the cable pull was not noticed within 60 s" };
      setPrompt(t("session.tether.test.plugBack"));
      const back = await next(
        (h) => h.name === "tether-state" && (h.payload as { connected: boolean }).connected,
        REPLUG_WAIT_MS,
        gone.at,
      );
      if (!back) return { state: "failed", reason: "not connected again within 3 minutes" };
      const ms = Math.round(back.at - gone.at);
      setPrompt(t("session.tether.test.shotAfterReplug"));
      const shot = await shoot(false);
      return shot.error
        ? { state: "failed", ms, asked: 1, arrived: 0, ...shot.error }
        : {
            state: "passed",
            ms,
            asked: 1,
            arrived: 1,
            reason: "time from the cable pull noticed to connected again, the replug included",
          };
    },
    release: async () => {
      if (!camera.transferQueue) return { state: "skipped", reason: "the camera keeps no transfer queue" };
      if (!freeCameraCopies) return { state: "skipped", reason: "freeing photos on the camera is turned off" };
      const r = releases.current;
      const waitUntil = now() + RELEASE_WAIT_MS;
      while (r.pending.length > 0 && now() < waitUntil) await new Promise((done) => window.setTimeout(done, 200));
      if (r.freed + r.notFreed + r.errors.length === 0 && r.pending.length === 0)
        return { state: "skipped", reason: "no photo taken in the test yet" };
      const problems = [
        r.pending.length ? `${r.pending.length} not freed` : "",
        r.notFreed ? `${r.notFreed} kept by the camera` : "",
        r.early ? `${r.early} freed before their commit` : "",
        r.errors.length ? `${r.errors.length} releases failed` : "",
      ].filter(Boolean);
      return problems.length
        ? { state: "failed", freed: r.freed, reason: problems.join(", "), detail: r.errors[0] }
        : { state: "passed", freed: r.freed };
    },
  };

  const run = async (id: StepId) => {
    runningStep.current = id;
    setRunning(id);
    setPrompt("");
    const started = now();
    let result: StepResult;
    try {
      result = await runners[id]();
    } catch (e) {
      result = { state: "failed", ...refused(e) };
    }
    if (PHOTO_STEPS.includes(id) && !closed.current) {
      // Every file of the step's photos counts, also the second file of a
      // RAW+JPEG exposure (counted once above), and one a moment late.
      await new Promise((done) => window.setTimeout(done, SETTLE_MS));
      const failed = heard.current.filter((h) => h.at >= started && isFailure(h));
      if (failed.length > 0 && result.state !== "failed") {
        const first = failed[0];
        result = {
          ...result,
          state: "failed",
          reason: `${failed.length} files not added or shots not delivered`,
          detail:
            first.name === "tether-capture-failed" ? notDelivered(first.payload).detail : (captured(first).error ?? undefined),
        };
      }
    }
    if (PHOTO_STEPS.includes(id)) lastPhotoStep.current = id;
    runningStep.current = null;
    setResults((current) => ({ ...current, [id]: result }));
    setPrompt("");
    setRunning(null);
  };

  const skip = (id: StepId) =>
    setResults((current) => ({ ...current, [id]: { state: "skipped", reason: "skipped by the user" } }));

  const info: ReportInfo = {
    appVersion,
    system: systemName(),
    model: camera.model,
    firmware: camera.firmware ?? null,
    library: camera.library ?? null,
    transport: camera.transport ?? "mock",
    transferQueue: camera.transferQueue ?? false,
    freeCameraCopies,
    failures,
  };
  const report = buildReport(info, results);
  const stepNames: Record<StepId, string> = {
    connect: t("session.tether.test.steps.connect"),
    settings: t("session.tether.test.steps.settings"),
    setSetting: t("session.tether.test.steps.setSetting"),
    autofocus: t("session.tether.test.steps.autofocus"),
    shots: t("session.tether.test.steps.shots"),
    burst: t("session.tether.test.steps.burst"),
    bodyShot: t("session.tether.test.steps.bodyShot"),
    replug: t("session.tether.test.steps.replug"),
    release: t("session.tether.test.steps.release"),
  };
  const stepHows: Record<StepId, string> = {
    connect: "",
    settings: t("session.tether.test.how.settings"),
    setSetting: t("session.tether.test.how.setSetting"),
    autofocus: t("session.tether.test.how.autofocus"),
    shots: t("session.tether.test.how.shots"),
    burst: t("session.tether.test.how.burst", { shots: BURST }),
    bodyShot: t("session.tether.test.how.bodyShot"),
    replug: t("session.tether.test.how.replug"),
    release: t("session.tether.test.how.release"),
  };
  const states: Record<StepResult["state"], string> = {
    passed: t("session.tether.test.state.passed"),
    failed: t("session.tether.test.state.failed"),
    skipped: t("session.tether.test.state.skipped"),
  };
  const current = STEPS.find((id) => !results[id]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(report);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };

  return (
    <section className="tether-test" aria-label={t("session.tether.test.title")}>
      <div className="tether-test-head">
        <h3>{t("session.tether.test.title")}</h3>
        <button type="button" className="session-icon-button" aria-label={t("session.tether.test.close")} onClick={onClose}>
          <X size={14} />
        </button>
      </div>
      <p className="tether-note">{t("session.tether.test.intro")}</p>
      <ol className="tether-test-steps">
        {STEPS.map((id) => {
          const result = results[id];
          const active = running === id;
          return (
            <li key={id} data-state={result?.state ?? (active ? "running" : "pending")}>
              <div className="tether-test-step">
                <span className="tether-test-mark" aria-hidden>
                  {active ? (
                    <LoaderCircle size={12} className="animate-spin" />
                  ) : result?.state === "passed" ? (
                    <Check size={12} />
                  ) : result?.state === "failed" ? (
                    <X size={12} />
                  ) : result ? (
                    <Minus size={12} />
                  ) : null}
                </span>
                <strong>{stepNames[id]}</strong>
                {result && <span>{states[result.state]}</span>}
              </div>
              {result && <small>{describe(result)}</small>}
              {result?.detail && <small className="tether-test-detail">{result.detail}</small>}
              {!result && (active || (current === id && running === null)) && (
                <div className="tether-test-run">
                  <small>{active && prompt ? prompt : stepHows[id]}</small>
                  {!active && (
                    <div className="tether-actions">
                      {id === "shots" && (
                        <select
                          aria-label={t("session.tether.test.shotCount")}
                          value={shotCount}
                          onChange={(e) => setShotCount(Number(e.target.value))}
                        >
                          {SHOT_COUNTS.map((n) => (
                            <option key={n} value={n}>
                              {n}
                            </option>
                          ))}
                        </select>
                      )}
                      <button type="button" className="session-action session-primary" onClick={() => void run(id)}>
                        {t("session.tether.test.run")}
                      </button>
                      <button type="button" className="session-action" onClick={() => skip(id)}>
                        {t("session.tether.test.skip")}
                      </button>
                    </div>
                  )}
                </div>
              )}
            </li>
          );
        })}
      </ol>
      <label className="session-field tether-test-report">
        {t("session.tether.test.report")}
        <textarea readOnly rows={10} value={report} aria-label={t("session.tether.test.report")} />
        <small>{t("session.tether.test.reportHint")}</small>
      </label>
      <div className="tether-actions">
        <button type="button" className="session-action session-primary" onClick={() => void copy()}>
          <ClipboardCopy size={14} />
          <span>{copied ? t("session.tether.test.copied") : t("session.tether.test.copy")}</span>
        </button>
        <button type="button" className="session-action" onClick={() => void open(ISSUE_URL)}>
          <ExternalLink size={14} />
          <span>{t("session.tether.test.openIssue")}</span>
        </button>
      </div>
    </section>
  );
}
