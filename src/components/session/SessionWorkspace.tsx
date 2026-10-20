import { validNamingTemplate, validSequenceStart, type NamePreview } from "../../utils/namingTemplates";
import NamingEditor from "../ui/NamingEditor";
import LibraryTransferDialog, { type TransferPurpose } from "./LibraryTransferDialog";
import FolderSyncDialog from "./FolderSyncDialog";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { join } from "@tauri-apps/api/path";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowDownToLine,
  ArrowRight,
  Camera,
  Check,
  ChevronDown,
  Database,
  FolderOpen,
  FolderPlus,
  FolderSync,
  FolderPlus as FolderAdd,
  HardDrive,
  Import,
  RefreshCcw,
  LoaderCircle,
  X,
} from "lucide-react";
import { toast } from "react-toastify";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { Invokes } from "../ui/AppProperties";
import RawtonicMark from "../ui/RawtonicMark";
import type { FolderTree } from "../../utils/folderTree";
import NextCaptureFields from "./NextCaptureFields";
import TetherPanel, { type TetherStatus } from "./TetherPanel";
import {
  DEFAULT_NEXT_CAPTURE,
  flattenPresets,
  nextCaptureOf,
  type NextCapture,
  type NextCaptureForm,
  type NextCaptureMetadata,
  type PresetChoice,
  type PresetItemJson,
} from "./nextCapture";
import { useContextMenu } from "../../context/ContextMenuContext";
import { useSessionBar } from "./sessionBar";
import { SESSION_ALBUMS_CHANGED } from "../studio/StudioAlbums";
import { OFFLINE_CHANGED, OFFLINE_COUNTED, watchVolumes } from "../../utils/offline";
import { hotFolderCommand } from "../../utils/hotFolderCommands";
import { FOLDER_SYNC_APPLIED, IMPORTS_CHANGED, OPEN_FOLDER_SYNC_EVENT, type OpenFolderSyncDetail } from "../../utils/folderSync";
import IngestPreviewGrid, { fileName, type IngestCandidate } from "./IngestPreviewGrid";
import {
  OPEN_INGEST_EVENT,
  folderAcceptsDrop,
  useWindowFileDrop,
  volumeOf,
  type FileDrop,
  type OpenIngestDetail,
} from "./fileDrop";
import { throttleAsync } from "../../utils/throttle";
import { preselectIngestMode, type IngestMode } from "./ingestMode";
import { Trans, useTranslation } from "react-i18next";
import i18n from "i18next";
import "./session.css";

export interface SessionSummary {
  directory: string;
  manifest: {
    name: string;
    mode: "session" | "catalog";
    capture: string;
    output: string;
  };
  assets: number;
  picked: number;
  rated: number;
  xmp_pending: number;
  /** Photographs whose file is missing or on a disconnected drive. */
  missing?: number;
}
interface Progress {
  current: number;
  total: number;
  mbps: number;
}
interface ImportResult {
  source: string;
  error: string | null;
  skipped: boolean;
  destination: string | null;
  /** The card sidecar copied beside the copy (and adopted). */
  sidecar?: string | null;
  /** A problem that did not keep the photograph out (its sidecar). */
  warning?: string | null;
}
/** Mirrors `session_bridge::IngestScan`. */
interface IngestScan {
  files: IngestCandidate[];
  truncated: boolean;
}
/** Mirrors `session_bridge::IngestedFile` (`session-ingest-imported`). */
export interface IngestedFile {
  index: number;
  total: number;
  source: string;
  destination: string | null;
  assetId: string | null;
  skipped: boolean;
  error: string | null;
}
/** Where photographs dropped onto the window go: a folder of the library
 * instead of its capture folder, and an album to add them to afterwards. */
interface IngestTarget {
  folder: string | null;
  album: { id: string; name: string } | null;
}
const NO_TARGET: IngestTarget = { folder: null, album: null };
/** The ingest dialog's file dialog filter when the app's list of formats
 * has not loaded (it normally comes from `get_supported_file_types`). */
const FALLBACK_EXTENSIONS = ["arw", "dng", "cr3", "cr2", "nef", "raf", "rw2", "orf", "jpg", "jpeg", "png", "tif", "tiff", "heic", "heif", "webp", "avif", "jxl"];
export interface HotFolderStatus {
  folder: string;
  session: string;
  /** Photographs this hot folder brought in since it was started (kept
   * by the library, so the count survives a restart). */
  imported: number;
  nextCapture: NextCapture;
  metadata?: NextCaptureMetadata;
  includeSubfolders?: boolean;
  /** The watched folder cannot be read now (say its drive is unplugged). */
  missing?: boolean;
}

/** What the hot folder did with one file (`hot-folder-imported`). */
interface HotFolderImported {
  source: string;
  skipped: boolean;
  error: string | null;
}

/** The hot folder after it handled `file`: its count of photographs taken
 * goes on from the count the library keeps (`imported` survives restarts:
 * the photographs this hot folder brought in since it was started); a
 * verified duplicate or a failure is not counted. */
export function countHotFolderCapture(
  status: HotFolderStatus | null,
  file: HotFolderImported,
): HotFolderStatus | null {
  if (!status || file.error || file.skipped) return status;
  return { ...status, imported: status.imported + 1 };
}

/** Mirrors `library_import::ImportPreview`. */
interface ImportPreview {
  name: string;
  kind: "lightroom" | "captureOne";
  photos: number;
  copies: number;
  edited: number;
  albums: number;
  roots: { folder: string; photos: number; found: number }[];
  notRead: { smartAlbums: number; localAdjustments: number; unlocated: number };
}

interface ImportReport {
  imported: number;
  withSettings: number;
  missing: number;
  copiesSkipped: number;
  albums: number;
  errors: string[];
}

interface Props {
  onNavigate: (path: string | null, newRoot?: boolean) => Promise<void>;
  nativeTitlebar?: boolean;
  windowControls?: ReactNode;
}

/** How long a session may take to open before the app says what it waits for. */
export const SLOW_RESTORE_MS = 3000;

/** Reopens the last session. Opening a session on another volume can wait on
 * macOS's permission dialog for that volume; after a few seconds the app says
 * which session it is opening instead of showing nothing. */
export function restoreLastSession(
  navigate: (summary: SessionSummary) => Promise<void>,
  setError: (message: string) => void,
) {
  const notice = "session-restore";
  const slow = window.setTimeout(() => {
    void invoke<string | null>("session_last_directory")
      .then((directory) => {
        if (!directory) return;
        toast.info(
          i18n.t("session.restore.opening", { name: directory.split(/[\\/]/).pop(), directory }),
          { toastId: notice, autoClose: false },
        );
      })
      .catch(() => {});
  }, SLOW_RESTORE_MS);
  const done = () => {
    window.clearTimeout(slow);
    toast.dismiss(notice);
  };
  return invoke<SessionSummary | null>("session_last")
    .then((last) => {
      done();
      if (last) void navigate(last).catch((e) => setError(String(e)));
    })
    .catch((e) => {
      done();
      toast.error(i18n.t("session.restore.failed", { error: String(e) }), { autoClose: false });
    });
}

/** Whether the open library is another one than the library a camera was
 * connected for. Not while no library is known yet (restoring, or this
 * view remounted): that is not leaving it. */
export function leavesTetheredLibrary(tetheredFor: string | null, openLibrary: string | null) {
  if (!tetheredFor || !openLibrary) return false;
  return tetheredFor.replace(/\/$/, "") !== openLibrary.replace(/\/$/, "");
}

export default function SessionWorkspace({ onNavigate, nativeTitlebar = false, windowControls }: Props) {
  const { t } = useTranslation();
  const [session, setSession] = useState<SessionSummary | null>(null);
  const [form, setForm] = useState<"create" | "ingest" | "hotfolder" | "import" | null>(null);
  const [importSource, setImportSource] = useState("");
  const [importPreview, setImportPreview] = useState<ImportPreview | null>(null);
  const [relinks, setRelinks] = useState<Record<string, string>>({});
  const [hotFolder, setHotFolder] = useState<HotFolderStatus | null>(null);
  const [hotPath, setHotPath] = useState("");
  const [nextCaptureForm, setNextCaptureForm] = useState<NextCaptureForm>(DEFAULT_NEXT_CAPTURE);
  const [tetherOpen, setTetherOpen] = useState(false);
  const [tether, setTether] = useState<TetherStatus | null>(null);
  const [includeSubfolders, setIncludeSubfolders] = useState(false);
  // The library whose kept hot folder was asked to resume.
  const resumedFor = useRef<string | null>(null);
  const [presets, setPresets] = useState<PresetChoice[]>([]);
  const [name, setName] = useState("");
  const [parent, setParent] = useState("");
  const [catalog, setCatalog] = useState(false);
  const [transfer, setTransfer] = useState<TransferPurpose | null>(null);
  /** Synchronize Folder's review dialog, for a folder (null: the whole library). */
  const [syncing, setSyncing] = useState<{ folder: string | null } | null>(null);
  // Coming from Lightroom or Capture One: a new catalog first, then the import.
  const [importAfterCreate, setImportAfterCreate] = useState(false);
  const [sources, setSources] = useState<IngestCandidate[]>([]);
  const [chosen, setChosen] = useState<Set<string>>(new Set());
  const [scanning, setScanning] = useState(false);
  const [target, setTarget] = useState<IngestTarget>(NO_TARGET);
  const [addToAlbum, setAddToAlbum] = useState(true);
  // Copy or add where they are (catalogs), preselected by the sources.
  const [mode, setMode] = useState<IngestMode>("copy");
  const [importMode, setImportMode] = useState<IngestMode>("copy");
  const [fileDrag, setFileDrag] = useState(false);
  // Cancel pressed for the running ingest (also before it reached the backend).
  const cancelRequested = useRef(false);
  // The ingest dialog's list as scans add to it (`addSources`).
  const listed = useRef<IngestCandidate[]>([]);
  const scanList = useRef(0);
  const scansRunning = useRef(0);
  const [backup, setBackup] = useState<string | null>(null);
  const [template, setTemplate] = useState("{orig}");
  const [sequenceStart, setSequenceStart] = useState(1);
  // The ingest dialog's example name made by the backend from the first
  // chosen photograph's own EXIF and card sidecar, for the template,
  // counter and photograph it was made for.
  const [namePreview, setNamePreview] = useState<
    { key: string; result: NamePreview } | null
  >(null);
  const previewSource =
    form === "ingest" ? (sources.find((file) => chosen.has(file.path))?.path ?? null) : null;
  const previewKey = `${previewSource}\n${template}\n${sequenceStart}\n${session?.manifest.name ?? ""}`;
  useEffect(() => {
    if (!previewSource || !isTauri() || !validNamingTemplate(template, "ingest") || !validSequenceStart(sequenceStart))
      return;
    let current = true;
    const timer = window.setTimeout(() => {
      invoke<NamePreview>("session_naming_preview", {
        source: previewSource,
        template,
        job: session?.manifest.name ?? "",
        sequence: sequenceStart,
        sequenceStart,
      })
        .then((result) => current && setNamePreview({ key: previewKey, result }))
        .catch(() => current && setNamePreview(null));
    }, 150);
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [previewKey, previewSource, template, sequenceStart, session?.manifest.name]);
  const [busy, setBusy] = useState(false);
  const [importing, setImporting] = useState(false);
  const [progress, setProgress] = useState<Progress | null>(null);
  const [error, setError] = useState("");
  const dialog = useRef<HTMLDialogElement>(null);
  const restored = useRef(false);
  const flagWriteQueue = useRef(Promise.resolve());
  const summaryRequest = useRef(0);
  const imageList = useLibraryStore((s) => s.imageList);
  const roots = useLibraryStore((s) => s.rootPaths);

  const navigate = useCallback(
    async (summary: SessionSummary) => {
      summaryRequest.current++;
      const root = summary.directory;
      const [tree, flags] = await Promise.all([
        invoke<FolderTree>(Invokes.GetFolderTree, {
          path: root,
          expandedFolders: [root],
          showImageCounts: true,
        }),
        invoke<Record<string, number>>("session_flags", { directory: root }),
      ]);
      setSession(summary);
      useLibraryStore.getState().setLibrary({
        sessionDirectory: root,
        imageFlags: flags,
        rootPaths: [root],
        folderTrees: [tree],
        expandedFolders: new Set([root]),
      });
      const settings = useSettingsStore.getState();
      if (settings.appSettings)
        settings.handleSettingsChange({
          ...settings.appSettings,
          rootFolders: [root],
        });
      await onNavigate(await join(root, summary.manifest.capture), true);
    },
    [onNavigate],
  );

  useEffect(() => {
    if (!isTauri() || restored.current) return;
    // Startup settings initialize the existing editor independently. Delay restore
    // until they are loaded, rather than racing two different folder navigations.
    const unsubscribe = useSettingsStore.subscribe((state) => {
      if (!state.appSettings) return;
      unsubscribe();
      if (restored.current) return;
      restored.current = true;
      restoreLastSession(navigate, setError);
    });
    if (useSettingsStore.getState().appSettings) {
      unsubscribe();
      if (restored.current) return;
      restored.current = true;
      restoreLastSession(navigate, setError);
    }
    return unsubscribe;
  }, [navigate]);

  useLayoutEffect(() => {
    if (form) {
      dialog.current?.showModal();
      dialog.current
        ?.querySelector<HTMLElement>('form input:not([type="radio"]), form button')
        ?.focus();
      setError("");
    } else dialog.current?.close();
  }, [form]);

  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen<Progress>("session-ingest-progress", (event) =>
      setProgress(event.payload),
    ).then((fn) => {
      if (disposed) fn();
      else stop = fn;
    });
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  // A camera that came back after a cable pull is connected again by the
  // backend, also while the tether panel is closed.
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen<{ connected: boolean; status?: TetherStatus }>("tether-state", (event) => {
      if (event.payload.connected && event.payload.status) setTether(event.payload.status);
    }).then((fn) => {
      if (disposed) fn();
      else stop = fn;
    });
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  // The photograph count follows an ingest as each photograph arrives.
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    const recount = throttleAsync(() => refreshRef.current(), 500);
    void listen<IngestedFile>("session-ingest-imported", (event) => {
      if (!event.payload.error && !event.payload.skipped) recount();
    }).then((fn) => {
      if (disposed) fn();
      else stop = fn;
    });
    return () => {
      disposed = true;
      recount.cancel();
      stop?.();
    };
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen<HotFolderImported>(
      "hot-folder-imported",
      (event) => {
        const { error, skipped, source } = event.payload;
        setHotFolder((current) => countHotFolderCapture(current, event.payload));
        if (error)
          toast.error(i18n.t("session.toasts.hotFolderFailed", { file: source.split(/[\\/]/).pop(), error }));
        else if (!skipped) void refreshRef.current();
      },
    ).then((fn) => {
      if (disposed) fn();
      else stop = fn;
    });
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  // The watched folder went away (an unplugged drive) or came back.
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen<{ missing: boolean }>("hot-folder-state", (event) => {
      setHotFolder((current) => current && { ...current, missing: event.payload.missing });
    }).then((fn) => {
      if (disposed) fn();
      else stop = fn;
    });
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  // A hot folder feeds the session it was started for; another library
  // pauses it (the library keeps it and resumes it when opened again).
  useEffect(() => {
    if (hotFolder && session && !hotFolder.session.startsWith(session.directory.replace(/\/$/, ""))) {
      void hotFolderCommand("hot_folder_suspend", { directory: hotFolder.session }).catch(() => {});
      setHotFolder(null);
    }
  }, [session, hotFolder]);

  // The library a camera was connected for, kept while the camera is away
  // (unplugged, waiting to be connected again): leaving that library also
  // cancels the pending reconnect.
  const tetheredFor = useRef<string | null>(null);
  // A tethered camera feeds the library it was connected for: another
  // library disconnects it (its photos must not land in the wrong one).
  useEffect(() => {
    if (!isTauri()) return;
    // A camera still connected (the window was reloaded).
    void invoke<TetherStatus | null>(Invokes.TetherStatus)
      .then((status) => {
        if (!status?.camera || typeof status.session !== "string") return;
        // Also a camera that is away: leaving its library cancels its
        // reconnect — at once if another library is already open.
        tetheredFor.current = status.session.replace(/\/$/, "");
        if (leavesTetheredLibrary(tetheredFor.current, useLibraryStore.getState().sessionDirectory)) {
          tetheredFor.current = null;
          void invoke(Invokes.TetherDisconnect).catch(() => {});
          return;
        }
        if (status.connected !== false) setTether(status);
      })
      .catch(() => {});
  }, []);
  useEffect(() => {
    if (tether) tetheredFor.current = tether.session.replace(/\/$/, "");
  }, [tether]);
  // The open library as the store knows it: set when a library opens, kept
  // while this view is remounted (fullscreen) or still restoring it.
  const openLibrary = useLibraryStore((s) => s.sessionDirectory);
  useEffect(() => {
    if (!leavesTetheredLibrary(tetheredFor.current, openLibrary)) return;
    tetheredFor.current = null;
    void invoke(Invokes.TetherDisconnect).catch(() => {});
    setTether(null);
    setTetherOpen(false);
  }, [openLibrary, tether]);

  // Opening a library (also at start-up) resumes the hot folder it keeps.
  const sessionDirectory = session?.directory ?? null;
  const openDirectory = useRef(sessionDirectory);
  openDirectory.current = sessionDirectory;
  useEffect(() => {
    if (!sessionDirectory || resumedFor.current === sessionDirectory) return;
    resumedFor.current = sessionDirectory;
    // Still the library shown when the answer comes.
    const current = () => openDirectory.current === sessionDirectory;
    hotFolderCommand<HotFolderStatus | null>("hot_folder_resume", { directory: sessionDirectory })
      .then((status) => {
        if (typeof status?.folder !== "string") return;
        // Resumed for a library left meanwhile: it must not feed the next one.
        if (!current()) {
          void invoke("hot_folder_suspend", { directory: sessionDirectory });
          return;
        }
        setHotFolder(status);
        const folder = status.folder.split(/[\\/]/).pop();
        if (status.missing) toast.info(i18n.t("session.toasts.watchingMissing", { folder }));
        else toast.info(i18n.t("session.toasts.watchingResumed", { folder }));
      })
      .catch((e) => {
        if (current()) toast.error(i18n.t("session.toasts.hotFolderResumeFailed", { error: String(e) }));
      });
  }, [sessionDirectory]);

  const startHotFolder = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!session) return;
    const { nextCapture, metadata } = nextCaptureOf(nextCaptureForm, presets);
    try {
      const status = await hotFolderCommand<HotFolderStatus>("hot_folder_start", {
        directory: session.directory,
        folder: hotPath,
        template,
        nextCapture,
        metadata,
        includeSubfolders,
      });
      setHotFolder(status);
      setForm(null);
      toast.success(t("session.toasts.watching", { folder: hotPath.split(/[\\/]/).pop() }));
    } catch (e) {
      setError(String(e));
    }
  };
  const stopHotFolder = async () => {
    try {
      await hotFolderCommand("hot_folder_stop", { directory: session?.directory ?? null });
      setHotFolder(null);
    } catch (e) {
      setError(String(e));
    }
  };
  const openHotFolder = async () => {
    if (hotFolder) {
      setHotPath(hotFolder.folder);
      setIncludeSubfolders(!!hotFolder.includeSubfolders);
      setNextCaptureForm((form) => ({
        ...form,
        rating: hotFolder.metadata?.rating ?? 0,
        keywords: (hotFolder.metadata?.keywords ?? []).join(", "),
      }));
    }
    setForm("hotfolder");
    try {
      setPresets(flattenPresets((await invoke<PresetItemJson[]>(Invokes.LoadPresets)) ?? []));
    } catch {
      setPresets([]);
    }
  };

  const relinkList = () =>
    Object.entries(relinks)
      .filter(([, to]) => to.trim())
      .map(([from, to]) => ({ from, to: to.trim() }));
  const previewImport = async () => {
    setError("");
    try {
      setImportPreview(
        await invoke<ImportPreview>("library_import_preview", {
          source: importSource.trim(),
          relinks: relinkList(),
        }),
      );
    } catch (e) {
      setError(String(e));
    }
  };
  const runImport = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!session || !importPreview) return;
    setBusy(true);
    setError("");
    try {
      const report = await invoke<ImportReport>("library_import", {
        source: importSource.trim(),
        directory: session.directory,
        relinks: relinkList(),
      });
      toast.success(
        [
          t("session.toasts.imported", {
            count: report.imported,
            withSettings: report.withSettings,
            albums: report.albums,
          }),
          report.missing ? t("session.toasts.importNotFound", { count: report.missing }) : null,
          report.copiesSkipped
            ? t("session.toasts.importCopiesSkipped", { count: report.copiesSkipped })
            : null,
        ]
          .filter(Boolean)
          .join(" · "),
        { autoClose: 8000 },
      );
      if (report.errors.length) toast.error(report.errors[0], { autoClose: false });
      window.dispatchEvent(new Event(SESSION_ALBUMS_CHANGED));
      setForm(null);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const refresh = useCallback(async () => {
    if (!session) return;
    const request = ++summaryRequest.current;
    const summary = await invoke<SessionSummary>("session_summary", {
      directory: session.directory,
    });
    if (request === summaryRequest.current && useLibraryStore.getState().sessionDirectory === session.directory)
      setSession(summary);
  }, [session]);
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  // The folder list's "Synchronize Folder…".
  useEffect(() => {
    const open = (e: Event) => setSyncing({ folder: (e as CustomEvent<OpenFolderSyncDetail>).detail.folder });
    window.addEventListener(OPEN_FOLDER_SYNC_EVENT, open);
    return () => window.removeEventListener(OPEN_FOLDER_SYNC_EVENT, open);
  }, []);
  /** The library menu's "Synchronize Folder…": which folder (the one shown, else the library's). */
  const chooseFolderToSync = async () => {
    if (!session) return;
    try {
      // An album view's "folder" is its name ("Album: …"), not a place.
      const shown = useLibraryStore.getState().currentFolderPath;
      const picked = await open({
        title: t("session.sync.chooseFolder"),
        directory: true,
        multiple: false,
        defaultPath: shown && !shown.startsWith("Album: ") ? shown : session.directory,
      });
      if (typeof picked === "string") setSyncing({ folder: picked });
    } catch (e) {
      toast.error(String(e));
    }
  };
  // A relink, or a check that found photographs gone or back, changes how
  // many photographs are offline.
  useEffect(() => {
    const changed = () => void refreshRef.current().catch(() => {});
    window.addEventListener(OFFLINE_CHANGED, changed);
    window.addEventListener(OFFLINE_COUNTED, changed);
    return () => {
      window.removeEventListener(OFFLINE_CHANGED, changed);
      window.removeEventListener(OFFLINE_COUNTED, changed);
    };
  }, []);
  // A drive connected, ejected or mounted elsewhere while a library is open.
  const libraryOpen = Boolean(session?.directory);
  useEffect(() => (libraryOpen ? watchVolumes() : undefined), [libraryOpen]);

  const mark = useCallback(
    (flag: number, only?: string[]) => {
      const selectedImage = useEditorStore.getState().selectedImage;
      const paths = only
        ? [...only]
        : useUIStore.getState().activeView === "editor" && selectedImage
          ? [selectedImage.path]
          : [...useLibraryStore.getState().multiSelectedPaths];
      if (!session || paths.length === 0) return;
      // Rapid P/X/U commands must reach SQLite in keyboard order. Apply only
      // acknowledged changes, and never paint a previous session's response.
      flagWriteQueue.current = flagWriteQueue.current.then(async () => {
        try {
          const changed = await invoke<Record<string, number>>("session_set_flag", {
            directory: session.directory,
            paths,
            flag,
          });
          if (useLibraryStore.getState().sessionDirectory !== session.directory) return;
          useLibraryStore.getState().setLibrary((state) => ({
            imageFlags: { ...state.imageFlags, ...changed },
          }));
          void refresh().catch((e) =>
            toast.error(t("session.toasts.flagsSavedRefreshFailed", { error: String(e) })),
          );
        } catch (e) {
          toast.error(t("session.toasts.flagsFailed", { error: String(e) }));
        }
      });
      return flagWriteQueue.current;
    },
    [session, refresh, t],
  );
  const { showContextMenu } = useContextMenu();
  const setCenterSlot = useCallback(
    (center: HTMLDivElement | null) => useSessionBar.getState().set({ center }),
    [],
  );
  const setEndSlot = useCallback(
    (end: HTMLDivElement | null) => useSessionBar.getState().set({ end }),
    [],
  );
  useEffect(() => {
    useSessionBar.getState().set({ mark: session ? mark : null });
    return () => useSessionBar.getState().set({ mark: null });
  }, [mark, session]);

  useLayoutEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (form) {
        // The import dialog's photo grid and its large view take their own
        // keys (arrows, Space; Escape leaves the large view, not the
        // dialog). Global shortcuts ignore keys while a dialog is open.
        if (target?.closest?.(".session-ingest-preview")) return;
        e.stopImmediatePropagation();
        if (e.key === "Escape") {
          e.preventDefault();
          if (!busy) setForm(null);
        }
        return;
      }
      if (
        e.ctrlKey ||
        e.metaKey ||
        e.altKey ||
        document.querySelector('dialog[open], [aria-modal="true"]') ||
        target.closest('input,textarea,select,[contenteditable="true"]')
      )
        return;
      // Pick P, unflag U, reject ⇧X: plain X is the proof margin (Capture
      // One's key, user decision 2026-10-11).
      const key = e.key.toLowerCase();
      const flag = key === "p" ? 1 : key === "u" ? 0 : key === "x" && e.shiftKey ? -1 : null;
      if (session && flag !== null) {
        e.preventDefault();
        e.stopImmediatePropagation();
        void mark(flag);
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [mark, form, session, busy]);

  const chooseFolder = async (setter: (value: string) => void) => {
    try {
      const value = await open({ directory: true, multiple: false });
      if (typeof value === "string") setter(value);
    } catch (e) {
      setError(String(e));
    }
  };
  const openSession = async () => {
    const directory = await open({
      directory: true,
      multiple: false,
      title: t("session.dialogs.openTitle"),
    });
    if (typeof directory !== "string") return;
    setBusy(true);
    setError("");
    try {
      await navigate(
        await invoke<SessionSummary>("session_open", { directory }),
      );
    } catch (e) {
      setError(String(e));
      toast.error(String(e));
    } finally {
      setBusy(false);
    }
  };
  const openImport = () => {
    setImportPreview(null);
    setRelinks({});
    setForm("import");
  };
  /** Imports into the open catalog, else creates one to import into. */
  const startImport = () => {
    if (session?.manifest.mode === "catalog") return openImport();
    setImportAfterCreate(true);
    setForm("create");
  };
  const create = async (event: React.FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError("");
    const asCatalog = catalog || importAfterCreate;
    try {
      const directory = await join(
        parent,
        name.trim() + (asCatalog ? ".catalog" : ""),
      );
      const summary = await invoke<SessionSummary>("session_create", {
        directory,
        name: name.trim(),
        catalog: asCatalog,
      });
      await navigate(summary);
      if (importAfterCreate) {
        setImportAfterCreate(false);
        openImport();
      } else setForm(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  /** Lists the photographs in `paths` (folders opened up) in the ingest
   * dialog, after those already there unless `replace`. Photographs the
   * library already holds start unchecked. */
  const addSources = async (paths: string[], replace: boolean) => {
    // A new list (a new dialog) makes scans still running for the old one stale.
    const list = replace ? ++scanList.current : scanList.current;
    if (replace) {
      listed.current = [];
      setSources([]);
      setChosen(new Set());
      scansRunning.current = 0;
      setScanning(false);
    }
    if (!paths.length) return;
    scansRunning.current++;
    setScanning(true);
    try {
      const scan = await invoke<IngestScan>("session_ingest_scan", {
        directory: session?.directory ?? null,
        paths,
      });
      if (list !== scanList.current) return;
      const known = new Set(listed.current.map((file) => file.path));
      const added = scan.files.filter((file) => !known.has(file.path));
      listed.current = [...listed.current, ...added];
      setSources(listed.current);
      setChosen((selected) => {
        const next = new Set(selected);
        for (const file of added) if (!file.duplicate) next.add(file.path);
        return next;
      });
      if (!listed.current.length) setError(t("session.ingest.noPhotos"));
      else if (scan.truncated) setError(t("session.ingest.truncated", { n: scan.files.length.toLocaleString() }));
      else setError("");
    } catch (e) {
      if (list === scanList.current) setError(String(e));
    } finally {
      if (list === scanList.current && --scansRunning.current <= 0) setScanning(false);
    }
  };
  const openIngest = (paths: string[] = [], next: IngestTarget = NO_TARGET) => {
    setTarget(next);
    setAddToAlbum(true);
    setForm("ingest");
    void addSources(paths, true);
  };
  const extensions = () => {
    const types = useSettingsStore.getState().supportedTypes;
    return types ? [...types.raw, ...types.nonRaw] : FALLBACK_EXTENSIONS;
  };
  const chooseSources = async (folders: boolean) => {
    try {
      const value = await open(
        folders
          ? { multiple: true, directory: true, title: t("session.ingest.chooseFolderTitle") }
          : {
              multiple: true,
              directory: false,
              title: t("session.ingest.chooseTitle"),
              filters: [{ name: t("session.ingest.photographs"), extensions: extensions() }],
            },
      );
      const paths = Array.isArray(value) ? value : value ? [value] : [];
      if (paths.length) await addSources(paths, false);
    } catch (e) {
      setError(String(e));
    }
  };

  /** Files dropped onto the window: the ingest dialog with them (never a
   * silent copy), onto an album or folder with that as the target. */
  const onFileDrop = ({ paths, target: onto }: FileDrop) => {
    if (!session) {
      toast.info(t("session.drop.noLibrary"));
      return;
    }
    if (importing) {
      toast.info(t("session.drop.importing"));
      return;
    }
    // Synchronize Folder and the library copy are dialogs of their own.
    if (syncing || transfer) return;
    if (form && form !== "ingest") return;
    const next: IngestTarget = {
      folder:
        onto?.kind === "folder" &&
        folderAcceptsDrop(onto.path, session.directory, session.manifest.mode === "catalog")
          ? onto.path
          : null,
      album: onto?.kind === "album" ? { id: onto.id, name: onto.name } : null,
    };
    if (form === "ingest") {
      // More files dropped onto the open dialog join the list.
      if (next.folder || next.album) setTarget(next);
      void addSources(paths, false);
    } else openIngest(paths, next);
  };
  useWindowFileDrop(onFileDrop, setFileDrag);
  // The folder list's "Import into this folder…": the dialog with that
  // destination and no photographs yet.
  const importInto = useRef<(folder: string) => void>(() => {});
  importInto.current = (folder: string) => {
    if (!session || importing) return;
    if (form === "ingest") setTarget((current) => ({ ...current, folder }));
    else if (!form) openIngest([], { folder, album: null });
  };
  useEffect(() => {
    const open = (e: Event) => importInto.current((e as CustomEvent<OpenIngestDetail>).detail.folder);
    window.addEventListener(OPEN_INGEST_EVENT, open);
    return () => window.removeEventListener(OPEN_INGEST_EVENT, open);
  }, []);

  // Recomputed whenever the sources or the target change, never kept from
  // the last import (ingestMode.ts has the rules).
  const modeChoice = preselectIngestMode({
    catalog: session?.manifest.mode === "catalog",
    files: sources,
    intoFolder: Boolean(target.folder),
  });
  const preselected = modeChoice.mode;
  useEffect(() => setMode(preselected), [sources, target.folder, preselected]);
  const ingestMode: IngestMode = mode === "add" && modeChoice.canAdd ? "add" : "copy";

  const ingest = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!session) return;
    const files = sources.filter((file) => chosen.has(file.path)).map((file) => file.path);
    if (!files.length) return;
    const adding = ingestMode === "add";
    const folder = adding ? null : target.folder;
    const album = addToAlbum ? target.album : null;
    cancelRequested.current = false;
    setImporting(true);
    setProgress({ current: 0, total: files.length, mbps: 0 });
    setForm(null);
    try {
      // The grid shows where the photographs go, and they appear in it as
      // each is copied (useTauriListeners, `session-ingest-imported`).
      // Added photographs appear in their own folders: the first one's is shown.
      const destination = adding
        ? files[0].slice(0, Math.max(files[0].lastIndexOf("/"), files[0].lastIndexOf("\\")))
        : (folder ?? (await join(session.directory, session.manifest.capture)));
      setImportMode(adding ? "add" : "copy");
      if (useUIStore.getState().activeView !== "editor") await onNavigate(destination);
      // Cancel pressed before the ingest started (the backend clears its
      // cancel flag when it starts): nothing is copied.
      if (cancelRequested.current) return;
      const results = await invoke<ImportResult[]>("session_ingest", {
        directory: session.directory,
        sources: files,
        mode: adding ? "add" : "copy",
        template,
        sequenceStart,
        backup,
        folder,
      });
      const failures = results.filter((r) => r.error);
      const skipped = results.filter((r) => r.skipped).length;
      const copied = results.length - failures.length - skipped;
      const sidecars = results.filter((r) => r.sidecar && !r.error).length;
      const warnings = results.filter((r) => r.warning);
      let albumError: string | null = null;
      if (album) {
        const paths = results.filter((r) => r.destination && !r.error).map((r) => r.destination as string);
        if (paths.length)
          try {
            await invoke("session_album_add", { directory: session.directory, id: album.id, paths });
            window.dispatchEvent(new Event(SESSION_ALBUMS_CHANGED));
          } catch (e) {
            albumError = String(e);
          }
      }
      if (failures.length)
        toast.error(
          t("session.toasts.ingestFailed", {
            copied,
            count: failures.length,
            error: failures[0].error,
          }),
          { autoClose: false },
        );
      else
        toast.success(
          [
            adding
              ? t("session.toasts.added", { count: copied })
              : t("session.toasts.ingested", { count: copied }),
            skipped ? t("session.toasts.duplicatesSkipped", { count: skipped }) : null,
            sidecars ? t("session.toasts.sidecarsCopied", { count: sidecars }) : null,
            album && !albumError ? t("session.toasts.addedToAlbum", { name: album.name }) : null,
            results.length < files.length ? t("session.toasts.ingestCancelled") : null,
          ]
            .filter(Boolean)
            .join(" · "),
        );
      if (warnings.length)
        toast.warning(
          t("session.toasts.sidecarWarnings", { count: warnings.length, error: warnings[0].warning }),
          { autoClose: false },
        );
      if (albumError)
        toast.error(t("session.toasts.albumAddFailed", { name: album?.name, error: albumError }), {
          autoClose: false,
        });
      await refresh();
    } catch (e) {
      toast.error(String(e), { autoClose: false });
    } finally {
      setImporting(false);
      window.dispatchEvent(new Event(IMPORTS_CHANGED));
    }
  };

  // Where the ingest copies to: the full path, so a folder on another drive
  // is plain to see, and that drive's name.
  const libraryRoot = session?.directory.replace(/[\\/]+$/, "") ?? "";
  const destinationPath = target.folder ?? `${libraryRoot}/${session?.manifest.capture ?? "Capture"}`;
  const insideLibrary = Boolean(libraryRoot) && destinationPath.startsWith(`${libraryRoot}`) &&
    /[\\/]/.test(destinationPath.charAt(libraryRoot.length));
  const destinationLabel = insideLibrary
    ? [session?.manifest.name, ...destinationPath.slice(libraryRoot.length + 1).split(/[\\/]/)].join(" / ")
    : fileName(destinationPath);
  const destinationVolume = volumeOf(destinationPath);

  return (
    <>
      {/* One bar for the job (left), the workspace's modules (middle, filled
          by the studio) and the job's actions (right), like Lightroom's
          identity plate and module picker on a single row. */}
      <div className="session-bar" data-tauri-drag-region={nativeTitlebar || undefined}>
        <div className="session-bar-start" data-tauri-drag-region={nativeTitlebar || undefined}>
          <span className="session-mark" title={t("session.bar.markTitle")} aria-hidden>
            <RawtonicMark size={18} />
          </span>
          <button
            className="session-identity"
            onClick={(e) => {
              const rect = e.currentTarget.getBoundingClientRect();
              showContextMenu(rect.left, rect.bottom + 4, [
                {
                  label: t("session.menu.open"),
                  icon: FolderOpen,
                  onClick: () => void openSession(),
                },
                {
                  label: t("session.menu.new"),
                  icon: FolderPlus,
                  onClick: () => {
                    setImportAfterCreate(false);
                    setForm("create");
                  },
                },
                {
                  label: t("session.menu.importFrom"),
                  icon: Import,
                  onClick: startImport,
                },
                {
                  label: t("session.sync.menu"),
                  icon: RefreshCcw,
                  disabled: !session,
                  onClick: () => void chooseFolderToSync(),
                },
                {
                  // Every folder of the library, drives not connected included
                  // (their photographs are listed as not available).
                  label: t("session.sync.menuLibrary"),
                  icon: RefreshCcw,
                  disabled: !session,
                  onClick: () => setSyncing({ folder: null }),
                },
                { label: t("session.transfer.transfer"), icon: FolderOpen, disabled: !session || !!hotFolder,
                  onClick: () => setTransfer("transfer") },
                { label: t("session.transfer.backup"), icon: Database, disabled: !session || !!hotFolder,
                  onClick: () => setTransfer("backup") },
                { label: t("session.transfer.restore"), icon: FolderOpen, disabled: !!hotFolder,
                  onClick: () => setTransfer("restore") },
              ]);
            }}
            disabled={busy || importing}
            aria-haspopup="menu"
            title={session?.directory || t("session.bar.identityTitle")}
          >
            {session?.manifest.mode === "catalog" ? (
              <Database size={15} />
            ) : (
              <FolderOpen size={15} />
            )}
            <span>{session?.manifest.name || t("session.bar.noSession")}</span>
            <ChevronDown size={12} />
          </button>
          {session && (
            <span
              className="session-count"
              data-tauri-drag-region={nativeTitlebar || undefined}
              // Cut off in a narrow window: the full count as a tooltip.
              onMouseEnter={(e) => {
                const count = e.currentTarget;
                count.title = count.scrollWidth > count.clientWidth ? (count.textContent ?? "") : "";
              }}
            >
              {t("session.bar.photos", {
                count: session.assets,
                formatted: session.assets.toLocaleString(),
              })}
              {session.picked > 0 && ` · ${t("session.bar.picked", { count: session.picked })}`}
              {(session.missing ?? 0) > 0 && (
                <span className="session-count-offline" title={t("offline.sessionCountTitle")}>
                  {` · ${t("offline.sessionCount", { count: session.missing })}`}
                </span>
              )}
              {session.xmp_pending > 0 && (
                <span title={t("session.bar.xmpPendingTitle")}>
                  {` · ${t("session.bar.xmpPending", { count: session.xmp_pending })}`}
                </span>
              )}
            </span>
          )}
        </div>
        <div
          className="session-bar-center"
          ref={setCenterSlot}
        />
        <div className="session-bar-end" data-tauri-drag-region={nativeTitlebar || undefined}>
          <button
            className="session-action"
            disabled={!session || importing}
            title={t("session.bar.ingestTitle")}
            onClick={() => openIngest()}
          >
            <ArrowDownToLine size={14} />
            <span>{t("session.bar.ingest")}</span>
          </button>
          <button
            className={`session-action${hotFolder ? " is-active" : ""}`}
            disabled={!session}
            aria-pressed={!!hotFolder}
            title={
              hotFolder?.missing
                ? t("session.bar.hotFolderMissingTitle", { folder: hotFolder.folder })
                : hotFolder
                  ? t("session.bar.hotFolderWatching", {
                      folder: hotFolder.folder,
                      count: hotFolder.imported,
                    })
                  : t("session.bar.hotFolderTitle")
            }
            onClick={() => void openHotFolder()}
          >
            <FolderSync size={14} />
            <span>
              {hotFolder?.missing
                ? t("session.bar.hotFolderMissing")
                : hotFolder
                  ? t("session.bar.hotFolderCount", { imported: hotFolder.imported })
                  : t("session.bar.hotFolder")}
            </span>
          </button>
          <button
            className={`session-action${tether ? " is-active" : ""}`}
            disabled={!session}
            aria-pressed={tetherOpen}
            title={tether ? t("session.bar.tetherConnected", { camera: tether.camera.model }) : t("session.bar.tetherTitle")}
            onClick={() => setTetherOpen((open) => !open)}
          >
            <Camera size={14} />
            <span>{tether ? tether.camera.model : t("session.bar.tether")}</span>
          </button>
          <div
            className="session-bar-slot"
            ref={setEndSlot}
          />
          {windowControls}
        </div>
      </div>
      {/* Stays mounted while a camera is connected, so ⌘K, capture
          reports and disconnects work with the panel closed. */}
      {(tetherOpen || tether) && session && (
        <TetherPanel
          hidden={!tetherOpen}
          directory={session.directory}
          jobName={session.manifest.name}
          status={tether}
          onStatus={setTether}
          onClose={() => setTetherOpen(false)}
        />
      )}
      {!session && imageList.length === 0 && roots.length === 0 && (
        <div className="session-welcome">
          <RawtonicMark size={48} />
          <h1>{t("session.welcome.title")}</h1>
          <p>{t("session.welcome.body")}</p>
          <div className="session-welcome-modes">
            <button
              className="session-welcome-mode"
              aria-label={t("session.welcome.create")}
              aria-describedby="welcome-session-description"
              onClick={() => {
                setCatalog(false);
                setImportAfterCreate(false);
                setForm("create");
              }}
            >
              <FolderOpen size={24} aria-hidden />
              <strong>{t("session.create.session")}</strong>
              <span className="session-mode-purpose">{t("session.create.sessionHint")}</span>
              <span id="welcome-session-description" className="session-mode-description">
                {t("session.create.sessionDescription")}
              </span>
              <span className="session-mode-cta">{t("session.welcome.create")} <ArrowRight size={15} aria-hidden /></span>
            </button>
            <button
              className="session-welcome-mode"
              aria-label={t("session.welcome.createCatalog")}
              aria-describedby="welcome-catalog-description"
              onClick={() => {
                setCatalog(true);
                setImportAfterCreate(false);
                setForm("create");
              }}
            >
              <Database size={24} aria-hidden />
              <strong>{t("session.create.catalog")}</strong>
              <span className="session-mode-purpose">{t("session.create.catalogHint")}</span>
              <span id="welcome-catalog-description" className="session-mode-description">
                {t("session.create.catalogDescription")}
              </span>
              <span className="session-mode-cta">{t("session.welcome.createCatalog")} <ArrowRight size={15} aria-hidden /></span>
            </button>
          </div>
          <div className="session-welcome-existing">
            <button
              className="session-action"
              onClick={() => void openSession()}
            >
              <FolderOpen size={16} aria-hidden />
              {t("session.welcome.open")}
            </button>
            <button className="session-welcome-import" onClick={startImport}>
              {t("session.welcome.importFrom")}
            </button>
          </div>
          <span>{t("session.welcome.tagline")}</span>
        </div>
      )}
      {importing && progress && (
        <div className="session-progress" role="status">
          <LoaderCircle size={15} className="animate-spin" />
          <span>
            {t(importMode === "add" ? "session.progress.adding" : "session.progress.verifying", {
              current: progress.current,
              count: progress.total,
            })}
          </span>
          <progress value={progress.current} max={progress.total} />
          <span>{progress.mbps.toFixed(1)} MB/s</span>
          <button
            onClick={() => {
              cancelRequested.current = true;
              void invoke("session_cancel_ingest");
            }}
          >
            {t("session.progress.cancel")}
          </button>
        </div>
      )}
      {fileDrag && session && !importing && !syncing && !transfer && (!form || form === "ingest") && (
        <div className="session-drop-overlay" aria-hidden>
          <span>
            <ArrowDownToLine size={18} />
            {t("session.drop.overlay", { name: session.manifest.name })}
          </span>
        </div>
      )}
      {syncing && session && (
        <FolderSyncDialog
          directory={session.directory}
          folder={syncing.folder}
          onClose={() => setSyncing(null)}
          onApplied={(applied) => {
            // Moved, added and removed photographs: counts, folders, the
            // grid, Missing Photographs and Previous Import follow.
            void refresh().catch(() => {});
            window.dispatchEvent(new Event(OFFLINE_CHANGED));
            window.dispatchEvent(new CustomEvent(FOLDER_SYNC_APPLIED, { detail: applied }));
            window.dispatchEvent(new Event(IMPORTS_CHANGED));
          }}
        />
      )}
      {transfer && <LibraryTransferDialog purpose={transfer} directory={session?.directory} name={session?.manifest.name}
        onClose={() => setTransfer(null)} onOpen={async (directory) => {
          await navigate(await invoke<SessionSummary>("session_open", { directory }));
        }} />}
      <dialog
        ref={dialog}
        className={`session-dialog ${form === "ingest" ? "session-ingest-dialog" : ""}`}
        onCancel={(e) => {
          if (busy) e.preventDefault();
          else setForm(null);
        }}
        onClose={() => {
          setForm(null);
          setImportAfterCreate(false);
        }}
        aria-labelledby="session-dialog-title"
      >
        <div className="session-dialog-header">
          <div>
            <h2 id="session-dialog-title">
              {form === "create"
                ? importAfterCreate
                  ? t("session.create.importTitle")
                  : t("session.create.title")
                : form === "hotfolder"
                  ? t("session.hotFolder.title")
                  : form === "import"
                    ? t("session.import.title")
                    : t("session.ingest.title")}
            </h2>
            <p>
              {form === "create"
                ? importAfterCreate
                  ? t("session.create.importSubtitle")
                  : t("session.create.subtitle")
                : form === "hotfolder"
                  ? t("session.hotFolder.subtitle")
                  : form === "import"
                    ? t("session.import.subtitle")
                    : t("session.ingest.subtitle")}
            </p>
          </div>
          <button
            className="session-icon-button"
            aria-label={t("session.dialogs.close")}
            disabled={busy}
            onClick={() => setForm(null)}
          >
            <X size={19} />
          </button>
        </div>
        {form === "import" ? (
          <form onSubmit={(e) => void runImport(e)}>
            <label className="session-field">
              {t("session.import.source")}
              <div className="session-backup">
                <input
                  aria-label={t("session.import.sourceAria")}
                  value={importSource}
                  placeholder={t("session.import.sourcePlaceholder")}
                  spellCheck={false}
                  required
                  onChange={(e) => {
                    setImportSource(e.target.value);
                    setImportPreview(null);
                  }}
                />
                <button
                  type="button"
                  className="session-icon-button"
                  aria-label={t("session.import.chooseSource")}
                  onClick={() =>
                    void open({
                      multiple: false,
                      directory: false,
                      filters: [
                        {
                          name: t("session.import.filterName"),
                          extensions: ["lrcat", "cosessiondb"],
                        },
                      ],
                    }).then((value) => {
                      if (typeof value === "string") {
                        setImportSource(value);
                        setImportPreview(null);
                      }
                    })
                  }
                >
                  <FolderOpen size={16} />
                </button>
              </div>
            </label>
            {importPreview && (
              <div className="session-import-preview" role="status">
                <p>
                  <strong>{importPreview.name}</strong>
                  {t("session.import.preview", {
                    items: [
                      t("session.import.photos", { count: importPreview.photos }),
                      t("session.import.edited", { count: importPreview.edited }),
                      t("session.import.albums", { count: importPreview.albums }),
                      importPreview.copies
                        ? t("session.import.copiesNotImported", { count: importPreview.copies })
                        : null,
                      importPreview.notRead.smartAlbums
                        ? t("session.import.smartAlbumsNotImported", {
                            count: importPreview.notRead.smartAlbums,
                          })
                        : null,
                    ]
                      .filter(Boolean)
                      .join(t("session.import.separator")),
                  })}
                </p>
                <ul>
                  {importPreview.roots.map((root) => (
                    <li key={root.folder}>
                      <span title={root.folder}>
                        {t("session.import.rootFound", {
                          folder: root.folder.split(/[\\/]/).slice(-2).join("/"),
                          found: root.found,
                          count: root.photos,
                        })}
                      </span>
                      {root.found < root.photos && (
                        <input
                          aria-label={t("session.import.relinkAria", { folder: root.folder })}
                          placeholder={t("session.import.relinkPlaceholder")}
                          value={relinks[root.folder] ?? ""}
                          onChange={(e) => setRelinks({ ...relinks, [root.folder]: e.target.value })}
                          onBlur={() => void previewImport()}
                        />
                      )}
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {error && (
              <p className="session-error" role="alert">
                {error}
              </p>
            )}
            <div className="session-dialog-footer">
              <button
                type="button"
                className="session-action"
                disabled={!importSource.trim() || busy}
                onClick={() => void previewImport()}
              >
                {t("session.import.check")}
              </button>
              <button
                type="submit"
                className="session-action session-primary"
                disabled={!importPreview || busy}
              >
                <Import size={16} />
                {t("session.import.submit", { name: session?.manifest.name ?? "" })}
              </button>
            </div>
          </form>
        ) : form === "hotfolder" ? (
          <form onSubmit={(e) => void startHotFolder(e)}>
            <label className="session-field">
              {t("session.hotFolder.folder")}
              <div className="session-backup">
                <input
                  aria-label={t("session.hotFolder.folder")}
                  value={hotPath}
                  placeholder={t("session.hotFolder.folderPlaceholder")}
                  spellCheck={false}
                  required
                  onChange={(e) => setHotPath(e.target.value)}
                />
                <button
                  type="button"
                  className="session-icon-button"
                  aria-label={t("session.hotFolder.chooseFolder")}
                  onClick={() => void chooseFolder(setHotPath)}
                >
                  <FolderOpen size={16} />
                </button>
              </div>
            </label>
            <label className="session-ingest-album">
              <input
                type="checkbox"
                checked={includeSubfolders}
                onChange={(e) => setIncludeSubfolders(e.target.checked)}
              />
              {t("session.hotFolder.includeSubfolders")}
            </label>
            <NamingEditor mode="ingest" value={template} onChange={setTemplate}
              label={t("session.fields.filenameTemplate")} job={session?.manifest.name} />

            <NextCaptureFields value={nextCaptureForm} onChange={setNextCaptureForm} presets={presets} />
            <div className="session-copy-note">
              <Check size={16} />
              <span>
                <Trans
                  i18nKey="session.hotFolder.copyNote"
                  values={{ target: `${session?.manifest.name ?? ""} / Capture` }}
                  components={{ strong: <strong /> }}
                />
              </span>
            </div>
            {error && (
              <p className="session-error" role="alert">
                {error}
              </p>
            )}
            <div className="session-dialog-footer">
              {hotFolder ? (
                <button
                  type="button"
                  className="session-action"
                  onClick={() => void stopHotFolder()}
                >
                  {t("session.hotFolder.stop")}
                </button>
              ) : (
                <span>{t("session.hotFolder.resumesOnOpen")}</span>
              )}
              <button
                type="submit"
                className="session-action session-primary"
                disabled={!hotPath.trim() || !validNamingTemplate(template, "ingest")}
              >
                <FolderSync size={16} />
                {hotFolder ? t("session.hotFolder.switch") : t("session.hotFolder.start")}
              </button>
            </div>
          </form>
        ) : form === "create" ? (
          <form onSubmit={(e) => void create(e)}>
            {!importAfterCreate && (
              <fieldset className="session-mode">
                <legend>{t("session.create.modeLabel")}</legend>
                <label>
                  <input type="radio" name="mode" checked={!catalog} onChange={() => setCatalog(false)} />
                  <span>
                    <strong>{t("session.create.session")}</strong>
                    <small>{t("session.create.sessionHint")}</small>
                  </span>
                </label>
                <label>
                  <input type="radio" name="mode" checked={catalog} onChange={() => setCatalog(true)} />
                  <span>
                    <strong>{t("session.create.catalog")}</strong>
                    <small>{t("session.create.catalogHint")}</small>
                  </span>
                </label>
              </fieldset>
            )}
            <p className="session-create-description">
              {catalog || importAfterCreate
                ? t("session.create.catalogDescription")
                : t("session.create.sessionDescription")}
            </p>
            <label className="session-field">
              {catalog || importAfterCreate ? t("session.create.catalogName") : t("session.create.name")}
              <input
                autoFocus
                required
                maxLength={160}
                value={name}
                onChange={(e) => setName(e.target.value)}
                pattern={'[^/\\\\:*?"<>|]+'}
                placeholder={
                  catalog || importAfterCreate
                    ? t("session.create.catalogPlaceholder")
                    : t("session.create.namePlaceholder")
                }
              />
            </label>
            <label className="session-field">
              {t("session.create.location")}
              <button
                type="button"
                className="session-path"
                onClick={() => void chooseFolder(setParent)}
              >
                <FolderOpen size={17} />
                <span>{parent || t("session.create.chooseParent")}</span>
              </button>
            </label>
            <div className="session-folder-preview">
              <FolderOpen size={16} />
              <span>
                {name.trim() ||
                  (catalog || importAfterCreate
                    ? t("session.create.catalogPlaceholder")
                    : t("session.create.namePreview"))}
                {catalog || importAfterCreate ? ".catalog" : ""}
              </span>
              {catalog || importAfterCreate ? <code>Photos / Output</code> : <code>Capture / Selects / Output</code>}
            </div>
            {error && (
              <p className="session-error" role="alert">
                {error}
              </p>
            )}
            <div className="session-dialog-footer">
              <span>{t("session.create.originalsUntouched")}</span>
              <button
                type="submit"
                className="session-action session-primary"
                disabled={busy || !name.trim() || !parent}
              >
                {busy ? (
                  <LoaderCircle size={16} className="animate-spin" />
                ) : (
                  <FolderPlus size={16} />
                )}
                {catalog || importAfterCreate
                  ? t("session.create.createCatalog")
                  : t("session.create.createSession")}
              </button>
            </div>
          </form>
        ) : (
          <form className="session-ingest-form" onSubmit={(e) => void ingest(e)}>
            <section className="session-ingest-source">
              <div className="session-ingest-source-head">
                <h3>{t("naming.source")}</h3>
                <button type="button" className="session-action" onClick={() => void chooseSources(false)}>
                  <FolderOpen size={15} />
                  {t("session.ingest.addFiles")}
                </button>
                <button type="button" className="session-action" onClick={() => void chooseSources(true)}>
                  <FolderAdd size={15} />
                  {t("session.ingest.addFolder")}
                </button>
              </div>
              {sources.length ? (
                <IngestPreviewGrid files={sources} selected={chosen} onSelectedChange={setChosen} />
              ) : (
                <div className="session-ingest-empty">
                  {scanning ? (
                    <>
                      <LoaderCircle size={16} className="animate-spin" />
                      {t("session.ingest.scanning")}
                    </>
                  ) : (
                    t("session.ingest.dropHint")
                  )}
                </div>
              )}
            </section>
            <section className="session-ingest-backup">
              <h3>{t("naming.destination")}</h3>
              {modeChoice.showSwitch && (
                <div className="session-ingest-mode" role="radiogroup" aria-label={t("session.ingest.modeLabel")}>
                  <label>
                    <input
                      type="radio"
                      name="ingest-mode"
                      checked={ingestMode === "copy"}
                      onChange={() => setMode("copy")}
                    />
                    <span>{t("session.ingest.modeCopy", { destination: destinationLabel })}</span>
                  </label>
                  <label
                    className={modeChoice.canAdd ? undefined : "is-disabled"}
                    title={modeChoice.canAdd ? undefined : t("session.ingest.addFromCard")}
                  >
                    <input
                      type="radio"
                      name="ingest-mode"
                      checked={ingestMode === "add"}
                      disabled={!modeChoice.canAdd}
                      onChange={() => setMode("add")}
                    />
                    <span>{t("session.ingest.modeAdd")}</span>
                  </label>
                  {!modeChoice.canAdd && <small>{t("session.ingest.addFromCard")}</small>}
                </div>
              )}
              {ingestMode === "copy" ? (
                <>
            <div className="session-ingest-target">
              <strong>{destinationLabel}</strong>
              <code className="session-ingest-path" title={destinationPath}>
                {destinationPath}
              </code>
              {session?.manifest.mode === "catalog" && !target.folder && <small>{t("session.ingest.dateFolders")}</small>}
              {destinationVolume && (
                <small className="session-ingest-volume">
                  <HardDrive size={12} />
                  {t("session.ingest.onVolume", { name: destinationVolume })}
                </small>
              )}
              {target.folder && (
                <button type="button" className="session-link-button" onClick={() => setTarget({ ...target, folder: null })}>
                  {t("session.ingest.useCapture", { folder: session?.manifest.capture ?? "Capture" })}
                </button>
              )}
              {target.album && (
                <label className="session-ingest-album">
                  <input type="checkbox" checked={addToAlbum} onChange={(e) => setAddToAlbum(e.target.checked)} />
                  {t("session.ingest.addToAlbum", { name: target.album.name })}
                </label>
              )}
            </div>
            <label className="session-field">
              <span>
                {t("session.ingest.secondCopy")}{" "}
                <span className="session-optional">{t("session.ingest.optional")}</span>
              </span>
              <div className="session-backup">
                <button
                  type="button"
                  className="session-path"
                  onClick={() => void chooseFolder(setBackup)}
                >
                  <FolderOpen size={17} />
                  <span>{backup || t("session.ingest.chooseBackup")}</span>
                </button>
                {backup && (
                  <button
                    type="button"
                    className="session-icon-button"
                    aria-label={t("session.ingest.removeBackup")}
                    onClick={() => setBackup(null)}
                  >
                    <X size={16} />
                  </button>
                )}
              </div>
            </label>
                </>
              ) : (
                target.album && (
                  <label className="session-ingest-album">
                    <input type="checkbox" checked={addToAlbum} onChange={(e) => setAddToAlbum(e.target.checked)} />
                    {t("session.ingest.addToAlbum", { name: target.album.name })}
                  </label>
                )
              )}
            </section>
            {ingestMode === "copy" && (
            <section className="session-ingest-naming">
              <h3>{t("naming.title")}</h3>
              <NamingEditor mode="ingest" value={template} onChange={setTemplate}
                label={t("session.fields.filenameTemplate")} original={sources.find((file) => chosen.has(file.path))?.path ?? sources[0]?.path} job={session?.manifest.name} total={chosen.size}
                sequenceStart={sequenceStart} onSequenceStartChange={setSequenceStart}
                preview={namePreview?.key === previewKey ? namePreview.result : null} />
            </section>
            )}
            <div className="session-copy-note">
              <Check size={16} />
              <span>
                {ingestMode === "add" ? (
                  t("session.ingest.addNote")
                ) : (
                <Trans
                  i18nKey="session.ingest.copyNote"
                  values={{
                    target: destinationLabel,
                  }}
                  components={{ strong: <strong /> }}
                />
                )}
              </span>
            </div>
            {error && (
              <p className="session-error" role="alert">
                {error}
              </p>
            )}
            <div className="session-dialog-footer">
              <span>{t("session.ingest.keepEditing")}</span>
              <button
                type="submit"
                className="session-action session-primary"
                disabled={
                  !chosen.size ||
                  scanning ||
                  (ingestMode === "copy" &&
                    (!validNamingTemplate(template, "ingest") || !validSequenceStart(sequenceStart)))
                }
              >
                <ArrowDownToLine size={16} />
                {!chosen.size
                  ? t("session.ingest.submitNone")
                  : ingestMode === "add"
                    ? t("session.ingest.submitAdd", { count: chosen.size })
                    : t("session.ingest.submit", { count: chosen.size })}
              </button>
            </div>
          </form>
        )}
      </dialog>
    </>
  );
}
