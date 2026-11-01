import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../ui/AppProperties";
import { openDialog } from "../../utils/fileDialog";
import { FOLDER_ROLES, moveFolderContents, type FolderRole, type SessionFolder } from "../../utils/sessionFolders";

/** What `session_set_folder` returns. */
export interface FolderChange {
  folder: SessionFolder;
  /** The folder it was before. */
  previous: SessionFolder;
}

/**
 * Session › Session Folders… (G38, Capture One's session folders): where
 * captures and imports go (Capture), where "Move to Selects" moves photos
 * (Selects) and where exports go by default (Output). Each can be any
 * folder inside the session (it moves with the session) or outside it (a
 * capture drive, a shared folder). Choosing a folder moves no files unless
 * "Move contents" is ticked: then the old folder's files move into the new
 * one with the journaled, undoable file operations. A folder whose drive
 * is not connected is shown as such; capture, import and export into it
 * fail with its name instead of writing elsewhere.
 */
export default function SessionFoldersDialog({
  directory,
  onChanged,
  onClose,
}: {
  directory: string;
  /** After a folder changed (the summary and the Folders panel follow). */
  onChanged: (change: FolderChange) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const roleName: Record<FolderRole, string> = {
    capture: t("session.folders.role.capture"),
    selects: t("session.folders.role.selects"),
    output: t("session.folders.role.output"),
  };
  const whenAway: Record<FolderRole, string> = {
    capture: t("session.folders.unavailableUse.capture"),
    selects: t("session.folders.unavailableUse.selects"),
    output: t("session.folders.unavailableUse.output"),
  };
  const dialog = useRef<HTMLDivElement>(null);
  const [folders, setFolders] = useState<SessionFolder[] | null>(null);
  const [moveContents, setMoveContents] = useState(false);
  const [busy, setBusy] = useState<FolderRole | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    dialog.current?.focus();
    return () => before?.focus?.();
  }, []);

  useEffect(() => {
    let live = true;
    invoke<SessionFolder[]>(Invokes.SessionFolders, { directory })
      .then((next) => live && setFolders(next))
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
    };
  }, [directory]);

  const choose = async (role: FolderRole) => {
    const current = folders?.find((f) => f.role === role);
    const picked = await openDialog({
      directory: true,
      multiple: false,
      defaultPath: current?.available ? current.path : directory,
      title: t("session.folders.choose", { folder: roleName[role] }),
    });
    if (typeof picked !== "string") return;
    setBusy(role);
    setError(null);
    setNotice(null);
    try {
      const change = await invoke<FolderChange>(Invokes.SessionSetFolder, {
        directory,
        role,
        path: picked,
        moveContents,
      });
      setFolders((list) => (list ?? []).map((f) => (f.role === role ? change.folder : f)));
      onChanged(change);
      // The folder changed; its old contents move after, each part an
      // undoable file operation (nothing moves without the option).
      if (moveContents && change.previous.available && change.previous.path !== change.folder.path) {
        const moved = await moveFolderContents(change.previous.path, change.folder.path);
        if (moved.error) setError(t("session.folders.moveFailed", { error: moved.error }));
        else if (moved.items) setNotice(t("session.folders.moved", { count: moved.items }));
        onChanged(change);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const keys = (e: KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "Escape") onClose();
  };
  const title = t("session.folders.title");

  return createPortal(
    <div className="studio-settings studio-smart-album-backdrop" onKeyDown={keys}>
      <div
        ref={dialog}
        className="studio-capture-time studio-session-folders"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
      >
        <h2>{title}</h2>
        <p className="studio-help">{t("session.folders.intro")}</p>
        {folders ? (
          <dl className="studio-session-folders-list">
            {FOLDER_ROLES.map((role) => {
              const folder = folders.find((f) => f.role === role);
              if (!folder) return null;
              const label = roleName[role];
              return (
                <div key={role} className="studio-session-folder" data-role={role}>
                  <dt>{label}</dt>
                  <dd>
                    <span className="studio-add-folder-path" title={folder.path}>
                      {folder.path}
                    </span>
                    <span className="studio-help">
                      {!folder.available
                        ? t("session.folders.unavailable")
                        : folder.inside
                          ? t("session.folders.inside")
                          : t("session.folders.outside")}
                    </span>
                    {!folder.available && (
                      <span className="studio-capture-time-error" role="alert">
                        {whenAway[role]}
                      </span>
                    )}
                    <button
                      type="button"
                      disabled={busy !== null}
                      aria-label={t("session.folders.chooseFor", { folder: label })}
                      onClick={() => void choose(role)}
                    >
                      {busy === role ? t("session.folders.working") : t("session.folders.change")}
                    </button>
                  </dd>
                </div>
              );
            })}
          </dl>
        ) : (
          !error && <p>{t("session.folders.reading")}</p>
        )}
        <label className="studio-auto-stack-camera">
          <input
            type="checkbox"
            checked={moveContents}
            disabled={busy !== null}
            onChange={(e) => setMoveContents(e.target.checked)}
          />
          <span>{t("session.folders.moveContents")}</span>
        </label>
        <p className="studio-help">
          {moveContents ? t("session.folders.moveContentsHelp") : t("session.folders.nothingMoves")}
        </p>
        {notice && (
          <p className="studio-help" role="status">
            {notice}
          </p>
        )}
        {error && (
          <p className="studio-capture-time-error" role="alert">
            {error}
          </p>
        )}
        <footer>
          <button type="button" className="is-primary" onClick={onClose} disabled={busy !== null}>
            {t("session.folders.done")}
          </button>
        </footer>
      </div>
    </div>,
    document.body,
  );
}
