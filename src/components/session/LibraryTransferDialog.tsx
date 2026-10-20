import { Invokes } from '../ui/AppProperties';
import { useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { join } from "@tauri-apps/api/path";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { Check, FolderOpen, LoaderCircle, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { flushBeforeQuit, registerQuitFlush } from "../../utils/quit";

export type TransferPurpose = "transfer" | "backup" | "restore";
interface Progress { phase: "copy" | "verify"; files: number; path: string }
/** Edit resources that were already missing are counted, not fatal. */
interface Receipt { photographs: number; missing_resources?: number; missing_current_resources?: number }
interface Props {
  purpose: TransferPurpose;
  directory?: string;
  name?: string;
  onClose(): void;
  onOpen(directory: string): Promise<void>;
}

export default function LibraryTransferDialog({ purpose, directory = "", name = "", onClose, onOpen }: Props) {
  const { t } = useTranslation();
  const dialog = useRef<HTMLDialogElement>(null);
  const [source, setSource] = useState(purpose === "restore" ? "" : directory);
  const [parent, setParent] = useState("");
  const [folder, setFolder] = useState(() => `${name || "Rawtonic"}-${purpose}-${new Date().toISOString().replace(/[:.]/g, "-")}`);
  const [busy, setBusy] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState("");
  const [progress, setProgress] = useState<Progress | null>(null);
  const [result, setResult] = useState<{
    directory: string; photographs: number; missingResources: number; missingCurrentResources: number;
  } | null>(null);
  const cancelRequested = useRef(false);
  const pending = useRef<Promise<void> | null>(null);
  const title = t(`session.transfer.${purpose}`);
  const invalidFolder = !folder.trim() || [".", ".."].includes(folder.trim()) || /[/\\:*?"<>|]/.test(folder);

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen<Progress>("library-copy-progress", ({ payload }) => setProgress(payload)).then((fn) => {
      if (disposed) fn(); else stop = fn;
    });
    return () => { disposed = true; stop?.(); };
  }, []);

  async function choose(set: (path: string) => void) {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === "string") set(selected);
    } catch (e) { setError(String(e)); }
  }
  async function run() {
    cancelRequested.current = false;
    setBusy(true); setError(""); setProgress(null); setCancelling(false);
    try {
      // Includes edits, metadata and multi-edit saves. A failure blocks copying.
      const failures = await flushBeforeQuit();
      if (failures.length) throw new Error(t("session.transfer.unsaved", { files: failures.join(", ") }));
      const destination = await join(parent, folder.trim());
      if (cancelRequested.current) return;
      const copy = invoke<Receipt>(Invokes.LibraryCopy, { source, destination, purpose });
      pending.current = copy.then(() => {}, () => {});
      const unregister = registerQuitFlush(async () => { await pending.current; return []; });
      try {
        const receipt = await copy;
        setResult({
          directory: destination,
          photographs: receipt.photographs,
          missingResources: receipt.missing_resources ?? 0,
          missingCurrentResources: receipt.missing_current_resources ?? 0,
        });
      } finally { unregister(); pending.current = null; }
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  async function cancel() {
    cancelRequested.current = true;
    setCancelling(true);
    if (!pending.current) return;
    try { await invoke(Invokes.SessionCancelIngest); }
    catch (e) { setError(String(e)); setCancelling(false); }
  }

  return (
    <dialog ref={dialog} className="session-dialog session-transfer-dialog" aria-labelledby="transfer-title"
      onCancel={(event) => { if (busy) event.preventDefault(); else onClose(); }}
      onKeyDown={(event) => event.stopPropagation()}>
      <div className="session-dialog-header">
        <div><h2 id="transfer-title">{title}</h2><p>{t(`session.transfer.${purpose}Hint`)}</p></div>
        <button type="button" className="session-icon-button" aria-label={t("session.dialogs.close")} disabled={busy} onClick={onClose}><X size={19} /></button>
      </div>
      <form onSubmit={(event) => { event.preventDefault(); void run(); }}>
        {result ? (
          <div className="session-transfer-result" role="status">
            <Check size={24} />
            <strong>{t("session.transfer.complete", { count: result.photographs })}</strong>
            <code>{result.directory}</code>
            <p>{t(purpose === "backup" ? "session.transfer.backupDone" : "session.transfer.transferDone")}</p>
            {result.missingResources > 0 && <p className="session-error">{t("session.transfer.missingResources", {
              files: result.missingResources, current: result.missingCurrentResources,
            })}</p>}
          </div>
        ) : (
          <>
            {purpose === "restore" ? (
              <label className="session-field">{t("session.transfer.source")}
                <button type="button" className="session-path" disabled={busy} onClick={() => void choose(setSource)}><FolderOpen size={16} /><span>{source || t("session.transfer.chooseSource")}</span></button>
              </label>
            ) : <p className="session-transfer-source">{source}</p>}
            <label className="session-field">{t("session.transfer.destination")}
              <button type="button" className="session-path" disabled={busy} onClick={() => void choose(setParent)}><FolderOpen size={16} /><span>{parent || t("session.create.chooseParent")}</span></button>
            </label>
            <label className="session-field">{t("session.transfer.folderName")}
              <input required maxLength={180} value={folder} disabled={busy} onChange={(e) => setFolder(e.target.value)} />
            </label>
            <p className="session-create-description">{t("session.transfer.includes")}</p>
            {busy && <div className="session-transfer-progress" role="status">
              <LoaderCircle size={18} className="animate-spin" />
              <span>{cancelling ? t("session.transfer.cancelling") : progress ? t(`session.transfer.${progress.phase}Progress`, { count: progress.files }) : t("session.transfer.preparing")}</span>
              {progress && <code>{progress.path}</code>}
            </div>}
          </>
        )}
        {error && <p className="session-error" role="alert">{error}</p>}
        <div className="session-dialog-footer">
          {result ? <>
            <button type="button" className="session-action" onClick={onClose}>{t("session.dialogs.close")}</button>
            {purpose !== "backup" && <button type="button" className="session-action session-primary" onClick={() => void onOpen(result.directory).then(onClose).catch((e) => setError(String(e)))}>{t("session.transfer.openCopy")}</button>}
          </> : <>
            <span>{t("session.transfer.originalUnchanged")}</span>
            {busy ? <button type="button" className="session-action" disabled={cancelling} onClick={() => void cancel()}>{t("session.progress.cancel")}</button>
              : <button type="submit" className="session-action session-primary" disabled={!source || !parent || invalidFolder}>{title}</button>}
          </>}
        </div>
      </form>
    </dialog>
  );
}
