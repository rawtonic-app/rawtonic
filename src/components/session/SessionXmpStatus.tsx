import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { notify } from "../../activity/notify";
import { useActivityStore } from "../../store/useActivityStore";
import { Invokes } from "../ui/AppProperties";
import { useLibraryStore } from "../../store/useLibraryStore";
import { xmpReadAction, type XmpReport } from "../../utils/appUndo";
import { invokeInOrder } from "../../utils/catalogWrites";

/** Why a photograph's XMP sidecar was not written (`rawtonic_catalog::XmpProblem`). */
type XmpProblem =
  | { kind: "offline" }
  | { kind: "shared_name" }
  | { kind: "changed_elsewhere" }
  | { kind: "failed"; detail: string };

/** A photograph of the panel's list (`XmpReviewEntry` in
 * src-tauri/src/xmp_review.rs); no problem: Try again writes it. */
export interface XmpReviewEntry {
  path: string;
  problem: XmpProblem | null;
  volume: string | null;
}

export interface XmpRetry {
  written: number;
  unsaved: { path: string; problem: XmpProblem }[];
}

/** Asks the app to show these photographs in the grid (App.tsx). */
export const SHOW_PENDING_XMP_EVENT = "rawtonic:show-pending-xmp";
export const PENDING_XMP_VIEW_ID = "rawtonic:pending-xmp";
/** The activity centre's entry for XMP files not up to date. */
const XMP_ACTIVITY = "xmp:pending";

/** Shows the photographs whose XMP is not up to date in the grid, as a view
 * like Missing Photographs (listed again when the library refreshes). */
export async function openPendingXmpView(
  directory: string,
  name: string,
  select: (id: string, name: string, paths: string[], preserveEditor?: boolean) => void,
  preserveEditor = false,
) {
  const asked = useLibraryStore.getState().sessionDirectory;
  const paths = await invoke<string[]>(Invokes.SessionPendingXmp, { directory });
  // Another library opened meanwhile: not its photographs.
  if (useLibraryStore.getState().sessionDirectory !== asked) return;
  select(PENDING_XMP_VIEW_ID, name, paths, preserveEditor);
}

/** The reasons in the order they are listed, with how many photographs
 * each: the list's, where a write Try again just tried failed says why. */
function reasons(entries: XmpReviewEntry[], retry: XmpRetry | null) {
  const failed = new Map<string, string>();
  for (const { path, problem } of retry?.unsaved ?? [])
    if (problem.kind === "failed") failed.set(path, problem.detail);
  const counts = new Map<string, { count: number; detail?: string }>();
  for (const { path, problem } of entries) {
    const detail = !problem ? failed.get(path) : undefined;
    const kind = detail !== undefined ? "failed" : (problem?.kind ?? "pending");
    const entry = counts.get(kind) ?? { count: 0 };
    entry.count++;
    entry.detail ??= detail;
    counts.set(kind, entry);
  }
  return ["changed_elsewhere", "shared_name", "offline", "failed", "pending"]
    .filter((kind) => counts.has(kind))
    .map((kind) => ({ kind, ...counts.get(kind)! }));
}

const fileName = (path: string) => path.slice(Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\")) + 1);

/** Height of a conflict row and of the list's window, in pixels. */
const ROW = 30;
const WINDOW = 210;
/** Rows kept above and below the window (Tab reaches them; scrolling to
 * a focused one brings the next). */
const OVERSCAN = 4;

/** Every conflict, one row each, of which only those in view (and a few
 * around them) are in the page, so a shoot of thousands stays light. */
function ConflictList({
  entries,
  label,
  row,
}: {
  entries: XmpReviewEntry[];
  label: string;
  row(entry: XmpReviewEntry): React.ReactNode;
}) {
  const [top, setTop] = useState(0);
  const first = Math.max(0, Math.floor(top / ROW) - OVERSCAN);
  const last = Math.min(entries.length, Math.ceil((top + WINDOW) / ROW) + OVERSCAN);
  return (
    <div
      className="session-xmp-scroll"
      style={{ height: Math.min(WINDOW, entries.length * ROW) }}
      onScroll={(event) => setTop(event.currentTarget.scrollTop)}
    >
      <ul className="session-xmp-list" aria-label={label} style={{ height: entries.length * ROW }}>
        {entries.slice(first, last).map((entry, index) => (
          <li key={entry.path} title={entry.path} style={{ top: (first + index) * ROW, height: ROW }}>
            {row(entry)}
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * The session bar's "XMP not updated" status: shown only while some
 * photographs' XMP sidecars lag behind the library (a write failed, a sidecar
 * was changed by another program, a RAW and JPEG share a name). It opens a
 * panel that says why in plain words, writes them again on request (Try
 * again), shows the photographs, and resolves the sidecars another program
 * changed: Keep library writes the library's metadata into them, Use XMP
 * reads theirs into the library (one step of Undo). Edits are safe in the
 * library either way and never change from it.
 */
export default function SessionXmpStatus({
  directory,
  count,
  onChanged,
}: {
  directory: string;
  count: number;
  onChanged(): Promise<void> | void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<XmpRetry | null>(null);
  const [entries, setEntries] = useState<XmpReviewEntry[] | null>(null);
  const [resolved, setResolved] = useState<XmpReport | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState({ left: 8, top: 40 });

  useLayoutEffect(() => {
    if (!open) return;
    const box = button.current?.getBoundingClientRect();
    const width = panel.current?.getBoundingClientRect().width ?? 300;
    if (!box) return;
    setPosition({
      left: Math.max(8, Math.min(box.left, window.innerWidth - width - 8)),
      top: box.bottom + 6,
    });
  }, [open]);

  useEffect(() => {
    if (!open) return;
    panel.current?.focus({ preventScroll: true });
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent) {
        if (event.key !== "Escape") return;
        // Only the panel closes: not a crop or a view behind it.
        event.preventDefault();
        event.stopPropagation();
        button.current?.focus({ preventScroll: true });
      } else if (
        panel.current?.contains(event.target as Node) ||
        button.current?.contains(event.target as Node)
      ) {
        return;
      }
      setOpen(false);
    };
    window.addEventListener("keydown", close, true);
    window.addEventListener("pointerdown", close, true);
    return () => {
      window.removeEventListener("keydown", close, true);
      window.removeEventListener("pointerdown", close, true);
    };
  }, [open]);

  // Not up to date is also under the activity centre's Needs attention
  // (this chip stays its own view), until written or resolved.
  useEffect(() => {
    if (count <= 0) return;
    useActivityStore.getState().report("warning", {
      id: XMP_ACTIVITY,
      source: "xmp",
      title: t("session.xmp.count", { count }),
      detail: t("session.xmp.explain"),
      actions: [
        { label: t("session.xmp.show"), run: () => window.dispatchEvent(new Event(SHOW_PENDING_XMP_EVENT)) },
        {
          label: t("activity.xmp.review"),
          run: () => {
            useActivityStore.getState().setOpen(false);
            setOpen(true);
            button.current?.focus({ preventScroll: true });
          },
        },
      ],
    });
    return () => useActivityStore.getState().resolve(XMP_ACTIVITY);
  }, [count, t]);

  const current = useCallback(() => useLibraryStore.getState().sessionDirectory === directory, [directory]);
  const load = useCallback(async () => {
    const listed = await invoke<XmpReviewEntry[]>(Invokes.SessionXmpReview, { directory });
    if (current()) setEntries(Array.isArray(listed) ? listed : []);
  }, [current, directory]);
  // Listed when the panel opens and again when the count changes under it
  // (a write elsewhere, a resolution).
  useEffect(() => {
    if (!open) return;
    load().catch((e) => current() && notify.error(t("session.xmp.retryFailed", { error: String(e) })));
  }, [open, count, load, current, t]);

  if (count <= 0 && !open) return null;

  /** Keep library or Use XMP for `paths`: through the photos' write queues
   * (Use XMP as one step of Undo); the photographs are never touched. */
  const resolve = async (paths: string[], keep: "library" | "xmp") => {
    setBusy(true);
    setResult(null);
    try {
      const report =
        keep === "xmp"
          ? await xmpReadAction(directory, paths)
          : await invokeInOrder<XmpReport>(Invokes.SessionXmpResolve, { directory, paths, keep }, "keepLibraryXmp");
      if (!current()) return;
      await onChanged();
      await load();
      if (useLibraryStore.getState().activeAlbumId === PENDING_XMP_VIEW_ID)
        window.dispatchEvent(new CustomEvent(SHOW_PENDING_XMP_EVENT, { detail: { refresh: true } }));
      if (report.done.length) notify.success(t("session.xmp.resolved", { count: report.done.length }));
      setResolved(report.failed.length || report.conflicts.length || report.offline.length ? report : null);
    } catch (e) {
      if (current()) notify.error(t("session.xmp.retryFailed", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const retry = async () => {
    setBusy(true);
    setResolved(null);
    try {
      const done = await invoke<XmpRetry>(Invokes.SessionWritePendingXmp, { directory });
      // Another library opened meanwhile: its bar is not this one's.
      if (!current()) return;
      await onChanged();
      await load().catch(() => {});
      // The grid showing these photographs lists the ones still left.
      if (useLibraryStore.getState().activeAlbumId === PENDING_XMP_VIEW_ID)
        window.dispatchEvent(new CustomEvent(SHOW_PENDING_XMP_EVENT, { detail: { refresh: true } }));
      if (done.unsaved.length === 0) {
        setOpen(false);
        setResult(null);
        notify.success(t("session.xmp.written", { count: done.written }));
      } else {
        setResult(done);
      }
    } catch (e) {
      if (current()) notify.error(t("session.xmp.retryFailed", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const show = () => {
    window.dispatchEvent(new Event(SHOW_PENDING_XMP_EVENT));
    setOpen(false);
  };

  const label = t("session.xmp.count", { count });
  const conflicts = entries?.filter((entry) => entry.problem?.kind === "changed_elsewhere") ?? [];
  const conflictPaths = () => conflicts.map((entry) => entry.path);
  const notResolved = resolved
    ? [
        ...resolved.failed,
        ...resolved.conflicts.map((path) => ({ path, reason: t("session.xmp.reason.changedElsewhere", { count: 1 }) })),
        ...resolved.offline.map((path) => ({ path, reason: t("session.xmp.reason.offline", { count: 1 }) })),
      ]
    : [];
  return (
    <>
      <button
        ref={button}
        type="button"
        className="session-count-pending"
        aria-label={label}
        aria-expanded={open}
        aria-haspopup="dialog"
        title={t("session.xmp.buttonTitle")}
        onClick={() => {
          setResult(null);
          setResolved(null);
          setEntries(null);
          setOpen((was) => !was);
        }}
      >
        <span className="session-count-status-text" aria-hidden>{` · ${label}`}</span>
      </button>
      {open &&
        createPortal(
          <div
            ref={panel}
            className={conflicts.length ? "session-xmp-panel has-conflicts" : "session-xmp-panel"}
            role="dialog"
            aria-label={t("session.xmp.title")}
            tabIndex={-1}
            style={position}
          >
            <p className="session-xmp-title">{t("session.xmp.heading", { count })}</p>
            <p>{t("session.xmp.explain")}</p>
            {!entries && <p className="session-xmp-hint" role="status">{t("session.xmp.loading")}</p>}
            {entries && entries.length > 0 && (
              <ul className="session-xmp-reasons">
                {reasons(entries, result).map(({ kind, count: n, detail }) => (
                  <li key={kind}>
                    {kind === "changed_elsewhere"
                      ? t("session.xmp.reason.changedElsewhere", { count: n })
                      : kind === "shared_name"
                        ? t("session.xmp.reason.sharedName", { count: n })
                        : kind === "offline"
                          ? t("session.xmp.reason.offline", { count: n })
                          : kind === "failed"
                            ? t("session.xmp.reason.failed", { count: n, error: detail })
                            : t("session.xmp.reason.pending", { count: n })}
                  </li>
                ))}
              </ul>
            )}
            {conflicts.length > 0 && (
              <section className="session-xmp-conflicts" aria-labelledby="xmp-conflicts">
                <div className="session-xmp-conflicts-header">
                  <p id="xmp-conflicts" className="session-xmp-title">
                    {t("session.xmp.conflictHeading", { count: conflicts.length })}
                  </p>
                  <span className="session-xmp-buttons">
                    <button
                      type="button"
                      className="session-action"
                      disabled={busy}
                      title={t("session.xmp.keepLibraryTitle")}
                      onClick={() => void resolve(conflictPaths(), "library")}
                    >
                      {t("session.xmp.keepLibraryAll")}
                    </button>
                    <button
                      type="button"
                      className="session-action"
                      disabled={busy}
                      title={t("session.xmp.useXmpTitle")}
                      onClick={() => void resolve(conflictPaths(), "xmp")}
                    >
                      {t("session.xmp.useXmpAll")}
                    </button>
                  </span>
                </div>
                <p className="session-xmp-hint">{t("session.xmp.conflictHint")}</p>
                <ConflictList
                  entries={conflicts}
                  label={t("session.xmp.conflictHeading", { count: conflicts.length })}
                  row={(entry) => {
                    const name = fileName(entry.path);
                    return (
                      <>
                        <span>{name}</span>
                        <button
                          type="button"
                          className="session-action"
                          disabled={busy}
                          aria-label={t("session.xmp.keepLibraryFor", { name })}
                          title={t("session.xmp.keepLibraryTitle")}
                          onClick={() => void resolve([entry.path], "library")}
                        >
                          {t("session.xmp.keepLibrary")}
                        </button>
                        <button
                          type="button"
                          className="session-action"
                          disabled={busy}
                          aria-label={t("session.xmp.useXmpFor", { name })}
                          title={t("session.xmp.useXmpTitle")}
                          onClick={() => void resolve([entry.path], "xmp")}
                        >
                          {t("session.xmp.useXmp")}
                        </button>
                      </>
                    );
                  }}
                />
              </section>
            )}
            {notResolved.length > 0 && (
              <div className="session-xmp-failed" role="status">
                <p>{t("session.xmp.notResolved", { count: notResolved.length })}</p>
                <ul>
                  {notResolved.map(({ path, reason }) => (
                    <li key={path} title={path}>{`${fileName(path)}: ${reason}`}</li>
                  ))}
                </ul>
              </div>
            )}
            <div className="session-xmp-actions">
              <button type="button" className="session-action" onClick={show}>
                {t("session.xmp.show")}
              </button>
              <button
                type="button"
                className="session-action session-primary"
                disabled={busy}
                onClick={() => void retry()}
              >
                {busy ? t("session.xmp.writing") : t("session.xmp.retry")}
              </button>
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}
