import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { AlertCircle } from "lucide-react";
import { toast } from "react-toastify";
import { Invokes } from "../ui/AppProperties";
import { useLibraryStore } from "../../store/useLibraryStore";

/** Why a photograph's XMP sidecar was not written (`rawtonic_catalog::XmpProblem`). */
type XmpProblem =
  | { kind: "offline" }
  | { kind: "shared_name" }
  | { kind: "changed_elsewhere" }
  | { kind: "failed"; detail: string };

export interface XmpRetry {
  written: number;
  unsaved: { path: string; problem: XmpProblem }[];
}

/** Asks the app to show these photographs in the grid (App.tsx). */
export const SHOW_PENDING_XMP_EVENT = "rawtonic:show-pending-xmp";
export const PENDING_XMP_VIEW_ID = "rawtonic:pending-xmp";

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

/** The reasons in the order they are listed, with how many photographs each. */
function reasons(retry: XmpRetry) {
  const counts = new Map<string, { count: number; detail?: string }>();
  for (const { problem } of retry.unsaved) {
    const entry = counts.get(problem.kind) ?? { count: 0 };
    entry.count++;
    if (problem.kind === "failed") entry.detail ??= problem.detail;
    counts.set(problem.kind, entry);
  }
  return ["changed_elsewhere", "shared_name", "offline", "failed"]
    .filter((kind) => counts.has(kind))
    .map((kind) => ({ kind, ...counts.get(kind)! }));
}

/**
 * The session bar's "XMP not updated" status: shown only while some
 * photographs' XMP sidecars lag behind the library (a write failed, a sidecar
 * was changed by another program, a RAW and JPEG share a name). It opens a
 * panel that says so in plain words, writes them again on request and shows
 * the photographs. Edits are safe in the library either way.
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

  if (count <= 0 && !open) return null;

  const retry = async () => {
    setBusy(true);
    const current = () => useLibraryStore.getState().sessionDirectory === directory;
    try {
      const done = await invoke<XmpRetry>(Invokes.SessionWritePendingXmp, { directory });
      // Another library opened meanwhile: its bar is not this one's.
      if (!current()) return;
      await onChanged();
      // The grid showing these photographs lists the ones still left.
      if (useLibraryStore.getState().activeAlbumId === PENDING_XMP_VIEW_ID)
        window.dispatchEvent(new CustomEvent(SHOW_PENDING_XMP_EVENT, { detail: { refresh: true } }));
      if (done.unsaved.length === 0) {
        setOpen(false);
        setResult(null);
        toast.success(t("session.xmp.written", { count: done.written }));
      } else {
        setResult(done);
      }
    } catch (e) {
      if (current()) toast.error(t("session.xmp.retryFailed", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const show = () => {
    window.dispatchEvent(new Event(SHOW_PENDING_XMP_EVENT));
    setOpen(false);
  };

  const label = t("session.xmp.count", { count });
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
          setOpen((was) => !was);
        }}
      >
        <AlertCircle className="session-count-status-icon" size={13} aria-hidden />
        <span className="session-count-status-text" aria-hidden>{` · ${label}`}</span>
      </button>
      {open &&
        createPortal(
          <div
            ref={panel}
            className="session-xmp-panel"
            role="dialog"
            aria-label={t("session.xmp.title")}
            tabIndex={-1}
            style={position}
          >
            <p className="session-xmp-title">{t("session.xmp.heading", { count })}</p>
            <p>{t("session.xmp.explain")}</p>
            {result && (
              <ul className="session-xmp-reasons">
                {reasons(result).map(({ kind, count: n, detail }) => (
                  <li key={kind}>
                    {kind === "changed_elsewhere"
                      ? t("session.xmp.reason.changedElsewhere", { count: n })
                      : kind === "shared_name"
                        ? t("session.xmp.reason.sharedName", { count: n })
                        : kind === "offline"
                          ? t("session.xmp.reason.offline", { count: n })
                          : t("session.xmp.reason.failed", { count: n, error: detail })}
                  </li>
                ))}
              </ul>
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
