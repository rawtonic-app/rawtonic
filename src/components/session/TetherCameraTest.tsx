import { useCallback, useEffect, useMemo, useRef, useState } from "react";
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
  scrub,
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

  // Everything heard during the test, so a wait never misses what came
  // before it was set up.
  const heard = useRef<Heard[]>([]);
  const waiters = useRef(new Set<(h: Heard) => boolean>());
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
      const h = { name, payload, at: now() };
      heard.current.push(h);
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
    const stops = EVENTS.map((name) => listen(name, (event) => hear(name, event.payload)));
    return () => stops.forEach((stop) => void stop.then((fn) => fn()));
  }, []);

  /** The first event since `since` that `match`es, or null after `ms`. */
  const next = useCallback(
    (match: (h: Heard) => boolean, ms: number, since: number) =>
      new Promise<Heard | null>((resolve) => {
        const early = heard.current.find((h) => h.at >= since && match(h));
        if (early) return resolve(early);
        const waiter = (h: Heard) => {
          if (!match(h)) return false;
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

  const secrets = useMemo(() => [camera.serial], [camera.serial]);
  const failed = useCallback(
    (e: unknown): StepResult => ({ state: "failed", reason: scrub(String(e), secrets) }),
    [secrets],
  );

  /** One shot from the app: request → photo in the library → its first
   * Develop frame (when `screen`: Develop shows the captures). */
  const shoot = useCallback(async (screen: boolean) => {
    const since = now();
    try {
      await invoke(Invokes.TetherCapture);
    } catch (e) {
      return { error: String(e) };
    }
    const photo = await next(
      (h) => (h.name === "tether-captured" && captured(h).requested) || h.name === "tether-capture-failed",
      SHOT_WAIT_MS,
      since,
    );
    if (!photo) return { error: "no photo arrived" };
    if (photo.name === "tether-capture-failed") return { error: (photo.payload as { message: string }).message };
    const c = captured(photo);
    if (c.error) return { error: c.error };
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
  }, [next]);

  const runners: Record<StepId, () => Promise<StepResult>> = {
    connect: async () => ({ state: "passed" }),
    settings: async () => {
      const started = now();
      const settings = await invoke<Setting[]>(Invokes.TetherSettings);
      const ms = Math.round(now() - started);
      const changeable = settings.filter((s) => !s.readOnly && s.choices.length > 1).length;
      return settings.length > 0
        ? { state: "passed", ms, settings: settings.length, changeable }
        : { state: "failed", ms, settings: 0, changeable: 0, reason: "no settings read" };
    },
    setSetting: async () => {
      const pick = settingToChange(await invoke<Setting[]>(Invokes.TetherSettings));
      if (!pick) return { state: "skipped", reason: "no changeable setting with two values" };
      const { setting, other } = pick;
      const valueOf = async () =>
        (await invoke<Setting[]>(Invokes.TetherSettings)).find((s) => s.key === setting.key)?.value;
      const started = now();
      await invoke(Invokes.TetherSetSetting, { key: setting.key, value: other });
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
      await invoke(Invokes.TetherAutofocus);
      return { state: "passed", ms: Math.round(now() - started) };
    },
    shots: async () => {
      const inLibrary: number[] = [];
      const onScreen: number[] = [];
      const errors: string[] = [];
      // Develop not showing the first photo: the others are not waited for.
      let screen = true;
      for (let i = 1; i <= shotCount; i += 1) {
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
        reason: errors.length ? scrub(errors[0], secrets) : undefined,
      };
    },
    burst: async () => {
      const since = now();
      const errors: string[] = [];
      setPrompt(t("session.tether.test.bursting", { shots: BURST }));
      for (let i = 0; i < BURST; i += 1) {
        try {
          await invoke(Invokes.TetherCapture);
        } catch (e) {
          errors.push(String(e));
        }
      }
      const fired = BURST - errors.length;
      let arrived = 0;
      let last = since;
      while (arrived < fired) {
        const photo = await next(
          (h) =>
            h.at > last &&
            ((h.name === "tether-captured" && captured(h).requested) || h.name === "tether-capture-failed"),
          SHOT_WAIT_MS,
          last,
        );
        if (!photo) {
          errors.push("no photo arrived");
          break;
        }
        last = photo.at;
        if (photo.name === "tether-capture-failed") errors.push((photo.payload as { message: string }).message);
        else if (captured(photo).error) errors.push(captured(photo).error ?? "");
        else arrived += 1;
        if (arrived + errors.length >= BURST) break;
      }
      return {
        state: errors.length === 0 && arrived === BURST ? "passed" : "failed",
        asked: BURST,
        arrived,
        ms: Math.round(last - since),
        reason: errors.length ? scrub(errors[0], secrets) : undefined,
      };
    },
    bodyShot: async () => {
      const since = now();
      setPrompt(t("session.tether.test.pressShutter"));
      const photo = await next(
        (h) => h.name === "tether-captured" && !captured(h).requested,
        BODY_SHOT_WAIT_MS,
        since,
      );
      if (!photo) return { state: "failed", asked: 1, arrived: 0, reason: "no photo within 30 s" };
      const c = captured(photo);
      return c.error
        ? { state: "failed", asked: 1, arrived: 0, reason: scrub(c.error, secrets) }
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
        ? { state: "failed", ms, asked: 1, arrived: 0, reason: scrub(shot.error, secrets) }
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
        r.errors[0] ? scrub(r.errors[0], secrets) : "",
      ].filter(Boolean);
      return problems.length
        ? { state: "failed", freed: r.freed, reason: problems.join(", ") }
        : { state: "passed", freed: r.freed };
    },
  };

  const run = async (id: StepId) => {
    setRunning(id);
    setPrompt("");
    let result: StepResult;
    try {
      result = await runners[id]();
    } catch (e) {
      result = failed(e);
    }
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
