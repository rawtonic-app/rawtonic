// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Library › Export as Catalog… (G39): the chosen photographs of the open
 * session or catalog become a new catalog — to take a job to another
 * computer and bring it back with Import from Catalog. Lightroom Classic's
 * Export as Catalog and Capture One's Export as Catalog are the models; the
 * defaults are Lightroom's (originals referenced, previews included).
 *
 * The dialog only collects the choices; the export runs in the background
 * with its progress and Cancel in the activity centre (`runCatalogExport`).
 */
import { useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { join } from "@tauri-apps/api/path";
import { listen } from "@tauri-apps/api/event";
import { FolderOpen, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import i18n from "i18next";
import OperationError from "../ui/OperationError";
import { Invokes } from "../ui/AppProperties";
import { flushBeforeQuit, registerQuitFlush } from "../../utils/quit";
import { openDialog } from "../../utils/fileDialog";
import { useActivityStore } from "../../store/useActivityStore";
import { notify } from "../../activity/notify";

interface Progress { phase: "originals" | "finishing"; done: number; total: number }
export interface CatalogExportReceipt {
  photographs: number;
  originalsCopied: number;
  offline: number;
  previews: number;
  missingResources: number;
  missingCurrentResources: number;
}
export interface CatalogExportRequest {
  source: string;
  paths: string[];
  destination: string;
  includeOriginals: boolean;
  includePreviews: boolean;
}

/** The activity centre's entry of a running export. */
export const CATALOG_EXPORT_ACTIVITY = "catalog-export";

/** Runs an export with its progress, Cancel and result in the activity
 * centre; resolves when it settled (failures are reported there too). */
export async function runCatalogExport(request: CatalogExportRequest, onOpen: (directory: string) => Promise<void>) {
  const t = i18n.t.bind(i18n);
  const name = request.destination.split(/[\\/]/).pop() ?? request.destination;
  const activity = useActivityStore.getState();
  const id = activity.start({
    id: CATALOG_EXPORT_ACTIVITY,
    source: "files",
    title: t("session.catalogExport.running", { name, count: request.paths.length }),
    progress: { done: 0, total: request.paths.length },
    cancel: () => {
      useActivityStore.getState().update(CATALOG_EXPORT_ACTIVITY, { title: t("session.catalogExport.cancelling"), cancel: undefined });
      return invoke(Invokes.LibraryExportCatalogCancel);
    },
  });
  let stop: (() => void) | undefined;
  if (isTauri()) {
    stop = await listen<Progress>("catalog-export-progress", ({ payload }) => {
      useActivityStore.getState().update(id, {
        progress: { done: payload.done, total: payload.total },
        detail: payload.phase === "originals" ? t("session.catalogExport.copying") : t("session.catalogExport.finishing"),
      });
    });
  }
  const run = invoke<CatalogExportReceipt>(Invokes.LibraryExportCatalog, { ...request });
  const unregister = registerQuitFlush(async () => { await run.catch(() => {}); return []; });
  try {
    const receipt = await run;
    const notes = [
      receipt.originalsCopied ? t("session.catalogExport.originalsCopied", { count: receipt.originalsCopied }) : t("session.catalogExport.referenced"),
      receipt.previews ? t("session.catalogExport.previews", { count: receipt.previews }) : "",
      receipt.offline ? t("session.catalogExport.offline", { count: receipt.offline }) : "",
      receipt.missingResources ? t("session.transfer.missingResources", {
        files: receipt.missingResources, current: receipt.missingCurrentResources,
      }) : "",
    ].filter(Boolean);
    const title = t("session.catalogExport.done", { name, count: receipt.photographs });
    const open = () => onOpen(request.destination);
    useActivityStore.getState().resolve(id);
    // The user's own action: a confirmation with Open catalog, kept under Done.
    notify.success(({ closeToast }: { closeToast(): void }) => (
      <span>{title}{" "}
        <button type="button" className="session-link-button" onClick={() => { closeToast(); void open(); }}>
          {t("session.catalogExport.open")}
        </button>
      </span>
    ), {
      title, source: "files", toastId: CATALOG_EXPORT_ACTIVITY,
      detail: [request.destination, ...notes].join("\n"),
      actions: [{ label: t("session.catalogExport.open"), run: open, settles: true }],
    });
  } catch (e) {
    const message = String(e);
    useActivityStore.getState().resolve(id);
    if (/cancelled/i.test(message)) {
      notify.info(t("session.catalogExport.cancelled", { name }), { source: "files" });
    } else {
      notify.error(t("session.catalogExport.failed", { name }), { source: "files", detail: message });
    }
  } finally {
    unregister();
    stop?.();
  }
}

interface Props {
  directory: string;
  name: string;
  /** The selected photographs (paths as listed; virtual copies included). */
  selected: string[];
  /** The photographs the current folder or album shows. */
  shown: string[];
  onClose(): void;
  onOpen(directory: string): Promise<void>;
}

export default function CatalogExportDialog({ directory, name, selected, shown, onClose, onOpen }: Props) {
  const { t } = useTranslation();
  const dialog = useRef<HTMLDialogElement>(null);
  const [scope, setScope] = useState<"selected" | "shown">(selected.length ? "selected" : "shown");
  const [parent, setParent] = useState("");
  const [folder, setFolder] = useState(() => `${name || "Rawtonic"} – ${new Date().toISOString().slice(0, 10)}`);
  const [includeOriginals, setIncludeOriginals] = useState(false);
  const [includePreviews, setIncludePreviews] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const paths = scope === "selected" ? selected : shown;
  const invalidFolder = !folder.trim() || [".", ".."].includes(folder.trim()) || /[/\\:*?"<>|]/.test(folder);

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  async function choose() {
    try {
      const chosen = await openDialog({ directory: true, multiple: false, title: t("session.transfer.destination") });
      if (typeof chosen === "string") setParent(chosen);
    } catch (e) { setError(String(e)); }
  }

  async function start() {
    setBusy(true); setError("");
    try {
      // The export reads the library's database: every edit and metadata
      // change made so far goes there first.
      const failures = await flushBeforeQuit();
      if (failures.length) throw new Error(t("session.transfer.unsaved", { files: failures.join(", ") }));
      const destination = await join(parent, folder.trim());
      void runCatalogExport({ source: directory, paths, destination, includeOriginals, includePreviews }, onOpen);
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }

  return (
    <dialog ref={dialog} className="session-dialog session-transfer-dialog" aria-labelledby="catalog-export-title"
      onCancel={(event) => { if (busy) event.preventDefault(); else onClose(); }}
      onKeyDown={(event) => event.stopPropagation()}>
      <div className="session-dialog-header">
        <div><h2 id="catalog-export-title">{t("session.catalogExport.title")}</h2><p>{t("session.catalogExport.hint")}</p></div>
        <button type="button" className="session-icon-button" aria-label={t("session.dialogs.close")} disabled={busy} onClick={onClose}><X size={19} /></button>
      </div>
      <form onSubmit={(event) => { event.preventDefault(); void start(); }}>
        <fieldset className="session-field">
          <legend>{t("session.catalogExport.photos")}</legend>
          <label className="session-ingest-album"><input type="radio" name="catalog-export-scope" checked={scope === "selected"} disabled={!selected.length || busy}
            onChange={() => setScope("selected")} /> {t("session.catalogExport.selected", { count: selected.length })}</label>
          <label className="session-ingest-album"><input type="radio" name="catalog-export-scope" checked={scope === "shown"} disabled={!shown.length || busy}
            onChange={() => setScope("shown")} /> {t("session.catalogExport.shown", { count: shown.length })}</label>
        </fieldset>
        <label className="session-field">{t("session.transfer.destination")}
          <button type="button" className="session-path" disabled={busy} onClick={() => void choose()}><FolderOpen size={16} /><span>{parent || t("session.create.chooseParent")}</span></button>
        </label>
        <label className="session-field">{t("session.catalogExport.name")}
          <input required maxLength={180} value={folder} disabled={busy} onChange={(e) => setFolder(e.target.value)} />
        </label>
        <label className="session-ingest-album">
          <input type="checkbox" checked={includeOriginals} disabled={busy} onChange={(e) => setIncludeOriginals(e.target.checked)} />
          {t("session.catalogExport.includeOriginals")}
        </label>
        <p className="session-create-description">{t(includeOriginals ? "session.catalogExport.originalsHint" : "session.catalogExport.referenceHint")}</p>
        <label className="session-ingest-album">
          <input type="checkbox" checked={includePreviews} disabled={busy} onChange={(e) => setIncludePreviews(e.target.checked)} />
          {t("session.catalogExport.includePreviews")}
        </label>
        <p className="session-create-description">{t("session.catalogExport.previewsHint")}</p>
        <p className="session-create-description">{t("session.catalogExport.includes")}</p>
        {error && <p className="session-error" role="alert"><OperationError detail={error} /></p>}
        <div className="session-dialog-footer">
          <span>{t("session.catalogExport.sourceUnchanged")}</span>
          <button type="submit" className="session-action session-primary" disabled={!paths.length || !parent || invalidFolder || busy}>
            {t("session.catalogExport.export", { count: paths.length })}
          </button>
        </div>
      </form>
    </dialog>
  );
}
