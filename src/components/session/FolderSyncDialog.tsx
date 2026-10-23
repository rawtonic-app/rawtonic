import OperationError from "../ui/OperationError";
import { useEffect, useMemo, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Check, LoaderCircle, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Invokes } from "../ui/AppProperties";
import {
  SYNC_GROUPS,
  SYNC_PROGRESS,
  defaultSelection,
  groupRows,
  planIsEmpty,
  rowKey,
  shownPath,
  sortPlan,
  syncActions,
  unresolved,
  type ContestedChoice,
  type SyncApplied,
  type SyncGroup,
  type SyncMissing,
  type SyncMove,
  type SyncNewFile,
  type SyncPlan,
  type SyncProgress,
} from "../../utils/folderSync";

interface Props {
  /** The open library. */
  directory: string;
  /** The folder to synchronize; null for the whole library. */
  folder: string | null;
  onClose(): void;
  /** After changes were applied (also when every one was skipped). */
  onApplied(applied: SyncApplied): void;
}

type Phase =
  | { kind: "scanning" }
  | { kind: "review"; plan: SyncPlan }
  | { kind: "applying"; plan: SyncPlan }
  | { kind: "done"; applied: SyncApplied }
  | { kind: "failed"; error: string };

/** Rows a group lists at most; the group's checkbox still covers all. */
const SHOWN = 300;

const fileName = (path: string) => path.slice(Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\")) + 1);

/** Lightroom's Synchronize Folder: scans, lists what changed in groups for
 * review (what the scan proved ticked, the rest left to the user), and
 * applies only what is ticked. Files on disk are never changed. */
export default function FolderSyncDialog({ directory, folder, onClose, onApplied }: Props) {
  const { t } = useTranslation();
  const dialog = useRef<HTMLDialogElement>(null);
  const [phase, setPhase] = useState<Phase>({ kind: "scanning" });
  const [progress, setProgress] = useState<SyncProgress | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [choices, setChoices] = useState<Map<string, ContestedChoice>>(new Map());
  const [error, setError] = useState("");

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen<SyncProgress>(SYNC_PROGRESS, ({ payload }) => setProgress(payload)).then((fn) => {
      if (disposed) fn();
      else stop = fn;
    });
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);
  // One scan per dialog (React's development double effects included).
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const started = useRef(false);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    invoke<SyncPlan>(Invokes.FolderSyncScan, { directory, folder })
      .then((scannedPlan) => {
        if (!mounted.current) return;
        const plan = sortPlan(scannedPlan);
        setScanned(plan.folders);
        setSelected(defaultSelection(plan));
        setChoices(new Map());
        setPhase({ kind: "review", plan });
      })
      .catch((e) => {
        if (!mounted.current) return;
        if (String(e) === "cancelled") closeRef.current();
        else setPhase({ kind: "failed", error: String(e) });
      });
  }, [directory, folder]);

  const plan = phase.kind === "review" || phase.kind === "applying" ? phase.plan : null;
  // The folders the last scan covered; paths show relative to them.
  const [scanned, setScanned] = useState<string[]>(folder ? [folder] : []);
  const shown = (path: string) => shownPath(scanned, path);
  const actions = useMemo(() => (plan ? syncActions(plan, selected, choices) : []), [plan, selected, choices]);
  const open = plan ? unresolved(plan, choices) : 0;
  const busy = phase.kind === "scanning" || phase.kind === "applying";

  const cancelScan = async () => {
    setCancelling(true);
    try {
      await invoke(Invokes.FolderSyncCancel);
    } catch (e) {
      setError(String(e));
      setCancelling(false);
    }
  };
  const apply = async () => {
    if (!plan) return;
    setError("");
    setPhase({ kind: "applying", plan });
    try {
      const applied = await invoke<SyncApplied>(Invokes.FolderSyncApply, { directory, actions });
      setPhase({ kind: "done", applied });
      onApplied(applied);
    } catch (e) {
      setError(String(e));
      setPhase({ kind: "review", plan });
    }
  };
  const toggle = (keys: string[], on: boolean) =>
    setSelected((previous) => {
      const next = new Set(previous);
      for (const key of keys) {
        if (on) next.add(key);
        else next.delete(key);
      }
      return next;
    });

  const describe = (group: SyncGroup, row: SyncMove | SyncNewFile | SyncMissing) => {
    if (group === "verifiedMoves" || group === "probableMoves") {
      const move = row as SyncMove;
      return (
        <>
          <span title={move.from}>{shown(move.from)}</span>
          <span aria-hidden> → </span>
          <span title={move.to}>{shown(move.to)}</span>
        </>
      );
    }
    if (group === "copies") {
      const file = row as SyncNewFile;
      return (
        <span title={[file.path, ...file.copyOf].join("\n")}>
          {t("session.sync.copyOf", { path: shown(file.path), of: file.copyOf.map(fileName).join(", ") })}
        </span>
      );
    }
    const path = "path" in row ? row.path : "";
    return <span title={path}>{shown(path)}</span>;
  };

  const groupSection = (current: SyncPlan, group: SyncGroup) => {
    const rows = groupRows(current, group);
    if (!rows.length) return null;
    const keys = rows.map((row) => rowKey(group, row));
    const ticked = keys.filter((key) => selected.has(key)).length;
    const title = t(`session.sync.groups.${group}`);
    return (
      <section key={group} className="session-sync-group" aria-label={title}>
        <header>
          <label>
            <input
              type="checkbox"
              checked={ticked === keys.length}
              ref={(input) => {
                if (input) input.indeterminate = ticked > 0 && ticked < keys.length;
              }}
              disabled={busy}
              onChange={(e) => toggle(keys, e.target.checked)}
              aria-label={t("session.sync.selectGroup", { group: title })}
            />
            <strong>{title}</strong>
          </label>
          <small>{t("session.sync.counted", { selected: ticked, total: keys.length })}</small>
        </header>
        <p>{t(`session.sync.hints.${group}`)}</p>
        <ul>
          {rows.slice(0, SHOWN).map((row, index) => (
            <li key={keys[index]}>
              <label>
                <input
                  type="checkbox"
                  checked={selected.has(keys[index])}
                  disabled={busy}
                  onChange={(e) => toggle([keys[index]], e.target.checked)}
                  aria-label={"to" in row ? row.to : row.path}
                />
                {describe(group, row)}
              </label>
            </li>
          ))}
          {rows.length > SHOWN && <li className="session-sync-more">{t("session.sync.more", { n: rows.length - SHOWN })}</li>}
        </ul>
      </section>
    );
  };

  const contestedSection = (current: SyncPlan) => {
    if (!current.contested.length) return null;
    const title = t("session.sync.groups.contested");
    return (
      <section className="session-sync-group" aria-label={title}>
        <header>
          <strong>{title}</strong>
          <small>{t("session.sync.toChoose", { n: open })}</small>
        </header>
        <p>{t("session.sync.hints.contested")}</p>
        <ul>
          {current.contested.map((contested) => {
            const choice = choices.get(contested.path);
            const choose = (value: ContestedChoice) =>
              setChoices((previous) => new Map(previous).set(contested.path, value));
            return (
              <li key={contested.path} className="session-sync-contested">
                <fieldset disabled={busy}>
                  <legend title={contested.path}>{shown(contested.path)}</legend>
                  {contested.moves.map((move) => (
                    <label key={move.assetId}>
                      <input
                        type="radio"
                        name={contested.path}
                        checked={typeof choice === "object" && choice.move === move.assetId}
                        onChange={() => choose({ move: move.assetId })}
                      />
                      <span title={move.from}>
                        {t(move.verified ? "session.sync.choice.movedVerified" : "session.sync.choice.moved", {
                          path: shown(move.from),
                        })}
                      </span>
                    </label>
                  ))}
                  <label>
                    <input type="radio" name={contested.path} checked={choice === "add"} onChange={() => choose("add")} />
                    <span>
                      {contested.copyOf.length
                        ? t("session.sync.choice.addCopy", { of: contested.copyOf.map(fileName).join(", ") })
                        : t("session.sync.choice.add")}
                    </span>
                  </label>
                  <label>
                    <input type="radio" name={contested.path} checked={choice === "skip"} onChange={() => choose("skip")} />
                    <span>{t("session.sync.choice.skip")}</span>
                  </label>
                </fieldset>
              </li>
            );
          })}
        </ul>
      </section>
    );
  };

  const infoSection = (title: string, hint: string, rows: { path: string; detail?: string | null }[]) =>
    rows.length ? (
      <section className="session-sync-group is-info" aria-label={title}>
        <header>
          <strong>{title}</strong>
          <small>{rows.length}</small>
        </header>
        <p>{hint}</p>
        <ul>
          {rows.slice(0, SHOWN).map((row) => (
            <li key={row.path} title={row.path}>
              <span>{shown(row.path)}</span>
              {row.detail && <small>{row.detail}</small>}
            </li>
          ))}
          {rows.length > SHOWN && <li className="session-sync-more">{t("session.sync.more", { n: rows.length - SHOWN })}</li>}
        </ul>
      </section>
    ) : null;

  const review = (current: SyncPlan) => (
    <div className="session-sync-review">
      {planIsEmpty(current) ? (
        <p className="session-sync-summary" role="status">
          {/* Photographs on a disconnected drive are no proof that all is in sync. */}
          {t(current.unavailable.length || current.errors.length ? "session.sync.nothingToChange" : "session.sync.inSync", {
            n: current.files,
          })}
        </p>
      ) : (
        <p className="session-sync-summary">{t("session.sync.found", { n: current.files })}</p>
      )}
      {SYNC_GROUPS.filter((group) => group === "verifiedMoves" || group === "newFiles").map((group) =>
        groupSection(current, group),
      )}
      {contestedSection(current)}
      {SYNC_GROUPS.filter((group) => group !== "verifiedMoves" && group !== "newFiles").map((group) =>
        groupSection(current, group),
      )}
      {infoSection(
        t("session.sync.groups.unavailable"),
        t("session.sync.hints.unavailable"),
        current.unavailable.map((row) => ({
          path: row.path,
          detail: row.volume ? t("session.sync.onVolume", { volume: row.volume }) : null,
        })),
      )}
      {infoSection(
        t("session.sync.groups.errors"),
        t("session.sync.hints.errors"),
        current.errors.map((row) => ({ path: row.path, detail: row.reason })),
      )}
    </div>
  );

  const scanning = () => (
    <div className="session-transfer-progress" role="status">
      <LoaderCircle size={18} className="animate-spin" />
      <span>
        {cancelling
          ? t("session.sync.cancelling")
          : progress?.phase === "checking"
            ? t("session.sync.checking", { done: progress.done, total: progress.total })
            : t("session.sync.listing", { n: progress?.done ?? 0 })}
      </span>
    </div>
  );

  const done = (applied: SyncApplied) => (
    <div className="session-transfer-result" role="status">
      <Check size={24} />
      <strong>
        {t("session.sync.applied", {
          moved: applied.moved.length,
          added: applied.added.length,
          removed: applied.removed.length,
        })}
      </strong>
      {applied.skipped.length > 0 && (
        <>
          <p>{t("session.sync.skipped", { n: applied.skipped.length })}</p>
          <ul className="session-sync-skipped">
            {applied.skipped.map((skip) => (
              <li key={`${skip.path}:${skip.reason}`} title={skip.path}>
                <span>{shown(skip.path)}</span>
                <small>{skip.reason}</small>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );

  return (
    <dialog
      ref={dialog}
      className="session-dialog session-sync-dialog"
      aria-labelledby="sync-title"
      onCancel={(event) => {
        event.preventDefault();
        if (phase.kind === "scanning") void cancelScan();
        else if (phase.kind !== "applying") onClose();
      }}
      onKeyDown={(event) => event.stopPropagation()}
    >
      <div className="session-dialog-header">
        <div>
          <h2 id="sync-title">{t(folder ? "session.sync.title" : "session.sync.titleLibrary")}</h2>
          <p>{folder ?? t("session.sync.wholeLibrary")}</p>
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
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void apply();
        }}
      >
        {phase.kind === "scanning" && scanning()}
        {plan && review(plan)}
        {phase.kind === "done" && done(phase.applied)}
        {phase.kind === "failed" && (
          <p className="session-error" role="alert">
            <OperationError detail={phase.error} />
          </p>
        )}
        {error && (
          <p className="session-error" role="alert">
            <OperationError detail={error} />
          </p>
        )}
        <div className="session-dialog-footer">
          {phase.kind === "scanning" ? (
            <>
              <span>{t("session.sync.readOnly")}</span>
              <button type="button" className="session-action" disabled={cancelling} onClick={() => void cancelScan()}>
                {t("session.progress.cancel")}
              </button>
            </>
          ) : plan && !planIsEmpty(plan) ? (
            <>
              <span>{open ? t("session.sync.chooseFirst", { n: open }) : t("session.sync.filesUnchanged")}</span>
              <span className="session-sync-buttons">
                <button type="button" className="session-action" disabled={busy} onClick={onClose}>
                  {t("session.sync.cancel")}
                </button>
                <button
                  type="submit"
                  className="session-action session-primary"
                  disabled={busy || open > 0 || actions.length === 0}
                >
                  {phase.kind === "applying" ? (
                    <LoaderCircle size={14} className="animate-spin" />
                  ) : (
                    t("session.sync.apply", { n: actions.length })
                  )}
                </button>
              </span>
            </>
          ) : (
            <>
              <span />
              <button type="button" className="session-action" onClick={onClose}>
                {t("session.dialogs.close")}
              </button>
            </>
          )}
        </div>
      </form>
    </dialog>
  );
}
