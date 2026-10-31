import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { notify } from "../../activity/notify";
import { Invokes } from "../ui/AppProperties";
import { stackAction } from "../../utils/appUndo";
import { invokeInOrder } from "../../utils/catalogWrites";
import { useAutoStackDialog } from "../../utils/stacks";
import { refreshStacks, type StackChange } from "../../store/useStackStore";

interface AutoGroup {
  kind: "bracket" | "burst" | "time";
  assets: string[];
}
interface AutoPlan {
  library: string;
  groups: AutoGroup[];
  photos: number;
  considered: number;
}

/** Interval stops of the slider, in seconds: 1 s to 60 min, finer where
 * bursts and brackets lie (Lightroom's slider goes to an hour too). */
export const INTERVALS = [
  1, 2, 3, 5, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300, 420, 600, 900, 1200, 1800, 2700, 3600,
];
const DEFAULT_INTERVAL = INTERVALS.indexOf(60);
const REMEMBERED = "rawtonic.autoStack.interval";

/** The interval last used (Lightroom remembers it), else a minute. */
function rememberedStop(): number {
  try {
    const stop = INTERVALS.indexOf(Number(localStorage.getItem(REMEMBERED)));
    return stop >= 0 ? stop : DEFAULT_INTERVAL;
  } catch {
    return DEFAULT_INTERVAL;
  }
}

export function intervalText(seconds: number, t: TFunction) {
  if (seconds < 60) return t("stacks.auto.seconds", { count: seconds });
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return rest
    ? t("stacks.auto.minutesSeconds", { minutes, seconds: rest })
    : t("stacks.auto.minutes", { count: minutes });
}

export function AutoStackDialogHost() {
  const paths = useAutoStackDialog((s) => s.paths);
  if (!paths) return null;
  return <AutoStackDialog key={paths.join("\n")} paths={paths} />;
}

/**
 * Auto-Stack by Capture Time: photographs taken at most the interval apart
 * share a stack; with camera data, brackets and bursts the camera marked
 * are stacks of their own first. The preview counts the stacks as the
 * slider moves; photos in a stack already are left as they are. One undo
 * step for the whole run.
 */
function AutoStackDialog({ paths }: { paths: string[] }) {
  const { t } = useTranslation();
  const close = useAutoStackDialog((s) => s.close);
  const dialog = useRef<HTMLFormElement>(null);
  const [stop, setStop] = useState(rememberedStop);
  const [useCameraData, setUseCameraData] = useState(true);
  const [plan, setPlan] = useState<{ key: string; plan: AutoPlan } | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);
  const seconds = INTERVALS[stop];
  const key = `${seconds}:${useCameraData}`;

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    dialog.current?.focus();
    return () => before?.focus?.();
  }, []);

  useEffect(() => {
    let current = true;
    invoke<AutoPlan>(Invokes.StacksAutoPlan, {
      paths,
      options: { intervalSecs: seconds, useCameraData },
    })
      .then((next) => current && setPlan({ key, plan: next }))
      .catch((error) => current && setFailure(t("stacks.failed", { error: String(error) })));
    return () => {
      current = false;
    };
  }, [paths, seconds, useCameraData, key, t]);

  const shown = plan?.key === key ? plan.plan : null;
  const apply = async (e: FormEvent) => {
    e.preventDefault();
    if (!shown || !shown.groups.length) return;
    setApplying(true);
    try {
      const result = await invokeInOrder<{ library: string; change: StackChange }>(Invokes.StacksAutoApply, {
        library: shown.library,
        groups: shown.groups,
      });
      try {
        localStorage.setItem(REMEMBERED, String(seconds));
      } catch {
        // A remembered interval is a convenience only.
      }
      stackAction(result.library, result.change, paths);
      await refreshStacks(result.library);
      notify.success(t("stacks.auto.done", { count: result.change.after.length }));
      close();
    } catch (error) {
      setFailure(t("stacks.failed", { error: String(error) }));
      setApplying(false);
    }
  };
  const keys = (e: KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "Escape") close();
  };
  const title = t("stacks.auto.title");
  const kinds = shown
    ? (["bracket", "burst"] as const)
        .map((kind) => [kind, shown.groups.filter((g) => g.kind === kind).length] as const)
        .filter(([, n]) => n > 0)
    : [];

  return createPortal(
    <div className="studio-settings studio-smart-album-backdrop" onKeyDown={keys}>
      <form
        ref={dialog}
        className="studio-capture-time studio-auto-stack"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onSubmit={(e) => void apply(e)}
      >
        <h2>{title}</h2>
        <p className="studio-help">{t("stacks.auto.intro")}</p>
        <label className="studio-auto-stack-interval">
          <span>{t("stacks.auto.interval")}</span>
          <input
            type="range"
            min={0}
            max={INTERVALS.length - 1}
            step={1}
            value={stop}
            aria-valuetext={intervalText(seconds, t)}
            onChange={(e) => setStop(Number(e.target.value))}
          />
          <output>{intervalText(seconds, t)}</output>
        </label>
        <label className="studio-auto-stack-camera">
          <input type="checkbox" checked={useCameraData} onChange={(e) => setUseCameraData(e.target.checked)} />
          <span>{t("stacks.auto.useCameraData")}</span>
        </label>
        <div className="studio-capture-time-preview" role="status" aria-live="polite">
          {shown ? (
            <>
              <p>{t("stacks.auto.preview", { count: shown.groups.length, photos: shown.photos })}</p>
              <p className="studio-help">{t("stacks.auto.single", { count: shown.considered - shown.photos })}</p>
              {kinds.length > 0 && (
                <p className="studio-help">
                  {kinds.map(([kind, n]) => t(`stacks.auto.kind.${kind}`, { count: n })).join(" · ")}
                </p>
              )}
              {shown.considered < paths.length && (
                <p className="studio-help">{t("stacks.auto.leftOut", { count: paths.length - shown.considered })}</p>
              )}
            </>
          ) : (
            <p>{t("stacks.auto.reading")}</p>
          )}
        </div>
        {failure && <p className="studio-capture-time-error" role="alert">{failure}</p>}
        <footer>
          <button type="button" onClick={close}>
            {t("stacks.auto.cancel")}
          </button>
          <button type="submit" className="is-primary" disabled={applying || !shown?.groups.length}>
            {t("stacks.auto.apply", { count: shown?.groups.length ?? 0 })}
          </button>
        </footer>
      </form>
    </div>,
    document.body,
  );
}
