// SPDX-License-Identifier: AGPL-3.0-or-later
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Invokes } from "../ui/AppProperties";
import { notify } from "../../activity/notify";
import { registerAppCommands } from "../../utils/appCommands";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useProcessStore } from "../../store/useProcessStore";
import {
  DUPLICATES_FINISHED,
  DUPLICATES_PROGRESS,
  cancelDuplicatesPass,
  confirmDuplicates,
  duplicateGroups,
  hasOwnWork,
  startDuplicatesPass,
  suggestedKeep,
  useDuplicatesDialog,
  type DuplicateGroup,
  type DuplicateMember,
  type DuplicatesFinished,
  type DuplicatesProgress,
} from "../../utils/duplicates";

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;
const folderOf = (path: string) => {
  const at = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return at > 0 ? path.slice(0, at) : path;
};

/** An import time (UTC, SQLite form) in the viewer's local time. */
const localTime = (utc: string) => {
  const date = new Date(`${utc.replace(" ", "T")}Z`);
  return Number.isNaN(date.getTime()) ? utc : date.toLocaleString();
};

type Trash = (paths: string[], options: { includeAssociated: boolean }) => Promise<void>;

/** Registers File › Find Duplicates… and shows the review when asked. */
export function DuplicatesDialogHost({ trash }: { trash: Trash }) {
  const open = useDuplicatesDialog((s) => s.open);
  const directory = useLibraryStore((s) => s.sessionDirectory);
  const { t } = useTranslation();
  useEffect(
    () =>
      registerAppCommands({
        find_duplicates: {
          enabled: () => !!useLibraryStore.getState().sessionDirectory,
          disabledHint: () => t("duplicates.noLibrary"),
          run: () => useDuplicatesDialog.getState().show(),
        },
      }),
    [t],
  );
  if (!open) return null;
  return <DuplicatesDialog key={directory ?? ""} directory={directory} trash={trash} />;
}

type Phase =
  | { kind: "reading"; run: number | null; done: number; total: number }
  | { kind: "review"; groups: DuplicateGroup[]; finished: DuplicatesFinished | null }
  | { kind: "failed"; error: string };

/**
 * Find Duplicates' review: reads what is not compared yet (Stop keeps what
 * was read), then lists each group of photographs with identical image
 * data. The user marks per photograph whether it goes to the Trash; the
 * suggestion keeps every photograph with work of its own, else the oldest
 * import. Move to Trash checks the files again, then trashes only the
 * marked photographs, one undoable step; nothing is merged.
 */
function DuplicatesDialog({ directory, trash }: { directory: string | null; trash: Trash }) {
  const { t } = useTranslation();
  const close = useDuplicatesDialog((s) => s.close);
  const dialog = useRef<HTMLDivElement>(null);
  const [phase, setPhase] = useState<Phase>({ kind: "reading", run: null, done: 0, total: 0 });
  const [trashed, setTrashed] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const thumbnails = useProcessStore((s) => s.thumbnails);

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    dialog.current?.focus();
    return () => before?.focus?.();
  }, []);

  const load = useCallback(
    (finished: DuplicatesFinished | null) => {
      if (!directory) return;
      duplicateGroups(directory)
        .then((groups) => {
          const marked = new Set<string>();
          for (const group of groups) {
            const keep = suggestedKeep(group);
            for (const member of group) if (!keep.has(member.path)) marked.add(member.path);
          }
          setTrashed(marked);
          setPhase({ kind: "review", groups, finished });
        })
        .catch((error) => setPhase({ kind: "failed", error: String(error) }));
    },
    [directory],
  );

  // Read what is not compared yet, then show the groups. Events are
  // matched by the pass's number (the backend names the library by its
  // real path); one that ends before its number is known waits for it.
  useEffect(() => {
    if (!directory) return;
    let current = true;
    let run: number | null = null;
    const early = new Map<number, DuplicatesFinished>();
    const finished = (payload: DuplicatesFinished) => {
      if (payload.error) setPhase({ kind: "failed", error: payload.error });
      else load(payload);
    };
    const stops = [
      listen<DuplicatesProgress>(DUPLICATES_PROGRESS, ({ payload }) => {
        if (current && payload.run === run)
          setPhase({ kind: "reading", run: payload.run, done: payload.done, total: payload.total });
      }),
      listen<DuplicatesFinished>(DUPLICATES_FINISHED, ({ payload }) => {
        if (!current) return;
        if (run === null) early.set(payload.run, payload);
        else if (payload.run === run) finished(payload);
      }),
    ];
    Promise.all(stops)
      .then(() => startDuplicatesPass(directory))
      .then((started) => {
        if (!current) return;
        run = started;
        const ended = early.get(started);
        if (ended) finished(ended);
        else setPhase((phase) => (phase.kind === "reading" ? { ...phase, run: started } : phase));
      })
      .catch((error) => current && setPhase({ kind: "failed", error: String(error) }));
    return () => {
      current = false;
      for (const stop of stops) void stop.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [directory, load, attempt]);

  const groups = useMemo(() => (phase.kind === "review" ? phase.groups : []), [phase]);
  // Photographs shown here whose thumbnails the grid has not made.
  useEffect(() => {
    const missing = groups.flat().map((m) => m.path).filter((path) => !useProcessStore.getState().thumbnails[path]);
    if (missing.length) void invoke(Invokes.UpdateThumbnailQueue, { paths: missing.slice(0, 500) }).catch(() => {});
  }, [groups]);

  const keepsNone = groups.filter((group) => group.every((m) => trashed.has(m.path)));
  const toTrash = groups.flat().filter((m) => trashed.has(m.path));
  const blocked = busy || toTrash.length === 0 || keepsNone.length > 0;

  const toggle = (member: DuplicateMember) =>
    setTrashed((marked) => {
      const next = new Set(marked);
      if (next.has(member.path)) next.delete(member.path);
      else next.add(member.path);
      return next;
    });

  const moveToTrash = async () => {
    if (blocked || !directory) return;
    setBusy(true);
    setProblem(null);
    const marked = toTrash.map((m) => m.path);
    const keep = groups.flat().filter((m) => !trashed.has(m.path)).map((m) => m.path);
    try {
      const changed = await confirmDuplicates(directory, keep, marked);
      if (changed.length) {
        // Compared again: what changed is not offered on old data.
        setProblem(t("duplicates.changed", { count: changed.length }));
        setBusy(false);
        setAttempt((n) => n + 1);
        setPhase({ kind: "reading", run: null, done: 0, total: 0 });
        return;
      }
      close();
      await trash(marked, { includeAssociated: false });
    } catch (error) {
      const text = t("duplicates.failed", { error: String(error) });
      setProblem(text);
      notify.error(text);
      setBusy(false);
    }
  };

  const keys = (e: KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "Escape") close();
  };
  const title = t("duplicates.title");
  const reading = phase.kind === "reading" ? phase : null;
  const finished = phase.kind === "review" ? phase.finished : null;

  const memberRow = (member: DuplicateMember, at: string) => {
    const marked = trashed.has(member.path);
    const badges = [
      member.edited && t("duplicates.badges.edited"),
      member.rating > 0 && "★".repeat(member.rating),
      member.flag === 1 && t("duplicates.badges.picked"),
      member.flag === -1 && t("duplicates.badges.rejected"),
      member.label && t("duplicates.badges.label", { label: member.label }),
      member.keywords > 0 && t("duplicates.badges.keywords", { count: member.keywords }),
      member.copies > 0 && t("duplicates.badges.copies", { count: member.copies }),
    ].filter(Boolean);
    return (
      <li key={member.path} className={marked ? "is-trashed" : undefined}>
        <span className="studio-duplicates-thumb">{thumbnails[member.path] ? <img src={thumbnails[member.path]} alt="" /> : null}</span>
        <span className="studio-duplicates-facts" id={`duplicates-${at}`}>
          <strong>{fileName(member.path)}</strong>
          <small title={member.path}>{folderOf(member.path)}</small>
          <small>
            {t("duplicates.captured", { date: member.captured ?? t("duplicates.unknown") })} ·{" "}
            {t("duplicates.imported", { date: member.imported ? localTime(member.imported) : t("duplicates.unknown") })}
          </small>
          <small>{badges.length ? badges.join(" · ") : t("duplicates.badges.none")}</small>
        </span>
        <label className="studio-duplicates-choice">
          <input
            type="checkbox"
            checked={marked}
            aria-describedby={`duplicates-${at}`}
            onChange={() => toggle(member)}
          />
          {t("duplicates.markTrash")}
        </label>
      </li>
    );
  };

  return createPortal(
    <div className="studio-settings studio-smart-album-backdrop" onKeyDown={keys}>
      <div
        ref={dialog}
        className="studio-capture-time studio-duplicates"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
      >
        <h2>{title}</h2>
        {!directory && <p className="studio-help">{t("duplicates.noLibrary")}</p>}
        {directory && <p className="studio-help">{t("duplicates.intro")}</p>}
        {problem && (
          <p className="studio-dialog-failure" role="alert">
            {problem}
          </p>
        )}
        {directory && reading && (
          <p role="status" aria-live="polite">
            {reading.total > 0
              ? t("duplicates.reading", { done: reading.done, total: reading.total })
              : t("duplicates.readingStart")}
          </p>
        )}
        {phase.kind === "failed" && (
          <p className="studio-dialog-failure" role="alert">
            {t("duplicates.failed", { error: phase.error })}
          </p>
        )}
        {phase.kind === "review" && (
          <>
            {finished?.cancelled && <p className="studio-capture-time-error">{t("duplicates.stopped")}</p>}
            {finished && finished.unreadable.length > 0 && (
              <p className="studio-capture-time-error">{t("duplicates.unreadable", { count: finished.unreadable.length })}</p>
            )}
            {groups.length === 0 ? (
              <p role="status">{t("duplicates.none")}</p>
            ) : (
              <>
                <p role="status">
                  {t("duplicates.summary", { count: groups.length, photos: groups.flat().length })}
                </p>
                <p className="studio-help">{t("duplicates.suggestion")}</p>
                <ol className="studio-duplicates-groups">
                  {groups.map((group, g) => (
                    <li key={group[0].id}>
                      <h3>
                        {t("duplicates.group", { count: group.length })} ·{" "}
                        {t(group[0].compared === "file" ? "duplicates.comparedFile" : "duplicates.comparedImage")}
                      </h3>
                      {group.every((m) => trashed.has(m.path)) && (
                        <p className="studio-capture-time-error" role="alert">
                          {t("duplicates.keepOne")}
                        </p>
                      )}
                      <ul>{group.map((member, m) => memberRow(member, `${g}-${m}`))}</ul>
                    </li>
                  ))}
                </ol>
              </>
            )}
          </>
        )}
        <footer>
          {reading && reading.run !== null && (
            <button type="button" onClick={() => void cancelDuplicatesPass(reading.run!).catch(() => {})}>
              {t("duplicates.stop")}
            </button>
          )}
          <button type="button" onClick={close}>
            {t("duplicates.close")}
          </button>
          {phase.kind === "review" && groups.length > 0 && (
            <button type="button" className="is-primary" disabled={blocked} onClick={() => void moveToTrash()}>
              {t("duplicates.moveToTrash", { count: toTrash.length })}
            </button>
          )}
        </footer>
        {phase.kind === "review" && toTrash.some(hasOwnWork) && (
          <p className="studio-help">{t("duplicates.workWarning")}</p>
        )}
      </div>
    </div>,
    document.body,
  );
}
