import { validNamingTemplate, validSequenceStart } from "../../utils/namingTemplates";
import NamingEditor from "../ui/NamingEditor";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { join } from "@tauri-apps/api/path";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowDownToLine,
  Check,
  ChevronDown,
  Database,
  FolderOpen,
  FolderPlus,
  FolderSync,
  Import,
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
import { useContextMenu } from "../../context/ContextMenuContext";
import { useSessionBar } from "./sessionBar";
import { SESSION_ALBUMS_CHANGED } from "../studio/StudioAlbums";
import { OFFLINE_CHANGED, OFFLINE_COUNTED, watchVolumes } from "../../utils/offline";
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
}
/** Mirrors `hot_folder::NextCapture`. */
type NextCapture =
  | { mode: "defaults" }
  | { mode: "copyFromLast" }
  | { mode: "preset"; name: string; adjustments: Record<string, unknown> };

interface HotFolderStatus {
  folder: string;
  session: string;
  imported: number;
  nextCapture: NextCapture;
}

interface PresetChoice {
  id: string;
  name: string;
  adjustments: Record<string, unknown>;
}

/** Presets of `load_presets`, folders flattened. */
type PresetItemJson =
  | { preset: PresetChoice }
  | { folder: { name: string; children: PresetChoice[] } };
export function flattenPresets(items: PresetItemJson[]): PresetChoice[] {
  return items.flatMap((item) =>
    "preset" in item
      ? [item.preset]
      : item.folder.children.map((p) => ({ ...p, name: `${item.folder.name} / ${p.name}` })),
  );
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

export default function SessionWorkspace({ onNavigate, nativeTitlebar = false }: Props) {
  const { t } = useTranslation();
  const [session, setSession] = useState<SessionSummary | null>(null);
  const [form, setForm] = useState<"create" | "ingest" | "hotfolder" | "import" | null>(null);
  const [importSource, setImportSource] = useState("");
  const [importPreview, setImportPreview] = useState<ImportPreview | null>(null);
  const [relinks, setRelinks] = useState<Record<string, string>>({});
  const [hotFolder, setHotFolder] = useState<HotFolderStatus | null>(null);
  const [hotPath, setHotPath] = useState("");
  const [nextCapture, setNextCapture] = useState("defaults");
  const [presets, setPresets] = useState<PresetChoice[]>([]);
  const [name, setName] = useState("");
  const [parent, setParent] = useState("");
  const [catalog, setCatalog] = useState(false);
  // Coming from Lightroom or Capture One: a new catalog first, then the import.
  const [importAfterCreate, setImportAfterCreate] = useState(false);
  const [sources, setSources] = useState<string[]>([]);
  const [backup, setBackup] = useState<string | null>(null);
  const [template, setTemplate] = useState("{orig}");
  const [sequenceStart, setSequenceStart] = useState(1);
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
        ?.querySelector<HTMLElement>("form input, form button")
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

  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen<{ source: string; skipped: boolean; error: string | null }>(
      "hot-folder-imported",
      (event) => {
        const { error, skipped, source } = event.payload;
        if (error)
          toast.error(i18n.t("session.toasts.hotFolderFailed", { file: source.split(/[\\/]/).pop(), error }));
        else if (!skipped) {
          setHotFolder((current) => current && { ...current, imported: current.imported + 1 });
          void refreshRef.current();
        }
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

  // A hot folder feeds the session it was started for.
  useEffect(() => {
    if (hotFolder && session && !hotFolder.session.startsWith(session.directory.replace(/\/$/, ""))) {
      void invoke("hot_folder_stop");
      setHotFolder(null);
    }
  }, [session, hotFolder]);

  const startHotFolder = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!session) return;
    const preset = presets.find((p) => `preset:${p.id}` === nextCapture);
    const next: NextCapture = preset
      ? { mode: "preset", name: preset.name, adjustments: preset.adjustments }
      : nextCapture === "copyFromLast"
        ? { mode: "copyFromLast" }
        : { mode: "defaults" };
    try {
      const status = await invoke<HotFolderStatus>("hot_folder_start", {
        directory: session.directory,
        folder: hotPath,
        template,
        nextCapture: next,
      });
      setHotFolder(status);
      setForm(null);
      toast.success(t("session.toasts.watching", { folder: hotPath.split(/[\\/]/).pop() }));
    } catch (e) {
      setError(String(e));
    }
  };
  const stopHotFolder = async () => {
    await invoke("hot_folder_stop");
    setHotFolder(null);
  };
  const openHotFolder = async () => {
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
    (flag: number) => {
      const selectedImage = useEditorStore.getState().selectedImage;
      const paths = useUIStore.getState().activeView === "editor" && selectedImage
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
      if (
        session &&
        ["p", "x", "u"].includes(e.key.toLowerCase())
      ) {
        e.preventDefault();
        e.stopImmediatePropagation();
        void mark(e.key.toLowerCase() === "p" ? 1 : e.key.toLowerCase() === "x" ? -1 : 0);
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
  const ingest = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!session) return;
    setImporting(true);
    setProgress({ current: 0, total: sources.length, mbps: 0 });
    setForm(null);
    try {
      const results = await invoke<ImportResult[]>("session_ingest", {
        directory: session.directory,
        sources,
        template,
        sequenceStart,
        backup,
      });
      const failures = results.filter((r) => r.error);
      const skipped = results.filter((r) => r.skipped).length;
      const copied = results.length - failures.length - skipped;
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
            t("session.toasts.ingested", { count: copied }),
            skipped ? t("session.toasts.duplicatesSkipped", { count: skipped }) : null,
            results.length < sources.length ? t("session.toasts.ingestCancelled") : null,
          ]
            .filter(Boolean)
            .join(" · "),
        );
      await onNavigate(await join(session.directory, session.manifest.capture));
      await refresh();
    } catch (e) {
      toast.error(String(e), { autoClose: false });
    } finally {
      setImporting(false);
    }
  };

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
            onClick={() => {
              setSources([]);
              setForm("ingest");
            }}
          >
            <ArrowDownToLine size={14} />
            <span>{t("session.bar.ingest")}</span>
          </button>
          <button
            className={`session-action${hotFolder ? " is-active" : ""}`}
            disabled={!session}
            aria-pressed={!!hotFolder}
            title={
              hotFolder
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
              {hotFolder
                ? t("session.bar.hotFolderCount", { imported: hotFolder.imported })
                : t("session.bar.hotFolder")}
            </span>
          </button>
          <div
            className="session-bar-slot"
            ref={setEndSlot}
          />
        </div>
      </div>
      {!session && imageList.length === 0 && roots.length === 0 && (
        <div className="session-welcome">
          <RawtonicMark size={48} />
          <h1>{t("session.welcome.title")}</h1>
          <p>{t("session.welcome.body")}</p>
          <div>
            <button
              className="session-action session-primary"
              onClick={() => {
                setImportAfterCreate(false);
                setForm("create");
              }}
            >
              <FolderPlus size={17} />
              {t("session.welcome.create")}
            </button>
            <button
              className="session-action"
              onClick={() => void openSession()}
            >
              <FolderOpen size={17} />
              {t("session.welcome.open")}
            </button>
            <button className="session-action" onClick={startImport}>
              <Import size={17} />
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
            {t("session.progress.verifying", {
              current: progress.current,
              count: progress.total,
            })}
          </span>
          <progress value={progress.current} max={progress.total} />
          <span>{progress.mbps.toFixed(1)} MB/s</span>
          <button onClick={() => void invoke("session_cancel_ingest")}>
            {t("session.progress.cancel")}
          </button>
        </div>
      )}
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
            <NamingEditor mode="ingest" value={template} onChange={setTemplate}
              label={t("session.fields.filenameTemplate")} job={session?.manifest.name} />

            <label className="session-field">
              {t("session.hotFolder.nextCapture")}
              <select
                aria-label={t("session.hotFolder.nextCapture")}
                value={nextCapture}
                onChange={(e) => setNextCapture(e.target.value)}
              >
                <option value="defaults">{t("session.hotFolder.defaults")}</option>
                <option value="copyFromLast">{t("session.hotFolder.copyFromLast")}</option>
                {presets.map((p) => (
                  <option key={p.id} value={`preset:${p.id}`}>
                    {t("session.hotFolder.preset", { name: p.name })}
                  </option>
                ))}
              </select>
            </label>
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
                <span>{t("session.hotFolder.stopsOnQuit")}</span>
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
            <label className="session-field">
              {importAfterCreate ? t("session.create.catalogName") : t("session.create.name")}
              <input
                autoFocus
                required
                maxLength={160}
                value={name}
                onChange={(e) => setName(e.target.value)}
                pattern={'[^/\\\\:*?"<>|]+'}
                placeholder={
                  importAfterCreate
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
            {!importAfterCreate && (
            <div className="session-mode">
              <label>
                <input
                  type="radio"
                  name="mode"
                  checked={!catalog}
                  onChange={() => setCatalog(false)}
                />
                <span>
                  <strong>{t("session.create.session")}</strong>
                  <small>{t("session.create.sessionHint")}</small>
                </span>
              </label>
              <label>
                <input
                  type="radio"
                  name="mode"
                  checked={catalog}
                  onChange={() => setCatalog(true)}
                />
                <span>
                  <strong>{t("session.create.catalog")}</strong>
                  <small>{t("session.create.catalogHint")}</small>
                </span>
              </label>
            </div>
            )}
            <div className="session-folder-preview">
              <FolderOpen size={16} />
              <span>
                {name.trim() ||
                  (importAfterCreate
                    ? t("session.create.catalogPlaceholder")
                    : t("session.create.namePreview"))}
                {catalog || importAfterCreate ? ".catalog" : ""}
              </span>
              {!importAfterCreate && <code>Capture / Selects / Output</code>}
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
              <h3>{t("naming.source")}</h3>
            <label className="session-field">
              {t("session.ingest.photographs")}
              <button
                type="button"
                className="session-path"
                onClick={() =>
                  void open({
                    multiple: true,
                    directory: false,
                    title: t("session.ingest.chooseTitle"),
                    filters: [
                      {
                        name: t("session.ingest.photographs"),
                        extensions: [
                          "arw",
                          "dng",
                          "cr3",
                          "cr2",
                          "nef",
                          "raf",
                          "rw2",
                          "jpg",
                          "jpeg",
                          "png",
                          "tif",
                          "tiff",
                          "heic",
                          "heif",
                          "webp",
                          "avif",
                          "jxl",
                        ],
                      },
                    ],
                  })
                    .then((value) => {
                      if (Array.isArray(value)) setSources(value);
                      else if (value) setSources([value]);
                    })
                    .catch((e) => setError(String(e)))
                }
              >
                <FolderOpen size={17} />
                {sources.length
                  ? t("session.ingest.selected", { count: sources.length })
                  : t("session.ingest.choose")}
              </button>
            </label>
            {sources.length > 0 && <div className="session-ingest-files">
              {sources.slice(0, 4).map((path) => <div key={path} title={path}>{path.split(/[\\/]/).pop()}</div>)}
              {sources.length > 4 && <small>{t("naming.moreFiles", { count: sources.length - 4 })}</small>}
            </div>}
            <div className="session-ingest-target"><span>{t("naming.destination")}</span><strong>{`${session?.manifest.name} / ${session?.manifest.capture ?? "Capture"}`}</strong></div>
            </section>
            <section className="session-ingest-naming">
              <h3>{t("naming.title")}</h3>
              <NamingEditor mode="ingest" value={template} onChange={setTemplate}
                label={t("session.fields.filenameTemplate")} original={sources[0]} job={session?.manifest.name} total={sources.length}
                sequenceStart={sequenceStart} onSequenceStartChange={setSequenceStart} />
            </section>
            <section className="session-ingest-backup">
              <h3>{t("naming.backup")}</h3>

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
            </section>
            <div className="session-copy-note">
              <Check size={16} />
              <span>
                <Trans
                  i18nKey="session.ingest.copyNote"
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
              <span>{t("session.ingest.keepEditing")}</span>
              <button
                type="submit"
                className="session-action session-primary"
                disabled={!sources.length || !validNamingTemplate(template, "ingest") || !validSequenceStart(sequenceStart)}
              >
                <ArrowDownToLine size={16} />
                {sources.length
                  ? t("session.ingest.submit", { count: sources.length })
                  : t("session.ingest.submitNone")}
              </button>
            </div>
          </form>
        )}
      </dialog>
    </>
  );
}
