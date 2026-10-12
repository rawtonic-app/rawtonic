import { useTranslation } from "react-i18next";
import { useShallow } from "zustand/react/shallow";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { Panel } from "../ui/AppProperties";

const BACKGROUND = "";

/** The layer the adjustment tools act on, in the viewer bar as in Capture
 * One. Choosing a layer opens Layers with the select tool; Background
 * returns the tools to the photograph. */
export default function StudioLayerSelect() {
  const { t } = useTranslation();
  const { masks, activeMaskContainerId, ready, setEditor } = useEditorStore(
    useShallow((s) => ({
      masks: s.adjustments.masks,
      activeMaskContainerId: s.activeMaskContainerId,
      ready: panelsShowReady(s),
      setEditor: s.setEditor,
    })),
  );
  const activePanel = useUIStore((s) => s.activePanel);
  const current =
    activePanel === Panel.Masks &&
    masks.some((m) => m.id === activeMaskContainerId)
      ? (activeMaskContainerId as string)
      : BACKGROUND;
  return (
    <select
      className="studio-layer-select"
      aria-label={t("studio.layers.select.aria")}
      title={t("studio.layers.select.title")}
      value={current}
      disabled={!ready}
      onChange={(e) => {
        if (!useEditorStore.getState().selectedImage?.isReady) return;
        const id = e.target.value;
        setEditor({
          activeMaskContainerId: id || null,
          activeMaskId: null,
          focusedMaskComponentId: null,
          isWbPickerActive: false,
        });
        if (id) useUIStore.getState().setPanel(Panel.Masks);
      }}
    >
      {/* As in the layer list: the last applied layer first, Background last. */}
      {[...masks].reverse().map((mask) => (
        <option key={mask.id} value={mask.id}>
          {mask.name}
        </option>
      ))}
      <option value={BACKGROUND}>{t("studio.layers.select.background")}</option>

    </select>
  );
}
