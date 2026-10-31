// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Library › Import from Catalog… (G39): the work done in another Rawtonic
 * catalog — typically one made by Export as Catalog and taken to another
 * computer — comes back into the open catalog. Lightroom Classic's Import
 * from Another Catalog and Capture One's Import Catalog are the models.
 *
 * Photographs are matched by their asset id, else the SHA-256 of their
 * original, never by name. The review lists new photographs (added where
 * they are, or copied to a chosen folder) and changed ones, with one choice
 * for the run and one per photo: Replace edits and metadata (the default;
 * the replaced edit stays in the photo's history), Keep current (the
 * default for photos only this catalog changed) or Import as virtual copy.
 * Unchanged photos are skipped and counted. The import runs in the
 * background with progress and Cancel in the activity centre.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
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

export type ImportChoice = "replace" | "keep" | "virtualCopy";
/** What differs between the two catalogs for a changed photo. */
export type ImportChange = "edit" | "copies" | "metadata" | "captureTime" | "eyes" | "snapshots" | "albums";
export interface IncomingPhoto {
  id: string;
  target: string | null;
  matchedBy: "id" | "sha256" | null;
  status: "new" | "changed";
  changes: ImportChange[];
  changedHereOnly: boolean;
  path: string;
  available: boolean;
}
export interface CatalogReview { name: string; photos: IncomingPhoto[]; new: number; changed: number; unchanged: number; leftOut: number }
export interface CatalogImportOptions {
  newPhotos: { mode: "reference" } | { mode: "copy"; folder: string };
  changed: ImportChoice | null;
  overrides: Record<string, ImportChoice>;
  includeNew: boolean;
}
interface Receipt {
  added: number; replaced: number; virtualCopies: number; kept: number; albumsCreated: number;
  unchanged: number; originalsCopied: number; offline: number; missingResources: number; leftOut: number;
}
interface Progress { phase: "originals" | "applying"; done: number; total: number }

export const CATALOG_IMPORT_ACTIVITY = "catalog-import";
const CHOICES: ImportChoice[] = ["replace", "keep", "virtualCopy"];

/** Runs the import with progress, Cancel and its result in the activity
 * centre; `onDone` refreshes the open catalog. */
export async function runCatalogImport(target: string, other: string, name: string, options: CatalogImportOptions,
  onDone: () => Promise<void>) {
  const t = i18n.t.bind(i18n);
  const id = useActivityStore.getState().start({
    id: CATALOG_IMPORT_ACTIVITY,
    source: "import",
    title: t("session.catalogImport.running", { name }),
    cancel: () => {
      useActivityStore.getState().update(CATALOG_IMPORT_ACTIVITY, { title: t("session.catalogImport.cancelling"), cancel: undefined });
      return invoke(Invokes.LibraryImportCatalogCancel);
    },
  });
  let stop: (() => void) | undefined;
  if (isTauri()) {
    stop = await listen<Progress>("catalog-import-progress", ({ payload }) => {
      useActivityStore.getState().update(id, {
        progress: { done: payload.done, total: payload.total },
        detail: t(payload.phase === "originals" ? "session.catalogImport.copying" : "session.catalogImport.applying"),
      });
    });
  }
  const run = invoke<Receipt>(Invokes.LibraryImportCatalog, { target, other, options });
  const unregister = registerQuitFlush(async () => { await run.catch(() => {}); return []; });
  try {
    const receipt = await run;
    useActivityStore.getState().resolve(id);
    const detail = [
      t("session.catalogImport.added", { count: receipt.added }),
      t("session.catalogImport.replaced", { count: receipt.replaced }),
      receipt.virtualCopies ? t("session.catalogImport.virtualCopies", { count: receipt.virtualCopies }) : "",
      receipt.kept ? t("session.catalogImport.kept", { count: receipt.kept }) : "",
      t("session.catalogImport.unchangedSkipped", { count: receipt.unchanged }),
      receipt.originalsCopied ? t("session.catalogExport.originalsCopied", { count: receipt.originalsCopied }) : "",
      receipt.offline ? t("session.catalogExport.offline", { count: receipt.offline }) : "",
      receipt.leftOut ? t("session.catalogExport.leftOut", { count: receipt.leftOut }) : "",
    ].filter(Boolean).join("\n");
    notify.success(t("session.catalogImport.done", { name }), { source: "import", toastId: CATALOG_IMPORT_ACTIVITY, detail });
    await onDone();
  } catch (e) {
    const message = String(e);
    useActivityStore.getState().resolve(id);
    if (/cancelled/i.test(message)) notify.info(t("session.catalogImport.cancelled", { name }), { source: "import" });
    else notify.error(t("session.catalogImport.failed", { name }), { source: "import", detail: message });
  } finally {
    unregister();
    stop?.();
  }
}

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

interface Props {
  /** The open catalog. */
  directory: string;
  onClose(): void;
  onDone(): Promise<void>;
}

export default function CatalogImportDialog({ directory, onClose, onDone }: Props) {
  const { t } = useTranslation();
  const dialog = useRef<HTMLDialogElement>(null);
  const [other, setOther] = useState("");
  const [review, setReview] = useState<CatalogReview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [includeNew, setIncludeNew] = useState(true);
  const [copyTo, setCopyTo] = useState<string | null>(null);
  const [copyMode, setCopyMode] = useState(false);
  const [runChoice, setRunChoice] = useState<ImportChoice>("replace");
  const [overrides, setOverrides] = useState<Record<string, ImportChoice>>({});
  const fresh = useMemo(() => review?.photos.filter((p) => p.status === "new") ?? [], [review]);
  const changed = useMemo(() => review?.photos.filter((p) => p.status === "changed") ?? [], [review]);

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  async function chooseCatalog() {
    setError("");
    try {
      const chosen = await openDialog({ directory: true, multiple: false, title: t("session.catalogImport.choose") });
      if (typeof chosen !== "string") return;
      setBusy(true);
      setOther(chosen);
      const found = await invoke<CatalogReview>(Invokes.LibraryImportCatalogReview, { target: directory, other: chosen });
      setReview(found);
      // Photos only this catalog changed since: keep what is here.
      setOverrides(Object.fromEntries(found.photos.filter((p) => p.changedHereOnly).map((p) => [p.id, "keep" as const])));
    } catch (e) {
      setReview(null);
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function chooseCopyFolder() {
    try {
      const chosen = await openDialog({ directory: true, multiple: false, title: t("session.catalogImport.copyTo") });
      if (typeof chosen === "string") { setCopyTo(chosen); setCopyMode(true); }
    } catch (e) { setError(String(e)); }
  }

  async function start() {
    if (!review) return;
    setBusy(true); setError("");
    try {
      // The import writes edits and metadata: everything typed so far is saved first.
      const failures = await flushBeforeQuit();
      if (failures.length) throw new Error(t("session.transfer.unsaved", { files: failures.join(", ") }));
      const options: CatalogImportOptions = {
        newPhotos: copyMode && copyTo ? { mode: "copy", folder: copyTo } : { mode: "reference" },
        changed: runChoice,
        overrides,
        includeNew,
      };
      void runCatalogImport(directory, other, review.name, options, onDone);
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }

  const nothing = review !== null && (includeNew ? fresh.length : 0) + changed.length === 0;
  const changeNames = (photo: IncomingPhoto) =>
    photo.changes.map((change) => t(`session.catalogImport.change.${change}`)).join(", ");

  return (
    <dialog ref={dialog} className="session-dialog session-transfer-dialog catalog-import-dialog" aria-labelledby="catalog-import-title"
      onCancel={(event) => { if (busy) event.preventDefault(); else onClose(); }}
      onKeyDown={(event) => event.stopPropagation()}>
      <div className="session-dialog-header">
        <div><h2 id="catalog-import-title">{t("session.catalogImport.title")}</h2><p>{t("session.catalogImport.hint")}</p></div>
        <button type="button" className="session-icon-button" aria-label={t("session.dialogs.close")} disabled={busy} onClick={onClose}><X size={19} /></button>
      </div>
      <form onSubmit={(event) => { event.preventDefault(); void start(); }}>
        <label className="session-field">{t("session.catalogImport.source")}
          <button type="button" className="session-path" disabled={busy} onClick={() => void chooseCatalog()}>
            <FolderOpen size={16} /><span>{other || t("session.catalogImport.choose")}</span>
          </button>
        </label>
        {review && (
          <>
            <p className="session-create-description" role="status">
              {t("session.catalogImport.summary", { name: review.name, new: review.new, changed: review.changed, unchanged: review.unchanged })}
              {review.leftOut > 0 && <> {t("session.catalogExport.leftOut", { count: review.leftOut })}</>}
            </p>
            {fresh.length > 0 && (
              <fieldset className="session-field">
                <legend>{t("session.catalogImport.newPhotos", { count: fresh.length })}</legend>
                <label className="session-ingest-album">
                  <input type="checkbox" checked={includeNew} disabled={busy} onChange={(e) => setIncludeNew(e.target.checked)} />
                  {t("session.catalogImport.includeNew")}
                </label>
                <label className="session-ingest-album">
                  <input type="radio" name="catalog-import-new" checked={!copyMode} disabled={busy || !includeNew} onChange={() => setCopyMode(false)} />
                  {t("session.catalogImport.addByReference")}
                </label>
                <label className="session-ingest-album">
                  <input type="radio" name="catalog-import-new" checked={copyMode} disabled={busy || !includeNew}
                    onChange={() => { if (copyTo) setCopyMode(true); else void chooseCopyFolder(); }} />
                  {t("session.catalogImport.copyOriginals")}
                </label>
                {copyMode && copyTo && (
                  <button type="button" className="session-path" disabled={busy} onClick={() => void chooseCopyFolder()}>
                    <FolderOpen size={16} /><span>{copyTo}</span>
                  </button>
                )}
                <ul className="catalog-import-list">
                  {fresh.map((photo) => (
                    <li key={photo.id}><span title={photo.path}>{fileName(photo.path)}</span>
                      {!photo.available && <small>{t("session.catalogImport.offline")}</small>}</li>
                  ))}
                </ul>
              </fieldset>
            )}
            {changed.length > 0 && (
              <fieldset className="session-field">
                <legend>{t("session.catalogImport.changedPhotos", { count: changed.length })}</legend>
                <label className="session-field">{t("session.catalogImport.forChanged")}
                  <select value={runChoice} disabled={busy} onChange={(e) => {
                    setRunChoice(e.target.value as ImportChoice);
                    setOverrides({});
                  }}>
                    {CHOICES.map((choice) => <option key={choice} value={choice}>{t(`session.catalogImport.choice.${choice}`)}</option>)}
                  </select>
                </label>
                <ul className="catalog-import-list">
                  {changed.map((photo) => (
                    <li key={photo.id}>
                      <span title={photo.path}>{fileName(photo.path)}</span>
                      <small>{changeNames(photo)}{photo.changedHereOnly ? ` · ${t("session.catalogImport.changedHereOnly")}` : ""}</small>
                      <select aria-label={t("session.catalogImport.choiceFor", { name: fileName(photo.path) })}
                        value={overrides[photo.id] ?? runChoice} disabled={busy}
                        onChange={(e) => setOverrides({ ...overrides, [photo.id]: e.target.value as ImportChoice })}>
                        {CHOICES.map((choice) => <option key={choice} value={choice}>{t(`session.catalogImport.choice.${choice}`)}</option>)}
                      </select>
                    </li>
                  ))}
                </ul>
              </fieldset>
            )}
            {nothing && <p className="session-create-description">{t("session.catalogImport.nothing")}</p>}
          </>
        )}
        {error && <p className="session-error" role="alert"><OperationError detail={error} /></p>}
        <div className="session-dialog-footer">
          <span>{t("session.catalogImport.otherUnchanged")}</span>
          <button type="submit" className="session-action session-primary" disabled={!review || nothing || busy || (copyMode && !copyTo)}>
            {t("session.catalogImport.import")}
          </button>
        </div>
      </form>
    </dialog>
  );
}
