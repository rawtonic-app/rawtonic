import { Camera, History } from "lucide-react";
import { useTranslation } from "react-i18next";
import { notify } from "../../activity/notify";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { takeSnapshot } from "../../utils/snapshots";
import { ToolGroup } from "./StudioDevelop";
import { describeHistoryStep, historyStepName } from "./historyLabels";

/** Labels of steps already named: a step's recipe object never changes, so
 * each new step is described once rather than every step on every change. */
const labels = new WeakMap<object, { before: object; language: string; label: string }>();

/** Lightroom's Develop history: every recorded step of the open photograph,
 * named by what it changed; a click returns to that step (undo/redo move
 * through the same list), the camera takes a snapshot of it. */
export default function StudioHistory() {
  const { t, i18n } = useTranslation();
  const image = useEditorStore((s) => s.selectedImage);
  const history = useEditorStore((s) => s.history);
  const historyIndex = useEditorStore((s) => s.historyIndex);
  const panelsReady = useEditorStore(panelsShowReady);
  return (
    <ToolGroup
      title="History"
      label={t("studio.develop.history.title")}
      open={false}
    >
      {image ? (
        history.map((entry, i) => {
          let label: string;
          const named = historyStepName(entry, t);
          if (i === 0) {
            label = t("studio.develop.history.opened");
          } else if (named) {
            label = named;
          } else {
            const known = labels.get(entry);
            if (known && known.before === history[i - 1] && known.language === i18n.language) {
              label = known.label;
            } else {
              label = describeHistoryStep(history[i - 1], entry, t, i18n.language);
              labels.set(entry, { before: history[i - 1], language: i18n.language, label });
            }
          }
          return (
          <div key={i} className="studio-history-row">
            <button
              title={label}
              className={`studio-history ${i === historyIndex ? "is-selected" : ""}`}
              disabled={!panelsReady}
              onClick={() => {
                const editor = useEditorStore.getState();
                if (editor.selectedImage?.isReady) editor.goToHistoryIndex(i);
              }}
            >
              <History size={12} />
              <span className="studio-history-label">{label}</span>
              <span>
                {i === historyIndex ? t("studio.develop.history.current") : ""}
              </span>
            </button>
            <button
              type="button"
              className="studio-history-snapshot"
              disabled={!panelsReady}
              title={t("studio.develop.snapshots.fromStep", { step: label })}
              aria-label={t("studio.develop.snapshots.fromStep", { step: label })}
              onClick={() => {
                takeSnapshot(label, entry).catch((error) =>
                  notify.error(t("studio.develop.snapshots.failed", { error: String(error) })),
                );
              }}
            >
              <Camera size={11} />
            </button>
          </div>
          );
        })
      ) : (
        <p className="studio-help">{t("studio.develop.history.empty")}</p>
      )}
    </ToolGroup>
  );
}
