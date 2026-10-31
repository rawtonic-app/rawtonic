import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../ui/AppProperties";
import type { FolderPlan } from "./addFolder";

function folderName(path: string) {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

/**
 * Folders › "Add Folder…" with a library open (G39 AF). In a catalog: the
 * folder's photographs are added where they are (Lightroom's Add; nothing
 * is copied or written beside them), its subfolders too unless unticked;
 * photographs the catalog holds already are skipped with a count, those
 * inside another library's folder are refused with that library's name.
 * In a session: the folder is only shown (a session keeps its own files),
 * with "Copy into this session…" (the verified copy import) beside it.
 */
export default function AddFolderDialog({
  folder,
  directory,
  catalog,
  onAdd,
  onShow,
  onCopy,
  onClose,
}: {
  folder: string;
  directory: string;
  catalog: boolean;
  onAdd: (files: string[]) => void;
  onShow: () => void;
  onCopy: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const form = useRef<HTMLFormElement>(null);
  const [subfolders, setSubfolders] = useState(true);
  const [plan, setPlan] = useState<{ subfolders: boolean; plan: FolderPlan } | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    form.current?.focus();
    return () => before?.focus?.();
  }, []);

  useEffect(() => {
    if (!catalog) return;
    let current = true;
    setFailure(null);
    invoke<FolderPlan>(Invokes.CatalogAddFolderScan, { directory, folder, subfolders })
      .then((next) => current && setPlan({ subfolders, plan: next }))
      .catch((error) => current && setFailure(String(error)));
    return () => {
      current = false;
    };
  }, [catalog, directory, folder, subfolders]);

  const shown = plan?.subfolders === subfolders ? plan.plan : null;
  const name = folderName(folder);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    // Nothing left to add (all in the catalog already): the folder is shown.
    if (!catalog || (shown && !shown.files.length)) onShow();
    else if (shown) onAdd(shown.files);
  };
  const keys = (e: KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "Escape") onClose();
  };
  const title = catalog ? t("session.addFolder.title", { name }) : t("session.addFolder.sessionTitle", { name });

  return createPortal(
    <div className="studio-settings studio-smart-album-backdrop" onKeyDown={keys}>
      <form
        ref={form}
        className="studio-capture-time studio-add-folder"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onSubmit={submit}
      >
        <h2>{title}</h2>
        <p className="studio-help studio-add-folder-path" title={folder}>
          {folder}
        </p>
        {catalog ? (
          <>
            <p className="studio-help">{t("session.addFolder.intro")}</p>
            <label className="studio-auto-stack-camera">
              <input type="checkbox" checked={subfolders} onChange={(e) => setSubfolders(e.target.checked)} />
              <span>{t("session.addFolder.subfolders")}</span>
            </label>
            <div className="studio-capture-time-preview" role="status" aria-live="polite">
              {shown ? (
                <>
                  <p>
                    {!shown.files.length
                      ? t("session.addFolder.nothing")
                      : shown.photographs
                        ? t("session.addFolder.toAdd", { count: shown.photographs })
                        : t("session.addFolder.nothingNew")}
                  </p>
                  {shown.companions > 0 && (
                    <p className="studio-help">{t("session.addFolder.companions", { count: shown.companions })}</p>
                  )}
                  {shown.unreadable.length > 0 && (
                    <p className="studio-help" title={shown.unreadable.join("\n")}>
                      {t("session.addFolder.unreadable", {
                        count: shown.unreadable.length,
                        first: shown.unreadable[0],
                      })}
                    </p>
                  )}
                  {shown.registered > 0 && (
                    <p className="studio-help">{t("session.addFolder.registered", { count: shown.registered })}</p>
                  )}
                  {shown.owned.map((owner) => (
                    <p key={owner.library} className="studio-help" title={owner.library}>
                      {t("session.addFolder.owned", { count: owner.count, name: owner.name })}
                    </p>
                  ))}
                  {shown.truncated && (
                    <p className="studio-help">
                      {t("session.addFolder.truncated", { n: shown.files.length.toLocaleString() })}
                    </p>
                  )}
                </>
              ) : (
                !failure && <p>{t("session.addFolder.reading")}</p>
              )}
            </div>
          </>
        ) : (
          <p className="studio-help">{t("session.addFolder.sessionNote")}</p>
        )}
        {failure && (
          <p className="studio-capture-time-error" role="alert">
            {t("session.addFolder.failed", { error: failure })}
          </p>
        )}
        <footer>
          <button type="button" onClick={onClose}>
            {t("session.addFolder.cancel")}
          </button>
          {catalog ? (
            <button type="submit" className="is-primary" disabled={!shown}>
              {shown && !shown.files.length
                ? t("session.addFolder.show")
                : shown && !shown.photographs
                  ? t("session.addFolder.addCompanions")
                  : t("session.addFolder.add", { count: shown?.photographs ?? 0 })}
            </button>
          ) : (
            <>
              <button type="button" onClick={onCopy}>
                {t("session.addFolder.copy")}
              </button>
              <button type="submit" className="is-primary">
                {t("session.addFolder.show")}
              </button>
            </>
          )}
        </footer>
      </form>
    </div>,
    document.body,
  );
}
