import { History } from "lucide-react";
import { useTranslation } from "react-i18next";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { ToolGroup } from "./StudioDevelop";
import { describeHistoryStep } from "./historyLabels";

/** Lightroom's Develop history: every recorded step of the open photograph,
 * named by what it changed; a click returns to that step (undo/redo move
 * through the same list). */
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
          const label =
            i === 0
              ? t("studio.develop.history.opened")
              : describeHistoryStep(history[i - 1], entry, t, i18n.language);
          return (
          <button
            key={i}
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
          );
        })
      ) : (
        <p className="studio-help">{t("studio.develop.history.empty")}</p>
      )}
    </ToolGroup>
  );
}
