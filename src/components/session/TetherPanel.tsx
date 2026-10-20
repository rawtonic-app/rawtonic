import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Aperture, Camera, ClipboardCheck, Focus, LoaderCircle, RefreshCcw, Unplug, Video, X } from "lucide-react";
import { toast } from "react-toastify";
import { Trans, useTranslation } from "react-i18next";
import i18n from "i18next";
import { Invokes } from "../ui/AppProperties";
import NamingEditor from "../ui/NamingEditor";
import NextCaptureFields from "./NextCaptureFields";
import {
  DEFAULT_NEXT_CAPTURE,
  flattenPresets,
  nextCaptureOf,
  type NextCaptureForm,
  type PresetChoice,
  type PresetItemJson,
} from "./nextCapture";
import { TETHER_CAPTURE_EVENT, panelHandlesCapture, setTetherConnected } from "./tether";
import TetherCameraTest, { type TestedCamera } from "./TetherCameraTest";
import { compatibilityOf } from "./cameraTest";
import "./tether.css";

/** Mirrors `rawtonic_tether::protocol::CameraInfo`. */
interface CameraInfo {
  model: string;
  port: string;
}

/** Mirrors `camera_tethering::TetherCameras`. */
interface TetherCameras {
  library: string | null;
  cameras: CameraInfo[];
}

/** Mirrors `rawtonic_tether::protocol::CameraStatus`. */
interface CameraStatus {
  model: string;
  port: string;
  battery: string | null;
  captureTarget: string | null;
  canCapture: boolean;
  canPreview: boolean;
  /** The camera keeps each photo until the computer frees it. */
  transferQueue?: boolean;
  serial?: string | null;
  firmware?: string | null;
}

/** Mirrors `camera_tethering::TetherStatus`. */
export interface TetherStatus {
  session: string;
  camera: CameraStatus;
  /** False while the camera is away, waiting to be connected again. */
  connected?: boolean;
  /** libgphoto2's version (null: the mock camera). */
  library?: string | null;
  /** How the helper reaches the camera (imageCaptureCore, usb, mock). */
  transport?: string;
}

/** Mirrors `rawtonic_tether::protocol::Setting`. */
interface CameraSetting {
  key: string;
  cameraKey: string;
  label: string;
  value: string;
  choices: string[];
  readOnly: boolean;
}

/** Mirrors `camera_tethering::TetherCaptured`. */
export interface TetherCaptured {
  cameraName: string;
  destination: string | null;
  assetId: string | null;
  skipped: boolean;
  error: string | null;
  requested: boolean;
  msToEvent: number | null;
  msToDownloaded: number | null;
  msToIngested: number | null;
}

interface TetherError {
  kind: string;
  message: string;
}

const LIVE_VIEW_INTERVAL_MS = 1000 / 30;
/** How often the panel looks for a newly plugged-in camera. */
const CAMERA_POLL_MS = 2000;

/** Whether photos are freed on cameras with a transfer queue (on unless
 * the user turned it off; only ever after the verified library commit). */
const FREE_COPIES_KEY = "rawtonic.tether.freeCameraCopies";
const storedFreeCopies = () => {
  try {
    return localStorage.getItem(FREE_COPIES_KEY) !== "0";
  } catch {
    return true;
  }
};

/** The camera keeps its photos only in its memory, not on its card (its
 * "Save to" names RAM but no card): a photo exists only until it is on
 * this computer. */
export const ramOnly = (settings: { key: string; value: string }[]) => {
  const target = settings.find((s) => s.key === "capturetarget")?.value.toLowerCase() ?? "";
  return target.includes("ram") && !target.includes("card");
};

/** What the compatibility list (tetherCameras.json) says of a camera
 * model. Information for the user only: every camera is handled the same
 * way. */
function Compatibility({ model }: { model: string }) {
  const { t } = useTranslation();
  const entry = compatibilityOf(model);
  const level = !entry
    ? t("session.tether.level.unlisted")
    : entry.level === "tested"
      ? t("session.tether.level.tested", { systems: entry.systems?.join(", ") ?? "", date: entry.tested ?? "" })
      : entry.level === "reported"
        ? t("session.tether.level.reported")
        : t("session.tether.level.notSupported");
  return (
    <p className="tether-note" data-level={entry?.level ?? "unlisted"}>
      {level}
      {entry?.notes && <> {entry.notes}</>}
    </p>
  );
}

/** The panel's naming template and next-capture choice, kept across its
 * unmounting (the connection keeps using them). */
let remembered: { template: string; nextCapture: NextCaptureForm } = {
  template: "{orig}",
  nextCapture: DEFAULT_NEXT_CAPTURE,
};

const sameCameras = (a: CameraInfo[], b: CameraInfo[]) =>
  a.length === b.length && a.every((c, i) => c.model === b[i].model && c.port === b[i].port);

/** The camera tool (Capture One's Camera tool, Lightroom's tethered
 * capture bar): connect a camera on USB, read and set its settings, fire
 * it, see its live view. Photos taken — here or on the camera — are copied
 * into the library's Capture folder with the next-capture settings. */
export default function TetherPanel({
  hidden = false,
  directory,
  jobName,
  status,
  onStatus,
  onClose,
}: {
  hidden?: boolean;
  directory: string;
  jobName: string;
  status: TetherStatus | null;
  onStatus: (status: TetherStatus | null) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [cameras, setCameras] = useState<CameraInfo[] | null>(null);
  const [port, setPort] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  // The camera went away; the backend connects it again when it is back.
  const [waiting, setWaiting] = useState(false);
  const [template, setTemplate] = useState(remembered.template);
  // On unless the user turned it off, as in Capture One.
  const [freeCopies, setFreeCopies] = useState(storedFreeCopies);
  const [nextCapture, setNextCapture] = useState<NextCaptureForm>(remembered.nextCapture);
  const [presets, setPresets] = useState<PresetChoice[]>([]);
  const [settings, setSettings] = useState<CameraSetting[]>([]);
  const [liveView, setLiveView] = useState(false);
  const [frame, setFrame] = useState<string | null>(null);
  const [last, setLast] = useState<TetherCaptured | null>(null);
  const [capturing, setCapturing] = useState(0);
  // The camera test, with the camera it tests (it outlives a cable pull).
  const [testing, setTesting] = useState<TestedCamera | null>(null);
  const [connectMs, setConnectMs] = useState<number | null>(null);
  const connected = status !== null;

  const findCameras = useCallback(async () => {
    setError("");
    setCameras(null);
    try {
      const found = await invoke<TetherCameras>(Invokes.TetherCameras);
      setCameras(found.cameras);
      setPort((current) => (found.cameras.some((c) => c.port === current) ? current : (found.cameras[0]?.port ?? "")));
    } catch (e) {
      setCameras([]);
      setError(String(e));
    }
  }, []);

  // A camera plugged in while the panel is open shows up by itself.
  useEffect(() => {
    if (connected || hidden || waiting) return;
    const timer = window.setInterval(() => {
      void invoke<TetherCameras>(Invokes.TetherCameras)
        .then((found) => {
          setCameras((current) =>
            current !== null && sameCameras(current, found.cameras) ? current : found.cameras,
          );
          setPort((current) =>
            found.cameras.some((c) => c.port === current) ? current : (found.cameras[0]?.port ?? ""),
          );
        })
        .catch(() => undefined);
    }, CAMERA_POLL_MS);
    return () => window.clearInterval(timer);
  }, [connected, hidden, waiting]);

  const readSettings = useCallback(async () => {
    try {
      setSettings(await invoke<CameraSetting[]>(Invokes.TetherSettings));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void invoke<PresetItemJson[]>(Invokes.LoadPresets)
      .then((items) => setPresets(flattenPresets(items ?? [])))
      .catch(() => setPresets([]));
    if (!connected) void findCameras();
    else void readSettings();
    // Only when the panel opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The connection outlives the panel (closed, or the view hidden in
  // fullscreen): unmounting it does not say "disconnected".
  useEffect(() => {
    setTetherConnected(connected);
  }, [connected]);
  // What the connection was given, for the panel shown again (it must not
  // send defaults over what the camera's photos are ingested with).
  useEffect(() => {
    remembered = { template, nextCapture };
  }, [template, nextCapture]);

  // What the camera and the helper report.
  useEffect(() => {
    const stops = [
      // Failures are said app-wide (useTauriListeners), also with no
      // panel mounted; here only the panel's own state follows.
      listen<TetherCaptured>("tether-captured", (event) => {
        setLast(event.payload);
        setCapturing((n) => Math.max(0, n - 1));
      }),
      listen<TetherError>("tether-capture-failed", () => {
        setCapturing((n) => Math.max(0, n - 1));
      }),
      listen("tether-settings-changed", () => void readSettings()),
      listen<{ connected: boolean; error?: TetherError; status?: TetherStatus }>("tether-state", (event) => {
        if (event.payload.connected) {
          // Connected again after the camera came back.
          setError("");
          setWaiting(false);
          void readSettings();
          return;
        }
        onStatus(null);
        setWaiting(event.payload.error?.kind === "disconnected");
        setLiveView(false);
        setCapturing(0);
        if (event.payload.error) setError(event.payload.error.message);
      }),
      listen<TetherError>("tether-live-view-stopped", (event) => {
        setLiveView(false);
        setError(event.payload.message);
      }),
    ];
    return () => stops.forEach((stop) => void stop.then((fn) => fn()));
  }, [onStatus, readSettings]);

  // Live view: the newest camera frame, pulled at most 30 times a second.
  const frameUrl = useRef<string | null>(null);
  useEffect(() => {
    if (!liveView) return;
    let stopped = false;
    let timer = 0;
    const pull = async () => {
      if (stopped) return;
      try {
        const bytes = await invoke<ArrayBuffer>(Invokes.TetherLiveViewFrame);
        if (!stopped && bytes.byteLength > 0) {
          const url = URL.createObjectURL(new Blob([bytes], { type: "image/jpeg" }));
          if (frameUrl.current) URL.revokeObjectURL(frameUrl.current);
          frameUrl.current = url;
          setFrame(url);
        }
      } catch {
        // The next pull tries again; a stop is reported by the backend.
      }
      if (!stopped) timer = window.setTimeout(() => void pull(), LIVE_VIEW_INTERVAL_MS);
    };
    void pull();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [liveView]);
  useEffect(
    () => () => {
      if (frameUrl.current) URL.revokeObjectURL(frameUrl.current);
    },
    [],
  );

  const connect = async () => {
    setBusy(true);
    setError("");
    setWaiting(false);
    const started = performance.now();
    try {
      const status = await invoke<TetherStatus>(Invokes.TetherConnect, {
        directory,
        port: port || null,
        template,
        ...nextCaptureOf(nextCapture, presets),
        freeCameraCopies: freeCopies,
      });
      setConnectMs(Math.round(performance.now() - started));
      onStatus(status);
      await readSettings();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    setLiveView(false);
    setFrame(null);
    try {
      await invoke(Invokes.TetherDisconnect);
    } finally {
      onStatus(null);
      void findCameras();
    }
  };

  const hiddenRef = useRef(hidden);
  hiddenRef.current = hidden;
  const capture = useCallback(async () => {
    setError("");
    setCapturing((n) => n + 1);
    try {
      await invoke(Invokes.TetherCapture);
    } catch (e) {
      setCapturing((n) => Math.max(0, n - 1));
      setError(String(e));
      // The panel may be closed (it still handles the shortcut).
      if (hiddenRef.current) toast.error(i18n.t("session.tether.captureFailed", { error: String(e) }));
    }
  }, []);

  useEffect(() => {
    if (!connected) return;
    const fire = () => void capture();
    window.addEventListener(TETHER_CAPTURE_EVENT, fire);
    const handled = panelHandlesCapture();
    return () => {
      window.removeEventListener(TETHER_CAPTURE_EVENT, fire);
      handled();
    };
  }, [connected, capture]);

  const focus = async () => {
    setError("");
    try {
      await invoke(Invokes.TetherAutofocus);
    } catch (e) {
      setError(String(e));
    }
  };

  const liveViewStarting = useRef(false);
  const toggleLiveView = async () => {
    setError("");
    // A start still on its way counts as on for the unmount below.
    if (!liveView) liveViewStarting.current = true;
    try {
      await invoke(Invokes.TetherLiveView, { on: !liveView });
      setLiveView(!liveView);
      if (liveView) setFrame(null);
    } catch (e) {
      setError(String(e));
    } finally {
      liveViewStarting.current = false;
    }
  };

  // Live view is shown only while the panel is open.
  useEffect(() => {
    if (!hidden || !liveView) return;
    setLiveView(false);
    setFrame(null);
    void invoke(Invokes.TetherLiveView, { on: false }).catch(() => undefined);
  }, [hidden, liveView]);
  // Nor after the panel is gone (the view hidden in fullscreen), also when
  // its start is still on its way (the helper answers in order: off after
  // on).
  const liveViewOn = useRef(false);
  useEffect(() => {
    liveViewOn.current = liveView;
  }, [liveView]);
  useEffect(
    () => () => {
      if (liveViewOn.current || liveViewStarting.current)
        void invoke(Invokes.TetherLiveView, { on: false }).catch(() => undefined);
    },
    [],
  );

  const setSetting = async (key: string, value: string) => {
    setError("");
    try {
      await invoke(Invokes.TetherSetSetting, { key, value });
    } catch (e) {
      setError(String(e));
    }
    await readSettings();
  };

  const changeNextCapture = async (form: NextCaptureForm) => {
    setNextCapture(form);
    if (!connected) return;
    try {
      await invoke(Invokes.TetherNextCapture, {
        template,
        ...nextCaptureOf(form, presets),
        freeCameraCopies: freeCopies,
      });
    } catch (e) {
      setError(String(e));
    }
  };

  const changeFreeCopies = async (on: boolean) => {
    setFreeCopies(on);
    try {
      localStorage.setItem(FREE_COPIES_KEY, on ? "1" : "0");
    } catch {
      // Not kept for the next time; this connection still uses it.
    }
    if (!connected) return;
    try {
      await invoke(Invokes.TetherNextCapture, {
        template,
        ...nextCaptureOf(nextCapture, presets),
        freeCameraCopies: on,
      });
    } catch (e) {
      setError(String(e));
    }
  };

  const selected = cameras?.find((c) => c.port === port);

  return (
    <aside className="tether-panel" hidden={hidden} aria-label={t("session.tether.title")}>
      <header className="tether-header">
        <Camera size={15} />
        <h2>{t("session.tether.title")}</h2>
        <button type="button" className="session-icon-button" aria-label={t("session.dialogs.close")} onClick={onClose}>
          <X size={15} />
        </button>
      </header>
      {testing ? (
        <div className="tether-body">
          <TetherCameraTest
            camera={testing}
            connectMs={connectMs}
            freeCameraCopies={freeCopies}
            onClose={() => setTesting(null)}
          />
        </div>
      ) : !connected ? (
        <div className="tether-body">
          <label className="session-field">
            {t("session.tether.camera")}
            <div className="session-backup">
              <select
                aria-label={t("session.tether.camera")}
                value={port}
                disabled={!cameras || cameras.length === 0}
                onChange={(e) => setPort(e.target.value)}
              >
                {cameras === null && <option value="">{t("session.tether.looking")}</option>}
                {cameras?.length === 0 && <option value="">{t("session.tether.noCamera")}</option>}
                {cameras?.map((c) => (
                  <option key={c.port} value={c.port}>
                    {c.model}
                  </option>
                ))}
              </select>
              <button
                type="button"
                className="session-icon-button"
                aria-label={t("session.tether.refresh")}
                onClick={() => void findCameras()}
              >
                <RefreshCcw size={15} />
              </button>
            </div>
            <small>{t("session.tether.usbHint")}</small>
          </label>
          {selected && <Compatibility model={selected.model} />}
          <NamingEditor mode="ingest" value={template} onChange={setTemplate}
            label={t("session.fields.filenameTemplate")} job={jobName} />
          <NextCaptureFields value={nextCapture} onChange={(form) => void changeNextCapture(form)} presets={presets} />
          <p className="tether-note">
            <Trans
              i18nKey="session.tether.copyNote"
              values={{ target: `${jobName} / Capture` }}
              components={{ strong: <strong /> }}
            />
          </p>
          {error && (
            <p className="session-error" role="alert">
              {error}
            </p>
          )}
          {waiting && <p className="tether-note">{t("session.tether.reconnecting")}</p>}
          <button
            type="button"
            className="session-action session-primary tether-wide"
            disabled={busy || !cameras?.length}
            onClick={() => void connect()}
          >
            {busy ? <LoaderCircle size={14} className="animate-spin" /> : <Camera size={14} />}
            <span>{busy ? t("session.tether.connecting") : t("session.tether.connect")}</span>
          </button>
        </div>
      ) : (
        <div className="tether-body">
          <div className="tether-camera">
            <strong>{status.camera.model}</strong>
            {status.camera.battery && (
              <span>{t("session.tether.battery", { level: status.camera.battery })}</span>
            )}
          </div>
          <Compatibility model={status.camera.model} />
          <div className="tether-actions">
            <button
              type="button"
              className="session-action session-primary tether-capture"
              disabled={!status.camera.canCapture}
              title={t("session.tether.captureTitle")}
              onClick={() => void capture()}
            >
              {capturing > 0 ? <LoaderCircle size={15} className="animate-spin" /> : <Aperture size={15} />}
              <span>{t("session.tether.capture")}</span>
            </button>
            <button type="button" className="session-action" onClick={() => void focus()}>
              <Focus size={14} />
              <span>{t("session.tether.focus")}</span>
            </button>
            <button
              type="button"
              className={`session-action${liveView ? " is-active" : ""}`}
              aria-pressed={liveView}
              disabled={!status.camera.canPreview}
              onClick={() => void toggleLiveView()}
            >
              <Video size={14} />
              <span>{t("session.tether.liveView")}</span>
            </button>
          </div>
          {liveView && (
            <div className="tether-live-view">
              {frame ? <img src={frame} alt={t("session.tether.liveView")} /> : <LoaderCircle className="animate-spin" />}
            </div>
          )}
          {last && (
            <p className="tether-last" role="status">
              {last.error
                ? t("session.tether.lastFailed", { file: last.cameraName })
                : last.requested && last.msToIngested !== null
                  ? t("session.tether.lastTiming", { file: last.cameraName, ms: last.msToIngested })
                  : t("session.tether.lastIn", { file: last.cameraName })}
            </p>
          )}
          {settings.length > 0 && (
            <div className="tether-settings">
              {settings.map((s) => (
                <label key={s.key}>
                  <span>{s.label}</span>
                  {s.readOnly || s.choices.length === 0 ? (
                    <output title={s.readOnly ? t("session.tether.setOnBody") : undefined}>{s.value}</output>
                  ) : (
                    <select
                      aria-label={s.label}
                      value={s.value}
                      onChange={(e) => void setSetting(s.key, e.target.value)}
                    >
                      {!s.choices.includes(s.value) && <option value={s.value}>{s.value}</option>}
                      {s.choices.map((c) => (
                        <option key={c} value={c}>
                          {c}
                        </option>
                      ))}
                    </select>
                  )}
                </label>
              ))}
            </div>
          )}
          {ramOnly(settings) && <p className="tether-note">{t("session.tether.ramOnly")}</p>}
          {status.camera.transferQueue && (
            <label className="tether-free-copies">
              <input
                type="checkbox"
                checked={freeCopies}
                onChange={(e) => void changeFreeCopies(e.target.checked)}
              />
              <span>{t("session.tether.freeCopies")}</span>
              <small>{t("session.tether.freeCopiesHint")}</small>
            </label>
          )}
          <details className="tether-next">
            <summary>{t("session.tether.nextCapture")}</summary>
            <NextCaptureFields value={nextCapture} onChange={(form) => void changeNextCapture(form)} presets={presets} />
          </details>
          {error && (
            <p className="session-error" role="alert">
              {error}
            </p>
          )}
          <button
            type="button"
            className="session-action tether-wide"
            title={t("session.tether.test.buttonTitle")}
            onClick={() =>
              setTesting({ ...status.camera, library: status.library ?? null, transport: status.transport ?? "mock" })
            }
          >
            <ClipboardCheck size={14} />
            <span>{t("session.tether.test.button")}</span>
          </button>
          <button type="button" className="session-action tether-wide" onClick={() => void disconnect()}>
            <Unplug size={14} />
            <span>{t("session.tether.disconnect")}</span>
          </button>
        </div>
      )}
    </aside>
  );
}
