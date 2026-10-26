import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { LoaderCircle, Undo2, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import OperationError from "../ui/OperationError";
import { Invokes } from "../ui/AppProperties";
import { xmpCounts, type XmpReport, type XmpReviewEntry, type XmpUndoStep } from "../../utils/xmpReview";

interface Props {
  /** The open library. */
  directory: string;
  /** Preferences' XMP Sync; off, the dialog says what it still covers. */
  syncEnabled: boolean;
  onClose(): void;
  /** The photographs to review, after the first check and after each change. */
  onEntries(entries: XmpReviewEntry[], first: boolean): void;
}

/** Rows a conflict list shows at most; "for all" still covers every one. */
const SHOWN = 300;

const fileName = (path: string) => path.slice(Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\")) + 1);

/**
 * The session bar's XMP review: how many sidecars are pending (the library
 * is newer) and how many changed outside Rawtonic (conflicts). "Sync now"
 * writes the pending ones; a conflict is resolved per photograph or for all,
 * keeping the library's metadata or reading the sidecar's (undoable). Only
 * XMP sidecars are written, never the photographs.
 */
export default function XmpSyncDialog({ directory, syncEnabled, onClose, onEntries }: Props) {
  const { t } = useTranslation();
  const dialog = useRef<HTMLDialogElement>(null);
  const [entries, setEntries] = useState<XmpReviewEntry[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<XmpReport | null>(null);
  // Every Use XMP of this dialog not undone yet: kept apart from the last
  // report, so a later Sync now (retrying a failed write-back) keeps them.
  const [undoSteps, setUndoSteps] = useState<XmpUndoStep[]>([]);
  const [error, setError] = useState("");
  const entriesRef = useRef(onEntries);
  entriesRef.current = onEntries;

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const load = useCallback(
    async (first: boolean) => {
      const listed = (await invoke<XmpReviewEntry[]>(Invokes.SessionXmpReview, { directory })) ?? [];
      if (!mounted.current) return;
      setEntries(listed);
      entriesRef.current(listed, first);
    },
    [directory],
  );
  // One check per dialog (React's development double effects included).
  const started = useRef(false);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    load(true).catch((e) => mounted.current && setError(String(e)));
  }, [load]);

  const run = async (work: () => Promise<XmpReport>, undo = false) => {
    setBusy(true);
    setError("");
    try {
      const result = await work();
      if (!mounted.current) return;
      setReport(result);
      // Steps an undo could not take back stay for another try.
      if (undo) setUndoSteps(result.undo);
      else if (result.undo.length) setUndoSteps((steps) => [...steps, ...result.undo]);
      await load(false);
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      if (mounted.current) setBusy(false);
    }
  };
  const syncNow = () => run(() => invoke<XmpReport>(Invokes.SessionXmpSync, { directory }));
  const resolve = (paths: string[], keep: "library" | "xmp") =>
    run(() => invoke<XmpReport>(Invokes.SessionXmpResolve, { directory, paths, keep }));
  const undo = (steps: XmpUndoStep[]) => run(() => invoke<XmpReport>(Invokes.SessionXmpUndo, { directory, steps }), true);

  const counts = entries ? xmpCounts(entries) : null;
  const conflicts = entries?.filter((entry) => entry.state === "conflict") ?? [];
  const readable = conflicts.filter((entry) => !entry.edit_not_adopted);
  const writable = counts ? counts.pending - counts.offline : 0;

  const outcome = (result: XmpReport) => (
    <div className="session-xmp-result" role="status">
      <p>
        {t("session.xmp.result", {
          done: result.done.length,
          offline: result.offline.length,
          conflicts: result.conflicts.length,
          failed: result.failed.length,
        })}
      </p>
      {(result.edit_not_adopted?.length ?? 0) > 0 && (
        <p>{t("session.xmp.leftOut", { count: result.edit_not_adopted.length })}</p>
      )}
      {result.failed.length > 0 && (
        <ul className="session-sync-skipped">
          {result.failed.map((failure) => (
            <li key={failure.path} title={failure.path}>
              <span>{fileName(failure.path)}</span>
              <small>{failure.reason}</small>
            </li>
          ))}
        </ul>
      )}
    </div>
  );

  return (
    <dialog
      ref={dialog}
      className="session-dialog session-xmp-dialog"
      aria-labelledby="xmp-title"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onClose();
      }}
      onKeyDown={(event) => event.stopPropagation()}
    >
      <div className="session-dialog-header">
        <div>
          <h2 id="xmp-title">{t("session.xmp.title")}</h2>
          <p>{t("session.xmp.subtitle")}</p>
        </div>
        <button
          type="button"
          className="session-icon-button"
          aria-label={t("session.dialogs.close")}
          disabled={busy}
          onClick={onClose}
        >
          <X size={19} />
        </button>
      </div>
      <div className="session-xmp-body">
        {!syncEnabled && <p className="session-xmp-note">{t("session.xmp.prefsOff")}</p>}
        {!entries && !error && (
          <div className="session-transfer-progress" role="status">
            <LoaderCircle size={18} className="animate-spin" />
            <span>{t("session.xmp.loading")}</span>
          </div>
        )}
        {counts && entries?.length === 0 && <p className="session-sync-summary">{t("session.xmp.inSync")}</p>}
        {counts && counts.pending > 0 && (
          <section className="session-xmp-group" aria-labelledby="xmp-pending">
            <div className="session-xmp-group-header">
              <div>
                <h3 id="xmp-pending">{t("session.xmp.pendingHeading", { count: counts.pending })}</h3>
                <p>{t("session.xmp.pendingHint")}</p>
                {counts.offline > 0 && (
                  <p className="session-xmp-note">{t("session.xmp.offlineNote", { count: counts.offline })}</p>
                )}
              </div>
              <button
                type="button"
                className="session-action session-primary"
                disabled={busy || writable === 0}
                onClick={() => void syncNow()}
              >
                {t("session.xmp.syncNow")}
              </button>
            </div>
          </section>
        )}
        {counts && counts.conflicts > 0 && (
          <section className="session-xmp-group" aria-labelledby="xmp-conflicts">
            <div className="session-xmp-group-header">
              <div>
                <h3 id="xmp-conflicts">{t("session.xmp.conflictHeading", { count: counts.conflicts })}</h3>
                <p>{t("session.xmp.conflictHint")}</p>
              </div>
              <span className="session-sync-buttons">
                <button
                  type="button"
                  className="session-action"
                  disabled={busy}
                  title={t("session.xmp.keepLibraryTitle")}
                  onClick={() => void resolve(conflicts.map((entry) => entry.path), "library")}
                >
                  {t("session.xmp.keepLibraryAll")}
                </button>
                <button
                  type="button"
                  className="session-action"
                  disabled={busy || readable.length === 0}
                  title={t("session.xmp.useXmpTitle")}
                  onClick={() => void resolve(readable.map((entry) => entry.path), "xmp")}
                >
                  {t("session.xmp.useXmpAll")}
                </button>
              </span>
            </div>
            <ul className="session-xmp-list">
              {conflicts.slice(0, SHOWN).map((entry) => {
                const name = fileName(entry.path);
                return (
                  <li key={entry.path} title={entry.path}>
                    <span>{name}</span>
                    {entry.offline && <small>{t("session.xmp.offline")}</small>}
                    {entry.edit_not_adopted && <small>{t("session.xmp.editNotAdopted")}</small>}
                    <button
                      type="button"
                      className="session-action"
                      disabled={busy || entry.offline}
                      aria-label={t("session.xmp.keepLibraryFor", { name })}
                      title={t("session.xmp.keepLibraryTitle")}
                      onClick={() => void resolve([entry.path], "library")}
                    >
                      {t("session.xmp.keepLibrary")}
                    </button>
                    <button
                      type="button"
                      className="session-action"
                      disabled={busy || entry.offline || entry.edit_not_adopted}
                      aria-label={t("session.xmp.useXmpFor", { name })}
                      title={t(entry.edit_not_adopted ? "session.xmp.editNotAdopted" : "session.xmp.useXmpTitle")}
                      onClick={() => void resolve([entry.path], "xmp")}
                    >
                      {t("session.xmp.useXmp")}
                    </button>
                  </li>
                );
              })}
              {conflicts.length > SHOWN && <li>{t("session.xmp.more", { count: conflicts.length - SHOWN })}</li>}
            </ul>
          </section>
        )}
        {report && outcome(report)}
        {error && (
          <p className="session-error" role="alert">
            <OperationError detail={error} />
          </p>
        )}
      </div>
      <div className="session-dialog-footer">
        <span>{t("session.xmp.originalsUntouched")}</span>
        <span className="session-sync-buttons">
          {undoSteps.length > 0 && (
            <button type="button" className="session-action" disabled={busy} onClick={() => void undo(undoSteps)}>
              <Undo2 size={14} />
              <span>{t("session.xmp.undo", { count: undoSteps.length })}</span>
            </button>
          )}
          <button type="button" className="session-action" disabled={busy} onClick={onClose}>
            {t("session.dialogs.close")}
          </button>
        </span>
      </div>
    </dialog>
  );
}
