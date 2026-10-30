import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import type { ParseKeys } from "i18next";
import { notify } from "../../activity/notify";
import { create } from "zustand";
import { captureTimeAction } from "../../utils/appUndo";
import {
  captureText,
  planCaptureTimes,
  previewCaptureTimes,
  type CapturePreview,
  type Plan,
  type PlanRequest,
  type ShiftParts,
} from "../../utils/captureTime";

interface CaptureTimeDialogState {
  paths: string[] | null;
  open(paths: string[]): void;
  close(): void;
}
export const useCaptureTimeDialog = create<CaptureTimeDialogState>((set) => ({
  paths: null,
  open: (paths) => set({ paths: [...paths] }),
  close: () => set({ paths: null }),
}));

const fileName = (path: string) => path.split("?vc=")[0].split(/[\\/]/).pop() ?? path;

export function CaptureTimeDialogHost() {
  const paths = useCaptureTimeDialog((s) => s.paths);
  if (!paths) return null;
  return <CaptureTimeDialog key={paths.join("\n")} paths={paths} />;
}

type Mode = "shift" | "reference" | "reset";
const NO_SHIFT: ShiftParts = { negative: false, days: "", hours: "", minutes: "", seconds: "" };

/**
 * Shift Capture Time: one signed shift for the selected photographs (or
 * the shift that sets one reference photograph's clock right), previewed
 * before anything is written. The library keeps the correction apart from
 * what the files say; Back to the files' times removes it. One undo step.
 */
function CaptureTimeDialog({ paths }: { paths: string[] }) {
  const { t } = useTranslation();
  const close = useCaptureTimeDialog((s) => s.close);
  const dialog = useRef<HTMLFormElement>(null);
  const [preview, setPreview] = useState<CapturePreview | null>(null);
  const [mode, setMode] = useState<Mode>("shift");
  const [parts, setParts] = useState<ShiftParts>(NO_SHIFT);
  const [reference, setReference] = useState<string>("");
  const [corrected, setCorrected] = useState("");
  // A plan with the request it answers: Apply waits for the current one.
  const [planned, setPlanned] = useState<{ key: string; plan: Plan } | null>(null);
  const [applying, setApplying] = useState(false);
  const [conflict, setConflict] = useState(false);

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    dialog.current?.focus();
    return () => before?.focus?.();
  }, []);

  const load = useCallback(() => {
    previewCaptureTimes(paths)
      .then((next) => {
        setPreview(next);
        setReference((current) => current || next.items[0]?.assetId || "");
      })
      .catch((error) => notify.error(t("captureTime.failed", { error: String(error) })));
  }, [paths, t]);
  useEffect(() => load(), [load]);

  const items = useMemo(() => preview?.items ?? [], [preview]);
  const referenceItem = items.find((item) => item.assetId === reference) ?? null;
  const referenceNow = referenceItem ? (referenceItem.corrected ?? referenceItem.source) : null;
  useEffect(() => {
    if (mode === "reference" && referenceNow && !corrected) setCorrected(referenceNow.local);
  }, [mode, referenceNow, corrected]);

  const request: PlanRequest | null =
    mode === "shift"
      ? { kind: "shift", parts }
      : mode === "reference"
        ? reference
          ? { kind: "reference", assetId: reference, corrected }
          : null
        : { kind: "reset" };
  const requestKey = JSON.stringify(request);
  const plan = planned?.key === requestKey ? planned.plan : null;
  useEffect(() => {
    if (!request || !items.length) {
      setPlanned(null);
      return;
    }
    let current = true;
    planCaptureTimes(items, request)
      .then((next) => current && setPlanned({ key: requestKey, plan: next }))
      .catch((error) => {
        if (!current) return;
        setPlanned(null);
        notify.error(t("captureTime.failed", { error: String(error) }));
      });
    return () => {
      current = false;
    };
    // The request is compared by value.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [requestKey, items, t]);

  const cameras = useMemo(() => {
    const counts = new Map<string, number>();
    for (const item of items) {
      const name = item.camera || t("captureTime.unknownCamera");
      counts.set(name, (counts.get(name) ?? 0) + 1);
    }
    return [...counts.entries()].map(([name, count]) => t("captureTime.cameraCount", { name, n: count })).join(", ");
  }, [items, t]);
  const correctedCount = items.filter((item) => item.corrected).length;
  const changes = plan?.changes ?? [];
  const referenceChange = changes.find((change) => change.assetId === reference);
  const blocked = !plan || !!plan.error || plan.outOfRange.length > 0 || changes.length === 0;

  const fieldError = (field: keyof ShiftParts) =>
    plan?.error?.kind === "field" && plan.error.field === field ? t("captureTime.errors.field") : null;
  const referenceError =
    plan?.error?.kind === "reference" ? t(`captureTime.errors.${plan.error.problem}` as ParseKeys) : null;

  const apply = async (e?: FormEvent) => {
    e?.preventDefault();
    if (blocked || applying || !preview?.library) return;
    setApplying(true);
    try {
      const applied = await captureTimeAction(preview.library, changes);
      if (applied.conflicts.length) {
        setConflict(true);
        setApplying(false);
        load();
        return;
      }
      notify.success(t("captureTime.applied", { count: changes.length }));
      if (applied.failed.length) notify.warning(t("captureTime.sidecarsFailed", { count: applied.failed.length }));
      close();
    } catch (error) {
      notify.error(t("captureTime.failed", { error: String(error) }));
      setApplying(false);
    }
  };

  const keys = (e: KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "Escape") close();
  };
  const title = t("captureTime.title");
  const shiftText = (shift: ShiftParts) =>
    t(shift.negative ? "captureTime.shiftEarlier" : "captureTime.shiftLater", {
      days: shift.days,
      hours: shift.hours,
      minutes: shift.minutes,
      seconds: shift.seconds,
    });
  const part = (field: Exclude<keyof ShiftParts, "negative">) => {
    const error = fieldError(field);
    return (
      <div className="studio-capture-time-part">
        <label htmlFor={`capture-time-${field}`}>{t(`captureTime.parts.${field}` as ParseKeys)}</label>
        <input
          id={`capture-time-${field}`}
          type="text"
          inputMode={field === "seconds" ? "decimal" : "numeric"}
          value={parts[field]}
          placeholder="0"
          aria-invalid={!!error}
          aria-describedby={error ? `capture-time-${field}-error` : undefined}
          onChange={(e) => setParts((p) => ({ ...p, [field]: e.target.value }))}
        />
        {error && (
          <small id={`capture-time-${field}-error`} className="studio-capture-time-error">
            {error}
          </small>
        )}
      </div>
    );
  };

  return createPortal(
    <div className="studio-settings studio-smart-album-backdrop" onKeyDown={keys}>
      <form
        ref={dialog}
        className="studio-capture-time"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onSubmit={(e) => void apply(e)}
      >
        <h2>{title}</h2>
        {!preview && <p role="status">{t("captureTime.reading")}</p>}
        {preview && !preview.library && <p className="studio-help">{t("captureTime.notInLibrary")}</p>}
        {preview?.library && (
          <>
            <p className="studio-help">{t("captureTime.intro")}</p>
            <p role="status">
              {t("captureTime.summary", { count: items.length })}
              {items.length > 0 && <> · {cameras}</>}
            </p>
            {preview.excluded.length > 0 && (
              <details className="studio-capture-time-excluded">
                <summary>{t("captureTime.excludedCount", { count: preview.excluded.length })}</summary>
                <ul>
                  {preview.excluded.map(({ path, reason }) => (
                    <li key={path}>
                      {fileName(path)} — {t(`captureTime.excluded.${reason}` as ParseKeys)}
                    </li>
                  ))}
                </ul>
              </details>
            )}
            {conflict && (
              <p className="studio-capture-time-error" role="alert">
                {t("captureTime.conflict", { count: changes.length })}
              </p>
            )}
            <fieldset className="studio-capture-time-modes">
              <legend>{t("captureTime.how")}</legend>
              <label>
                <input type="radio" name="capture-time-mode" checked={mode === "shift"} onChange={() => setMode("shift")} />
                {t("captureTime.modes.shift")}
              </label>
              <label>
                <input
                  type="radio"
                  name="capture-time-mode"
                  checked={mode === "reference"}
                  onChange={() => setMode("reference")}
                />
                {t("captureTime.modes.reference")}
              </label>
              <label>
                <input
                  type="radio"
                  name="capture-time-mode"
                  checked={mode === "reset"}
                  disabled={correctedCount === 0}
                  onChange={() => setMode("reset")}
                />
                {t("captureTime.modes.reset", { count: correctedCount })}
              </label>
            </fieldset>
            {mode === "shift" && (
              <div className="studio-capture-time-shift">
                <label className="studio-capture-time-part">
                  <span>{t("captureTime.direction")}</span>
                  <select
                    value={parts.negative ? "earlier" : "later"}
                    onChange={(e) => setParts((p) => ({ ...p, negative: e.target.value === "earlier" }))}
                  >
                    <option value="later">{t("captureTime.later")}</option>
                    <option value="earlier">{t("captureTime.earlier")}</option>
                  </select>
                </label>
                {part("days")}
                {part("hours")}
                {part("minutes")}
                {part("seconds")}
              </div>
            )}
            {mode === "reference" && (
              <div className="studio-capture-time-reference">
                <label>
                  <span>{t("captureTime.referencePhoto")}</span>
                  <select
                    value={reference}
                    onChange={(e) => {
                      setReference(e.target.value);
                      setCorrected("");
                    }}
                  >
                    {items.map((item) => (
                      <option key={item.assetId} value={item.assetId}>
                        {fileName(item.path)} — {captureText(item.corrected ?? item.source)}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  <span>{t("captureTime.correctedTime")}</span>
                  <input
                    type="text"
                    value={corrected}
                    placeholder="YYYY-MM-DD HH:MM:SS"
                    aria-invalid={!!referenceError}
                    aria-describedby="capture-time-reference-help"
                    onChange={(e) => setCorrected(e.target.value)}
                  />
                </label>
                <small id="capture-time-reference-help" className={referenceError ? "studio-capture-time-error" : "studio-help"}>
                  {referenceError ?? t("captureTime.correctedHelp")}
                </small>
              </div>
            )}
            {plan && !plan.error && (
              <div className="studio-capture-time-preview" role="status" aria-live="polite">
                {plan.shift && mode !== "reset" && <p>{shiftText(plan.shift)}</p>}
                {plan.outOfRange.length > 0 && (
                  <p className="studio-capture-time-error">
                    {t("captureTime.outOfRange", {
                      count: plan.outOfRange.length,
                      name: fileName(plan.outOfRange[0]),
                    })}
                  </p>
                )}
                {referenceChange && mode !== "reset" && (
                  <p>
                    {t("captureTime.referenceChange", {
                      name: fileName(referenceChange.path),
                      before: captureText(referenceChange.before),
                      after: captureText(referenceChange.after),
                    })}
                  </p>
                )}
                {plan.beforeRange && plan.afterRange && (
                  <p>
                    {t("captureTime.rangeChange", {
                      from: captureText(plan.beforeRange[0]),
                      to: captureText(plan.beforeRange[1]),
                      newFrom: captureText(plan.afterRange[0]),
                      newTo: captureText(plan.afterRange[1]),
                    })}
                  </p>
                )}
                {changes.length === 0 && plan.outOfRange.length === 0 && <p>{t("captureTime.nothingToChange")}</p>}
              </div>
            )}
            <p className="studio-help">{t("captureTime.filesUnchanged")}</p>
          </>
        )}
        <footer>
          <button type="button" onClick={close}>
            {t("captureTime.cancel")}
          </button>
          <button type="submit" className="is-primary" disabled={blocked || applying || !preview?.library}>
            {t("captureTime.apply", { count: changes.length })}
          </button>
        </footer>
      </form>
    </div>,
    document.body,
  );
}
