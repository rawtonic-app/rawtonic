import { useTranslation } from "react-i18next";
import { useShallow } from "zustand/react/shallow";
import { Eye, EyeOff, Trash2 } from "lucide-react";
import AdjustmentControl from "./AdjustmentControl";
import { useEditorStore } from "../../store/useEditorStore";
import { updateAdjustments } from "../../hooks/useEditorActions";
import { useSettingsStore } from "../../store/useSettingsStore";
import { shortcutHint } from "../../utils/keyboardUtils";
import type { Spot } from "../../utils/adjustments";
import { spotItems, withoutSpotItems, withSpotItemsVisible, type SpotItemKind } from "../../utils/spotItems";
import StudioRedEyeControls from "./StudioRedEyeControls";
import { useStudioTools } from "./useStudioTools";

type Settings = ReturnType<typeof useEditorStore.getState>["spotSettings"];

/** The settings new spots get at first (the panels' reset). */
export const resetSpotSettings = () =>
  useEditorStore.getState().setEditor({
    spotSettings: { mode: "heal", size: 60, feather: 0, opacity: 100 },
  });

/** The spot tool in the Retouch tab, as Lightroom's Remove panel: the
 * settings stay visible (the right-click on the photo is a shortcut), a
 * button with the tool's key starts or ends the tool, and the list below
 * shows, hides and deletes what the tool made. */
export function StudioSpotPanel() {
  const { t } = useTranslation();
  const { activeTool, selectTool, ready } = useStudioTools();
  const keybinds = useSettingsStore((s) => s.appSettings?.keybinds);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const key = shortcutHint("tool_spot", keybinds, osPlatform);
  const active = activeTool === "spot";
  const label = t(active ? "studio.spot.done" : "studio.spot.start");
  return (
    <>
      <button
        type="button"
        className="studio-text-button"
        disabled={!ready}
        aria-pressed={active}
        onClick={() => selectTool("spot")}
      >
        {key ? t("studio.spot.withShortcut", { text: label, key }) : label}
      </button>
      <StudioSpotControls />
      <StudioSpotList />
    </>
  );
}

/** What the spot tool made (`spotItems`; Capture One's layer list for its
 * repair layers, but per spot, as Lightroom keeps them): a
 * click selects one on the photo, the eye hides it in the photo and its
 * exports, the bin deletes it; the header does both for all of them. */
export function StudioSpotList() {
  const { t } = useTranslation();
  const { startSpotTool } = useStudioTools();
  const redEye = useEditorStore((s) => s.spotSettings.mode === "redEye");
  // The item lists only: the photo's other values change on every edit.
  const { spots, aiPatches, redEyes } = useEditorStore(
    useShallow((s) => ({
      spots: s.adjustments.spots,
      aiPatches: s.adjustments.aiPatches,
      redEyes: s.adjustments.redEyes,
    })),
  );
  const items = spotItems({ spots, aiPatches, redEyes }, redEye);
  const activeSpotId = useEditorStore((s) => s.activeSpotId);
  if (items.length === 0) return null;
  const all = new Set(items.map((item) => item.id));
  const anyShown = items.some((item) => item.visible);
  const numbers = new Map<SpotItemKind, number>();
  const select = (id: string) => {
    startSpotTool();
    useEditorStore.getState().setEditor({ activeSpotId: id });
  };
  const remove = (ids: Set<string>) => {
    updateAdjustments((prev) => withoutSpotItems(prev, ids));
    const { activeSpotId: selected, setEditor } = useEditorStore.getState();
    if (selected && ids.has(selected)) setEditor({ activeSpotId: null });
  };
  const show = (ids: Set<string>, visible: boolean) =>
    updateAdjustments((prev) => withSpotItemsVisible(prev, ids, visible));
  return (
    <div className="studio-spot-list">
      <div className="studio-spot-list-header">
        <span>{t("spotRemoval.list.count", { number: items.length })}</span>
        <button
          type="button"
          className="studio-row-icon"
          aria-label={t(anyShown ? "spotRemoval.list.hideAll" : "spotRemoval.list.showAll")}
          title={t(anyShown ? "spotRemoval.list.hideAll" : "spotRemoval.list.showAll")}
          onClick={() => show(all, !anyShown)}
        >
          {anyShown ? <Eye size={12} /> : <EyeOff size={12} />}
        </button>
        <button
          type="button"
          className="studio-row-icon"
          aria-label={t("spotRemoval.list.deleteAll")}
          title={t("spotRemoval.list.deleteAll")}
          onClick={() => remove(all)}
        >
          <Trash2 size={12} />
        </button>
      </div>
      <div className="studio-layer-list" role="list" aria-label={t("spotRemoval.list.aria")}>
        {items.map((item) => {
          const number = (numbers.get(item.kind) ?? 0) + 1;
          numbers.set(item.kind, number);
          const kind =
            item.kind === "red" || item.kind === "pet"
              ? t(`spotRemoval.redEye.kinds.${item.kind}`)
              : t(`spotRemoval.modes.${item.kind}`);
          const name = t("spotRemoval.list.item", { kind, number });
          const selected = item.id === activeSpotId;
          return (
            <div
              key={item.id}
              role="listitem"
              className={`studio-layer ${selected ? "is-selected" : ""} ${item.visible ? "" : "is-hidden"}`}
            >
              <button
                type="button"
                className="studio-layer-name"
                aria-label={t("spotRemoval.list.select", { name })}
                aria-current={selected ? "true" : undefined}
                onClick={() => select(item.id)}
              >
                <span className="studio-layer-label">{name}</span>
              </button>
              <button
                type="button"
                className="studio-row-icon"
                aria-label={t("spotRemoval.list.toggle", { name })}
                aria-pressed={item.visible}
                onClick={() => show(new Set([item.id]), !item.visible)}
              >
                {item.visible ? <Eye size={12} /> : <EyeOff size={12} />}
              </button>
              <button
                type="button"
                className="studio-row-icon studio-row-delete"
                aria-label={t("spotRemoval.list.delete", { name })}
                onClick={() => remove(new Set([item.id]))}
              >
                <Trash2 size={12} />
              </button>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** Spot removal settings (Lightroom's Remove tool): Remove (the area filled
 * by the local inpainting model, an AI patch), Heal or Clone, size, feather
 * and opacity. They apply to the selected spot and are what new spots get.
 * Remove's Detect objects (Lightroom's object-aware Remove) selects an
 * object by a click, a loose brush stroke over it or a box around it;
 * Shift adds to the selected removal and Alt subtracts from it. Red eye
 * places red eye and pet eye corrections (`StudioRedEyeControls`). */
export default function StudioSpotControls() {
  const { t } = useTranslation();
  const settings = useEditorStore((s) => s.spotSettings);
  const visualize = useEditorStore((s) => s.spotVisualize);
  const keybinds = useSettingsStore((s) => s.appSettings?.keybinds);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const setVisualize = (patch: Partial<typeof visualize>) =>
    useEditorStore.getState().setEditor({ spotVisualize: { ...visualize, ...patch } });
  // Removals (the AI patches Remove made): shown unless all are hidden.
  const hasRemovals = useEditorStore((s) => Boolean(s.adjustments.aiPatches?.some((p) => p.name === "Remove")));
  const removalsShown = useEditorStore((s) =>
    Boolean(s.adjustments.aiPatches?.some((p) => p.name === "Remove" && p.visible !== false)),
  );
  const activeSpot = useEditorStore((s) =>
    s.adjustments.spots?.find((spot) => spot.id === s.activeSpotId),
  );
  const redEye = settings.mode === "redEye";
  const shown: Settings = activeSpot && !redEye
    ? {
        mode: activeSpot.mode,
        size: Math.round(activeSpot.radius),
        feather: activeSpot.feather,
        opacity: activeSpot.opacity,
      }
    : settings;
  const detectObjects = shown.mode === "remove" && Boolean(settings.detectObjects);
  const objectSelect = settings.objectSelect ?? "brush";
  const objectsKey = shortcutHint("remove_objects", keybinds, osPlatform);
  const { startSpotTool } = useStudioTools();
  // The selected spot, removal or red eye correction (Delete removes it).
  const selectedId = useEditorStore((s) =>
    s.activeSpotId && spotItems(s.adjustments, redEye).some((item) => item.id === s.activeSpotId)
      ? s.activeSpotId
      : null,
  );

  const set = (patch: Partial<Settings>) => {
    // Choosing a mode (or Detect objects) is choosing the tool, as in
    // Lightroom; the size and the other sliders only set what it does.
    if (patch.mode !== undefined || patch.detectObjects) startSpotTool();
    const { setEditor, spotSettings } = useEditorStore.getState();
    // Into or out of Red eye: the selection belongs to the other mode.
    const switching =
      patch.mode !== undefined && (patch.mode === "redEye") !== (spotSettings.mode === "redEye");
    setEditor({
      spotSettings: { ...spotSettings, ...patch },
      ...(switching && { activeSpotId: null }),
    });
    if (!activeSpot || switching) return;
    // A spot stays heal or clone; Remove makes new removals only.
    const spotPatch: Partial<Spot> = {
      ...(patch.mode &&
        patch.mode !== "remove" &&
        patch.mode !== "redEye" && { mode: patch.mode }),
      ...(patch.size !== undefined && { radius: patch.size }),
      ...(patch.feather !== undefined && { feather: patch.feather }),
      ...(patch.opacity !== undefined && { opacity: patch.opacity }),
    };
    updateAdjustments((prev) => ({
      ...prev,
      spots: (prev.spots ?? []).map((s) =>
        s.id === activeSpot.id ? { ...s, ...spotPatch } : s,
      ),
    }));
  };

  return (
    <div className="studio-spot-controls">
      <div className="studio-segments" role="radiogroup" aria-label={t("spotRemoval.mode")}>
        {(["remove", "heal", "clone", "redEye"] as const).map((mode) => (
          <button
            key={mode}
            type="button"
            role="radio"
            aria-checked={shown.mode === mode}
            onClick={() => set({ mode })}
          >
            {t(`spotRemoval.modes.${mode}`)}
          </button>
        ))}
      </div>
      {redEye && <StudioRedEyeControls />}
      {shown.mode === "remove" && (
        <label
          className="studio-inline studio-check"
          title={
            objectsKey
              ? t("studio.spot.withShortcut", {
                  text: t("spotRemoval.detectObjectsHint"),
                  key: objectsKey,
                })
              : t("spotRemoval.detectObjectsHint")
          }
        >
          <input
            type="checkbox"
            checked={detectObjects}
            onChange={(e) => set({ detectObjects: e.target.checked })}
          />
          {t("spotRemoval.detectObjects")}
        </label>
      )}
      {redEye ? null : detectObjects ? (
        <>
          <div
            className="studio-segments"
            role="radiogroup"
            aria-label={t("spotRemoval.objectSelect")}
          >
            {(["brush", "box"] as const).map((way) => (
              <button
                key={way}
                type="button"
                role="radio"
                aria-checked={objectSelect === way}
                onClick={() => set({ objectSelect: way })}
              >
                {t(`spotRemoval.objectSelectModes.${way}`)}
              </button>
            ))}
          </div>
          {objectSelect === "brush" && (
            <AdjustmentControl
              label={t("spotRemoval.size")}
              ariaLabel={t("spotRemoval.sizeAria")}
              value={shown.size}
              min={2}
              max={1000}
              step={1}
              defaultValue={60}
              unit=" px"
              onChange={(size) => set({ size })}
            />
          )}
          <p className="studio-hint">
            {t(
              objectSelect === "brush"
                ? "spotRemoval.detectObjectsHint"
                : "spotRemoval.detectObjectsBoxHint",
            )}{" "}
            {t("spotRemoval.detectObjectsRefineHint")}
          </p>
        </>
      ) : (
        <>
          <AdjustmentControl
            label={t("spotRemoval.size")}
            ariaLabel={t("spotRemoval.sizeAria")}
            value={shown.size}
            min={2}
            max={1000}
            step={1}
            defaultValue={60}
            unit=" px"
            onChange={(size) => set({ size })}
          />
          {shown.mode !== "remove" && (
            <>
              <AdjustmentControl
                label={t("spotRemoval.feather")}
                ariaLabel={t("spotRemoval.featherAria")}
                value={shown.feather}
                min={0}
                max={100}
                defaultValue={0}
                onChange={(feather) => set({ feather })}
              />
              <AdjustmentControl
                label={t("spotRemoval.opacity")}
                ariaLabel={t("spotRemoval.opacityAria")}
                value={shown.opacity}
                min={0}
                max={100}
                defaultValue={100}
                onChange={(opacity) => set({ opacity })}
              />
            </>
          )}
        </>
      )}
      {selectedId && (
        <button
          type="button"
          className="studio-text-button"
          title={t("spotRemoval.deleteSelectedHint")}
          onClick={() => {
            updateAdjustments((prev) => withoutSpotItems(prev, new Set([selectedId])));
            useEditorStore.getState().setEditor({ activeSpotId: null });
          }}
        >
          {t("spotRemoval.deleteSelected")}
        </button>
      )}
      {!redEye && hasRemovals && (
        // The tool's eye: hides every removal in the
        // photo and its exports (the patches stay in the recipe).
        <label className="studio-inline studio-check">
          <input
            type="checkbox"
            checked={removalsShown}
            onChange={(e) => {
              const visible = e.target.checked;
              updateAdjustments((prev) => ({
                ...prev,
                aiPatches: (prev.aiPatches ?? []).map((p) => (p.name === "Remove" ? { ...p, visible } : p)),
              }));
            }}
          />
          {t("spotRemoval.showRemovals")}
        </label>
      )}
      {!redEye && (
        <label className="studio-inline studio-check">
          <input
            type="checkbox"
            checked={visualize.on}
            onChange={(e) => setVisualize({ on: e.target.checked })}
          />
          {t("spotRemoval.visualize")}
        </label>
      )}
      {!redEye && visualize.on && (
        <AdjustmentControl
          label={t("spotRemoval.sensitivity")}
          ariaLabel={t("spotRemoval.sensitivityAria")}
          value={visualize.sensitivity}
          min={0}
          max={100}
          defaultValue={50}
          onChange={(sensitivity) => setVisualize({ sensitivity })}
        />
      )}
    </div>
  );
}
