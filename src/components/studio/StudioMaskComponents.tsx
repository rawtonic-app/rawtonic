import type { MouseEvent } from "react";
import { useTranslation } from "react-i18next";
import { detectionPending } from "../../utils/settingsClipboard";
import type { TFunction } from "i18next";
import { Check, Eye, EyeOff, FlipHorizontal2, Minus, Plus, SquaresIntersect, Trash2, type LucideIcon } from "lucide-react";
import { useEditorStore } from "../../store/useEditorStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { openMaskingTools } from "../../hooks/useAiMasking";
import { useContextMenu } from "../../context/ContextMenuContext";
import { useProcessStore } from "../../store/useProcessStore";
import { Mask, SubMaskMode, type SubMask } from "../panel/right/Masks";
import {
  DEFAULT_COLOR_RANGE,
  DEFAULT_DEPTH_RANGE,
  erasesFromLayer,
  kindOf,
  type MaskKind,
} from "../../utils/maskKinds";
import AdjustmentControl from "./AdjustmentControl";
import StudioPeopleMask from "./StudioPeopleMask";
import { useStudioTools } from "./useStudioTools";
import { componentIcon, maskKindMenu } from "./maskKindMenu";
import MaskThumb from "./MaskThumb";
import { useMaskThumbnailStore } from "../../hooks/useMaskThumbnails";
import { useSettingsStore } from "../../store/useSettingsStore";
import { formatKeyCode, shortcutHint } from "../../utils/keyboardUtils";
import { soloAfterSelect, toggleLayerSolo } from "../../utils/layerSolo";

/** What to do on the photo after creating a component of this kind. */
const KIND_HINTS: Partial<Record<MaskKind, "object" | "color">> = {
  object: "object",
  color: "color",
};

/** How a component combines with the ones above it: a glyph per mode, as
 * Lightroom marks a component's mode on its row. */
const MODE_ICONS: Record<SubMaskMode, LucideIcon> = {
  [SubMaskMode.Additive]: Plus,
  [SubMaskMode.Subtractive]: Minus,
  [SubMaskMode.Intersect]: SquaresIntersect,
};

/** Label of a mask kind (MASK_KIND_LABELS, translated). */
export function maskKindLabel(t: TFunction, kind: MaskKind) {
  return t(`studio.masks.kinds.${kind}`);
}

/** Label of a component in the layer's component list (componentLabel,
 * translated). A brush that erases from its layer reads "Erase", whether it
 * subtracts or adds on an inverted layer (`erasesFromLayer`). */
export function maskComponentLabel(t: TFunction, sub: SubMask, layerInverted = false) {
  if (sub.type === Mask.All) return t("studio.masks.kinds.all");
  if (erasesFromLayer(sub, layerInverted)) return t("studio.masks.erase");
  const kind = kindOf(sub);
  return kind ? maskKindLabel(t, kind) : sub.name || sub.type;
}

/** The selected layer, its components (the luma range is edited by the
 * layer's Luma range control) and the component edits shared by the list and
 * the settings. */
function useMaskComponents() {
  const { layer, addComponent, paintOnLayer } = useStudioTools();
  const activeMaskId = useEditorStore((s) => s.activeMaskId);
  const setEditor = useEditorStore((s) => s.setEditor);
  const { setAdjustments } = useEditorActions();
  const components = layer ? layer.subMasks.filter((m) => m.type !== Mask.LumaRange) : [];
  const active = components.find((m) => m.id === activeMaskId);
  const updateComponent = (id: string, patch: Partial<SubMask>) =>
    layer &&
    setAdjustments((prev) => ({
      ...prev,
      masks: prev.masks.map((m) =>
        m.id === layer.id ? { ...m, subMasks: m.subMasks.map((s) => (s.id === id ? { ...s, ...patch } : s)) } : m,
      ),
    }));
  return { layer, components, active, activeMaskId, setEditor, setAdjustments, updateComponent, addComponent, paintOnLayer };
}

/**
 * The components of the selected layer, nested under its row in the layer
 * list (Lightroom's mask components, which Capture One lacks beyond brush and
 * gradients): each with its mode (add, subtract, intersect), invert,
 * visibility and delete; then one line of Add, Subtract and Intersect, each
 * opening the grouped kind menu.
 */
export default function StudioMaskComponents() {
  const { t } = useTranslation();
  const { layer, components, activeMaskId, setEditor, setAdjustments, updateComponent, addComponent, paintOnLayer } =
    useMaskComponents();
  const aiTasks = useProcessStore((s) => s.activeAiTasks);
  const thumbnails = useMaskThumbnailStore((s) => s.components);
  const layerSolo = useEditorStore((s) => s.layerSolo);
  const altKey = formatKeyCode("alt", useSettingsStore((s) => s.osPlatform));
  const { showContextMenu } = useContextMenu();
  if (!layer) return null;
  const layerId = layer.id;

  const removeComponent = (id: string) => {
    setAdjustments((prev) => ({
      ...prev,
      masks: prev.masks.map((m) => (m.id === layerId ? { ...m, subMasks: m.subMasks.filter((s) => s.id !== id) } : m)),
    }));
    if (id === activeMaskId) setEditor({ activeMaskId: null, focusedMaskComponentId: null });
  };
  const below = (e: MouseEvent) => (e.currentTarget as HTMLElement).getBoundingClientRect();
  const kindMenu = (e: MouseEvent, mode: SubMaskMode) => {
    openMaskingTools();
    const rect = below(e);
    showContextMenu(
      rect.left,
      rect.bottom,
      maskKindMenu(
        t,
        (kind) =>
          // Adding and subtracting brushes are the layer's brush and eraser;
          // an intersecting one is a component of its own.
          kind === "brush" && mode !== SubMaskMode.Intersect
            ? paintOnLayer(layer, mode === SubMaskMode.Additive)
            : addComponent(layer, kind, mode),
        (kind) => mode === SubMaskMode.Additive || kind !== "background",
      ),
    );
  };
  const modeMenu = (e: MouseEvent, sub: SubMask) => {
    const rect = below(e);
    showContextMenu(
      rect.left,
      rect.bottom,
      Object.values(SubMaskMode).map((mode) => ({
        label: t(`studio.masks.modes.${mode}`),
        icon: sub.mode === mode ? Check : MODE_ICONS[mode],
        onClick: () => updateComponent(sub.id, { mode }),
      })),
    );
  };
  const busy = (sub: SubMask) => Boolean(aiTasks[sub.id]);
  const componentLabel = (sub: SubMask) => maskComponentLabel(t, sub, Boolean(layer.invert));

  return (
    <div className="studio-mask-components" role="group" aria-label={t("studio.masks.aria.components")}>
      <div className="studio-mask-component-list" role="list">
        {components.map((sub, index) => {
          const Icon = componentIcon(sub, Boolean(layer.invert));
          const ModeIcon = MODE_ICONS[sub.mode] ?? Plus;
          const modeName = t(`studio.masks.modes.${sub.mode}`);
          return (
            <div
              key={sub.id}
              role="listitem"
              className={`studio-mask-component ${sub.id === activeMaskId ? "is-selected" : ""} ${
                sub.visible ? "" : "is-hidden"
              } ${layerSolo?.componentId ? (layerSolo.componentId === sub.id ? "is-solo" : "is-solo-muted") : ""}`}
            >
              {index > 0 ? (
                <button
                  className="studio-mask-mode"
                  aria-haspopup="menu"
                  aria-label={t("studio.masks.aria.modeValue", { name: componentLabel(sub), mode: modeName })}
                  title={t("studio.masks.modeTitle", { mode: modeName })}
                  onClick={(e) => modeMenu(e, sub)}
                >
                  <ModeIcon size={10} strokeWidth={2.5} />
                </button>
              ) : (
                <span className="studio-mask-mode" aria-hidden="true" />
              )}
              <button
                className="studio-mask-component-name"
                aria-label={t("studio.masks.aria.select", { name: componentLabel(sub) })}
                aria-current={sub.id === activeMaskId ? "true" : undefined}
                title={componentLabel(sub)}
                onClick={() =>
                  setEditor({
                    activeMaskId: sub.id,
                    focusedMaskComponentId: sub.id,
                    // A component solo moves to the picked component; a layer solo stays.
                    ...(layerSolo?.componentId ? { layerSolo: soloAfterSelect(layerSolo, layerId, sub.id) } : {}),
                  })
                }
              >
                <MaskThumb kind="component" url={thumbnails[sub.id]} icon={Icon} busy={busy(sub)} />
                <span className="studio-mask-component-label">{componentLabel(sub)}</span>
                {busy(sub) && <span className="studio-mask-busy">· {t("studio.masks.computing")}</span>}
                {!busy(sub) && detectionPending(sub) && (
                  <span
                    className="studio-mask-pending"
                    title={sub.type === "ai-subject" ? t("studio.copySettings.pendingObject") : t("studio.copySettings.pending")}
                  >
                    · {sub.type === "ai-subject" ? t("studio.copySettings.pendingObjectShort") : t("studio.copySettings.pendingShort")}
                  </span>
                )}
              </button>
              <button
                className="studio-row-icon"
                title={t("studio.masks.invertTitle")}
                aria-label={t("studio.masks.aria.invert", { name: componentLabel(sub) })}
                aria-pressed={sub.invert}
                onClick={() => updateComponent(sub.id, { invert: !sub.invert })}
              >
                <FlipHorizontal2 size={11} />
              </button>
              <button
                className="studio-row-icon studio-mask-eye"
                aria-label={t("studio.masks.aria.toggle", { name: componentLabel(sub) })}
                title={t("studio.masks.eyeTitle", { alt: altKey })}
                onClick={(e) =>
                  e.altKey
                    ? setEditor({ layerSolo: toggleLayerSolo(layerSolo, layerId, sub.id) })
                    : updateComponent(sub.id, { visible: !sub.visible })
                }
              >
                {sub.visible ? <Eye size={11} /> : <EyeOff size={11} />}
              </button>
              <button
                className="studio-row-icon studio-row-delete"
                aria-label={t("studio.masks.aria.delete", { name: componentLabel(sub) })}
                onClick={() => removeComponent(sub.id)}
              >
                <Trash2 size={11} />
              </button>
            </div>
          );
        })}
      </div>
      <div className="studio-mask-add" role="group" aria-label={t("studio.masks.aria.combine")}>
        <button aria-haspopup="menu" title={t("studio.masks.addTitle")} onClick={(e) => kindMenu(e, SubMaskMode.Additive)}>
          <Plus size={11} />
          {t("studio.masks.modes.additive")}
        </button>
        <button
          aria-haspopup="menu"
          title={t("studio.masks.subtractTitle")}
          onClick={(e) => kindMenu(e, SubMaskMode.Subtractive)}
        >
          <Minus size={11} />
          {t("studio.masks.modes.subtractive")}
        </button>
        <button
          aria-haspopup="menu"
          title={t("studio.masks.intersectTitle")}
          onClick={(e) => kindMenu(e, SubMaskMode.Intersect)}
        >
          <SquaresIntersect size={11} />
          {t("studio.masks.modes.intersect")}
        </button>
      </div>
    </div>
  );
}

/** The selected component's own settings: Grow and Feather of the automatic
 * selections, the People parts, a colour range's Refine, a depth range's
 * limits. Nothing for the drawn shapes (their layer settings follow). */
export function StudioMaskComponentSettings() {
  const { t } = useTranslation();
  const { layer, active, setEditor, updateComponent } = useMaskComponents();
  const keybinds = useSettingsStore((s) => s.appSettings?.keybinds);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  if (!active) return null;
  const activeKind = kindOf(active);
  const setParameters = (patch: Record<string, unknown>) =>
    updateComponent(active.id, { parameters: { ...active.parameters, ...patch } });
  // These sliders shape the mask (depth range, colour refine, people parts,
  // selection refine): it shows while they are dragged, like the luma range.
  const onInteraction = (dragging: boolean) =>
    setEditor({ isSliderDragging: dragging, isMaskRangeDragging: dragging });
  const hint = activeKind ? KIND_HINTS[activeKind] : undefined;
  const selection =
    activeKind === "subject" || activeKind === "background" || activeKind === "sky" || activeKind === "object";
  if (!hint && !selection && activeKind !== "people" && activeKind !== "color" && activeKind !== "depth") return null;

  return (
    <div
      className="studio-layer-settings-part"
      role="group"
      aria-label={maskComponentLabel(t, active, Boolean(layer?.invert))}
    >
      <p className="studio-layer-settings-caption" aria-hidden="true">
        {maskComponentLabel(t, active, Boolean(layer?.invert))}
      </p>
      {hint && (
        <p className="studio-hint" role="note">
          {t(`studio.masks.hints.${hint}`)}
        </p>
      )}
      {activeKind === "object" && (
        <small className="studio-hint">
          {t("studio.masks.hints.objectMore", {
            shift: formatKeyCode("shift", osPlatform),
            alt: formatKeyCode("alt", osPlatform),
            undo: shortcutHint("undo", keybinds, osPlatform) ?? formatKeyCode("ctrl", osPlatform) + "Z",
          })}
        </small>
      )}
      {selection && (
        <>
          <AdjustmentControl
            label={t("editor.masks.params.grow")}
            min={-100}
            max={100}
            value={Number(active.parameters.grow ?? 0)}
            defaultValue={0}
            onInteraction={onInteraction}
            onChange={(grow) => setParameters({ grow })}
          />
          <AdjustmentControl
            label={t("editor.masks.params.feather")}
            min={0}
            max={100}
            value={Number(active.parameters.feather ?? 0)}
            defaultValue={0}
            onInteraction={onInteraction}
            onChange={(feather) => setParameters({ feather })}
          />
        </>
      )}
      {activeKind === "people" && (
        <StudioPeopleMask sub={active} setParameters={setParameters} onInteraction={onInteraction} />
      )}
      {activeKind === "color" && (
        <>
          {/* Lightroom's Refine, measured in Camera Raw (notes:
              reference/color-range.md); moving it switches an inherited
              tolerance mask to the measured model. */}
          <AdjustmentControl
            label={t("studio.masks.refine")}
            ariaLabel={t("studio.masks.refineAria")}
            min={0}
            max={100}
            value={Number(active.parameters.refine ?? DEFAULT_COLOR_RANGE.refine)}
            defaultValue={DEFAULT_COLOR_RANGE.refine}
            onInteraction={onInteraction}
            onChange={(refine) => setParameters({ refine, tolerance: undefined })}
          />
          <small className="studio-hint">{t("studio.masks.colorSampleHint")}</small>
          <AdjustmentControl
            label={t("editor.masks.params.feather")}
            min={0}
            max={100}
            value={Number(active.parameters.feather ?? DEFAULT_COLOR_RANGE.feather)}
            defaultValue={DEFAULT_COLOR_RANGE.feather}
            onInteraction={onInteraction}
            onChange={(feather) => setParameters({ feather })}
          />
        </>
      )}
      {activeKind === "depth" && (
        <>
          <AdjustmentControl
            label={t("studio.masks.depth.farLimit")}
            min={0}
            max={100}
            unit="%"
            value={Number(active.parameters.minDepth ?? DEFAULT_DEPTH_RANGE.minDepth)}
            defaultValue={DEFAULT_DEPTH_RANGE.minDepth}
            onInteraction={onInteraction}
            onChange={(minDepth) => setParameters({ minDepth })}
          />
          <AdjustmentControl
            label={t("studio.masks.depth.nearLimit")}
            min={0}
            max={100}
            unit="%"
            value={Number(active.parameters.maxDepth ?? DEFAULT_DEPTH_RANGE.maxDepth)}
            defaultValue={DEFAULT_DEPTH_RANGE.maxDepth}
            onInteraction={onInteraction}
            onChange={(maxDepth) => setParameters({ maxDepth })}
          />
          <AdjustmentControl
            label={t("studio.masks.depth.falloff")}
            min={0}
            max={50}
            unit="%"
            value={Number(active.parameters.minFade ?? DEFAULT_DEPTH_RANGE.minFade)}
            defaultValue={DEFAULT_DEPTH_RANGE.minFade}
            onInteraction={onInteraction}
            onChange={(fade) => setParameters({ minFade: fade, maxFade: fade })}
          />
        </>
      )}
    </div>
  );
}
