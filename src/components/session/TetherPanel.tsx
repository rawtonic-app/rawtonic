import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Aperture, Camera, Focus, LoaderCircle, RefreshCcw, Unplug, Video, X } from "lucide-react";
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
import { TETHER_CAPTURE_EVENT, setTetherConnected } from "./tether";
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
}

/** Mirrors `camera_tethering::TetherStatus`. */
export interface TetherStatus {
  session: string;
  camera: CameraStatus;
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
  const [template, setTemplate] = useState("{orig}");
  const [nextCapture, setNextCapture] = useState<NextCaptureForm>(DEFAULT_NEXT_CAPTURE);
  const [presets, setPresets] = useState<PresetChoice[]>([]);
  const [settings, setSettings] = useState<CameraSetting[]>([]);
  const [liveView, setLiveView] = useState(false);
  const [frame, setFrame] = useState<string | null>(null);
  const [last, setLast] = useState<TetherCaptured | null>(null);
  const [capturing, setCapturing] = useState(0);
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

  useEffect(() => {
    setTetherConnected(connected);
    return () => setTetherConnected(false);
  }, [connected]);

  // What the camera and the helper report.
  useEffect(() => {
    const stops = [
      listen<TetherCaptured>("tether-captured", (event) => {
        setLast(event.payload);
        setCapturing((n) => Math.max(0, n - 1));
        const { error: failed, cameraName } = event.payload;
        if (failed) toast.error(i18n.t("session.tether.importFailed", { file: cameraName, error: failed }));
      }),
      listen<TetherError>("tether-capture-failed", (event) => {
        setCapturing((n) => Math.max(0, n - 1));
        toast.error(i18n.t("session.tether.captureFailed", { error: event.payload.message }));
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
      listen<string>("tether-incomplete", (event) =>
        toast.warning(i18n.t("session.tether.incomplete", { path: event.payload }), { autoClose: false }),
      ),
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
    try {
      const status = await invoke<TetherStatus>(Invokes.TetherConnect, {
        directory,
        port: port || null,
        template,
        ...nextCaptureOf(nextCapture, presets),
      });
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

  const capture = useCallback(async () => {
    setError("");
    setCapturing((n) => n + 1);
    try {
      await invoke(Invokes.TetherCapture);
    } catch (e) {
      setCapturing((n) => Math.max(0, n - 1));
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    if (!connected) return;
    const fire = () => void capture();
    window.addEventListener(TETHER_CAPTURE_EVENT, fire);
    return () => window.removeEventListener(TETHER_CAPTURE_EVENT, fire);
  }, [connected, capture]);

  const focus = async () => {
    setError("");
    try {
      await invoke(Invokes.TetherAutofocus);
    } catch (e) {
      setError(String(e));
    }
  };

  const toggleLiveView = async () => {
    setError("");
    try {
      await invoke(Invokes.TetherLiveView, { on: !liveView });
      setLiveView(!liveView);
      if (liveView) setFrame(null);
    } catch (e) {
      setError(String(e));
    }
  };

  // Live view is shown only while the panel is open.
  useEffect(() => {
    if (!hidden || !liveView) return;
    setLiveView(false);
    setFrame(null);
    void invoke(Invokes.TetherLiveView, { on: false }).catch(() => undefined);
  }, [hidden, liveView]);

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
      await invoke(Invokes.TetherNextCapture, { template, ...nextCaptureOf(form, presets) });
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <aside className="tether-panel" hidden={hidden} aria-label={t("session.tether.title")}>
      <header className="tether-header">
        <Camera size={15} />
        <h2>{t("session.tether.title")}</h2>
        <button type="button" className="session-icon-button" aria-label={t("session.dialogs.close")} onClick={onClose}>
          <X size={15} />
        </button>
      </header>
      {!connected ? (
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
          <details className="tether-next">
            <summary>{t("session.tether.nextCapture")}</summary>
            <NextCaptureFields value={nextCapture} onChange={(form) => void changeNextCapture(form)} presets={presets} />
          </details>
          {error && (
            <p className="session-error" role="alert">
              {error}
            </p>
          )}
          <button type="button" className="session-action tether-wide" onClick={() => void disconnect()}>
            <Unplug size={14} />
            <span>{t("session.tether.disconnect")}</span>
          </button>
        </div>
      )}
    </aside>
  );
}
