import { Fragment, useState, type MouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowDown,
  ArrowUp,
  Check,
  Copy,
  Eraser,
  Eye,
  EyeOff,
  FlipHorizontal2,
  Layers,
  PenLine,
  Plus,
  Trash2,
} from "lucide-react";
import { useEditorStore } from "../../store/useEditorStore";
import { useProcessStore } from "../../store/useProcessStore";
import { useUIStore } from "../../store/useUIStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { openMaskingTools } from "../../hooks/useAiMasking";
import { useContextMenu } from "../../context/ContextMenuContext";
import { shortcutHint } from "../../utils/keyboardUtils";
import { OPTION_SEPARATOR, Panel } from "../ui/AppProperties";
import type { MaskContainer } from "../../utils/adjustments";
import type { MaskKind } from "../../utils/maskKinds";
import StudioLayerControls from "./StudioLayerControls";
import StudioMaskComponents, { StudioMaskComponentSettings } from "./StudioMaskComponents";
import { MASK_KIND_GROUPS, MASK_KIND_ICONS, componentIcon, maskKindMenu } from "./maskKindMenu";
import { useStudioTools } from "./useStudioTools";
import { createDragGhost } from "./dragGhost";
import MaskThumb from "./MaskThumb";
import { useMaskThumbnailStore, useMaskThumbnailUpdates } from "../../hooks/useMaskThumbnails";

/** Drop target id of the Background row in the layer list. */
const BACKGROUND_ROW = "background";

/** The radial's option (Capture One's Draw Mask Inside): which side a new
 * radial covers, kept in the settings. */
function useRadialInside() {
  const appSettings = useSettingsStore((s) => s.appSettings);
  const handleSettingsChange = useSettingsStore((s) => s.handleSettingsChange);
  const inside = appSettings?.radialDrawInside !== false;
  const toggle = () => appSettings && handleSettingsChange({ ...appSettings, radialDrawInside: !inside });
  return { inside, toggle };
}

/** The radial's right-click menu (the new-layer grid's tile and the viewer
 * bar's Radial tool). */
export function useRadialOptionsMenu() {
  const { t } = useTranslation();
  const { showContextMenu } = useContextMenu();
  const { inside, toggle } = useRadialInside();
  return (e: MouseEvent) => {
    e.preventDefault();
    showContextMenu(e.clientX, e.clientY, [
      {
        label: t("studio.develop.layers.drawMaskInside"),
        icon: inside ? Check : undefined,
        onClick: toggle,
      },
    ]);
  };
}

/** The "+" in the Layers & masks header (Capture One's new layer, with
 * Lightroom's grouped Create New Mask menu): a new layer of any kind. */
export function NewLayerButton() {
  const { t } = useTranslation();
  const { addLayer, ready } = useStudioTools();
  const { showContextMenu } = useContextMenu();
  return (
    <button
      type="button"
      className="studio-layer-new"
      aria-label={t("studio.develop.layers.new")}
      aria-haspopup="menu"
      title={t("studio.develop.layers.newTitle")}
      disabled={!ready}
      // Pointing at it starts Subject and Sky in the background, so the
      // automatic selections are ready when picked.
      onPointerEnter={() => openMaskingTools()}
      onClick={(e) => {
        e.preventDefault();
        openMaskingTools();
        const rect = e.currentTarget.getBoundingClientRect();
        showContextMenu(rect.left, rect.bottom + 2, maskKindMenu(t, (kind) => addLayer(kind)));
      }}
    >
      <Plus size={13} />
    </button>
  );
}

/** With no layers yet: every kind as a tile, in three captioned groups
 * (Lightroom's Create New Mask grid). */
function NewLayerGrid() {
  const { t } = useTranslation();
  const { addLayer, ready } = useStudioTools();
  const radialMenu = useRadialOptionsMenu();
  const { inside } = useRadialInside();
  const keybinds = useSettingsStore((s) => s.appSettings?.keybinds);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  // Subject and Background have keys of their own (mask_subject, mask_background).
  const titleOf = (kind: MaskKind) => {
    const title = t(`studio.develop.layers.newKind.${kind}`);
    const key = kind === "subject" || kind === "background" ? shortcutHint(`mask_${kind}`, keybinds, osPlatform) : null;
    return key ? `${title} (${key})` : title;
  };
  // Short names: the group caption says "Range".
  const label = (kind: MaskKind) => t(`studio.develop.layers.kindButtons.${kind}`);
  return (
    <div className="studio-layer-grid" role="group" aria-label={t("studio.develop.layers.new")}>
      {MASK_KIND_GROUPS.map(({ group, kinds }) => (
        <div
          key={group}
          className="studio-layer-grid-group"
          role="group"
          aria-labelledby={`studio-layer-grid-${group}`}
          // Pointing at the selections starts Subject and Sky in the
          // background (instant Subject/Background/Sky).
          onPointerEnter={group === "select" ? () => openMaskingTools() : undefined}
          onFocus={group === "select" ? () => openMaskingTools() : undefined}
        >
          <p className="studio-layer-grid-caption" id={`studio-layer-grid-${group}`}>
            {t(`studio.develop.layers.groups.${group}`)}
          </p>
          <div className="studio-layer-grid-tiles">
            {kinds.map((kind) => {
              const Icon = MASK_KIND_ICONS[kind];
              return (
                <button
                  key={kind}
                  type="button"
                  className="studio-layer-tile"
                  disabled={!ready}
                  aria-label={t(`studio.develop.layers.newKind.${kind}`)}
                  title={
                    kind === "radial"
                      ? inside
                        ? t("studio.develop.layers.newRadialInside")
                        : t("studio.develop.layers.newRadialOutside")
                      : titleOf(kind)
                  }
                  onClick={() => addLayer(kind)}
                  onContextMenu={kind === "radial" ? radialMenu : undefined}
                >
                  <Icon size={16} aria-hidden="true" />
                  <span>{label(kind)}</span>
                </button>
              );
            })}
          </div>
        </div>
      ))}
    </div>
  );
}

/**
 * The Layers & masks tool's body: Capture One's layer list (top = applied
 * last, Background at the bottom) with Lightroom's components nested under
 * the selected layer; with no layers, the new-layer grid. Below a divider the
 * selection's settings: the selected component's, then the layer's.
 */
export default function StudioLayers({
  compact = false,
}: {
  /** Among the other tools (the Adjust tab): no layers yet shows one line
   * instead of the New layer grid, which the header's + still opens. */
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const masks = useEditorStore((s) => s.adjustments.masks);
  const setEditor = useEditorStore((s) => s.setEditor);
  const { setAdjustments } = useEditorActions();
  const { showContextMenu } = useContextMenu();
  const { layer, paintOnLayer, toggleInvert } = useStudioTools();
  const [renaming, setRenaming] = useState<string | null>(null);
  useMaskThumbnailUpdates();
  const thumbnails = useMaskThumbnailStore((s) => s.layers);
  const aiTasks = useProcessStore((s) => s.activeAiTasks);
  const [layerDrag, setLayerDrag] = useState<{
    id: string;
    target: string | null;
    above: boolean;
  } | null>(null);

  const selectLayer = (mask: MaskContainer) => {
    setEditor({
      activeMaskContainerId: mask.id,
      activeMaskId: mask.subMasks[0]?.id || null,
      focusedMaskComponentId: null,
      isWbPickerActive: false,
    });
    useUIStore.getState().setPanel(Panel.Masks);
  };
  // Lightroom's Duplicate and Duplicate and Invert: a copy above the layer,
  // with new ids for it and its components.
  const duplicateLayer = (target: MaskContainer, invert: boolean) => {
    const copy = {
      ...structuredClone(target),
      id: crypto.randomUUID(),
      name: t("studio.develop.layers.copyName", { name: target.name }),
      invert: invert ? !target.invert : target.invert,
    };
    copy.subMasks = copy.subMasks.map((sub) => ({ ...sub, id: crypto.randomUUID() }));
    setAdjustments((prev) => {
      const at = prev.masks.findIndex((m) => m.id === target.id);
      return { ...prev, masks: [...prev.masks.slice(0, at + 1), copy, ...prev.masks.slice(at + 1)] };
    });
    setEditor({
      activeMaskContainerId: copy.id,
      activeMaskId: copy.subMasks[0]?.id ?? null,
      focusedMaskComponentId: null,
    });
    useUIStore.getState().setPanel(Panel.Masks);
  };
  const renameLayer = (id: string, name: string) => {
    setRenaming(null);
    const trimmed = name.trim();
    if (!trimmed) return;
    setAdjustments((prev) => ({
      ...prev,
      masks: prev.masks.map((m) => (m.id === id ? { ...m, name: trimmed } : m)),
    }));
  };
  const deleteLayer = (id: string) => {
    setAdjustments((prev) => ({ ...prev, masks: prev.masks.filter((m) => m.id !== id) }));
    setEditor({ activeMaskContainerId: null, activeMaskId: null });
  };
  // Layers apply in list order from the bottom (Background) up, as in
  // Photoshop: the top row is applied last. `masks` is in application order.
  const reorderLayers = (order: string[]) =>
    setAdjustments((prev) => {
      const byId = new Map(prev.masks.map((m) => [m.id, m]));
      const next = order.flatMap((id) => byId.get(id) ?? []);
      return next.length === prev.masks.length ? { ...prev, masks: next } : prev;
    });
  const moveLayer = (id: string, step: number) => {
    const order = masks.map((m) => m.id);
    const at = order.indexOf(id);
    const to = at + step;
    if (at < 0 || to < 0 || to >= order.length) return;
    order.splice(at, 1);
    order.splice(to, 0, id);
    reorderLayers(order);
  };
  const layerMenu = (e: MouseEvent, target: MaskContainer) => {
    e.preventDefault();
    showContextMenu(e.clientX, e.clientY, [
      {
        label: t("editor.masks.settings.invertMask"),
        icon: target.invert ? Check : FlipHorizontal2,
        onClick: () => toggleInvert(target),
      },
      {
        label: t("studio.develop.layers.menu.drawMask"),
        icon: PenLine,
        onClick: () => paintOnLayer(target, true),
      },
      {
        label: t("studio.develop.layers.menu.eraseMask"),
        icon: Eraser,
        onClick: () => paintOnLayer(target, false),
      },
      { type: OPTION_SEPARATOR },
      {
        label: t("studio.develop.layers.menu.rename"),
        icon: PenLine,
        onClick: () => setRenaming(target.id),
      },
      {
        label: t("studio.develop.layers.menu.duplicate"),
        icon: Copy,
        onClick: () => duplicateLayer(target, false),
      },
      {
        label: t("studio.develop.layers.menu.duplicateInvert"),
        icon: FlipHorizontal2,
        onClick: () => duplicateLayer(target, true),
      },
      { type: OPTION_SEPARATOR },
      {
        label: t("studio.develop.layers.menu.moveUp"),
        icon: ArrowUp,
        disabled: masks.at(-1)?.id === target.id,
        onClick: () => moveLayer(target.id, 1),
      },
      {
        label: t("studio.develop.layers.menu.moveDown"),
        icon: ArrowDown,
        disabled: masks[0]?.id === target.id,
        onClick: () => moveLayer(target.id, -1),
      },
      { type: OPTION_SEPARATOR },
      {
        label: t("studio.develop.layers.delete", { name: target.name }),
        icon: Trash2,
        isDestructive: true,
        onClick: () => deleteLayer(target.id),
      },
    ]);
  };
  const pressLayer = (e: ReactPointerEvent<HTMLElement>, mask: MaskContainer) => {
    if (e.button !== 0 || (e.target as Element).closest("button:not(:first-child), input")) return;
    const start = { x: e.clientX, y: e.clientY };
    let ghost: ReturnType<typeof createDragGhost> | null = null;
    let drop: { target: string | null; above: boolean } = {
      target: null,
      above: true,
    };
    const rowAt = (x: number, y: number) => {
      const row = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-layer-row]");
      if (!row?.dataset.layerRow) return { target: null, above: true };
      const rect = row.getBoundingClientRect();
      return {
        target: row.dataset.layerRow,
        above: !rect.height || y < rect.top + rect.height / 2,
      };
    };
    const move = (ev: PointerEvent) => {
      if (!ghost) {
        if (Math.hypot(ev.clientX - start.x, ev.clientY - start.y) < 5) return;
        ghost = createDragGhost({ label: mask.name, kind: "layer" });
      }
      ghost.move(ev.clientX, ev.clientY);
      drop = rowAt(ev.clientX, ev.clientY);
      setLayerDrag({ id: mask.id, ...drop });
    };
    const end = (ev: PointerEvent) => {
      document.removeEventListener("pointermove", move, true);
      document.removeEventListener("pointerup", end, true);
      document.removeEventListener("pointercancel", end, true);
      if (!ghost) return;
      ghost.remove();
      setLayerDrag(null);
      // The press ended a drag, not a click that selects the layer.
      const swallow = (click: globalThis.MouseEvent) => {
        click.stopPropagation();
        click.preventDefault();
      };
      window.addEventListener("click", swallow, { capture: true, once: true });
      setTimeout(() => window.removeEventListener("click", swallow, { capture: true }), 0);
      if (ev.type !== "pointerup" || !drop.target || drop.target === mask.id) return;
      // Rows show the top layer first: work in display order.
      const shown = masks
        .map((m) => m.id)
        .reverse()
        .filter((id) => id !== mask.id);
      if (drop.target === BACKGROUND_ROW) shown.push(mask.id);
      else {
        const at = shown.indexOf(drop.target);
        shown.splice(drop.above ? at : at + 1, 0, mask.id);
      }
      reorderLayers(shown.reverse());
    };
    document.addEventListener("pointermove", move, true);
    document.addEventListener("pointerup", end, true);
    document.addEventListener("pointercancel", end, true);
  };

  if (masks.length === 0)
    return compact ? (
      <p className="studio-help">{t("studio.develop.layers.noneYet")}</p>
    ) : (
      <NewLayerGrid />
    );

  return (
    <>
      <div className="studio-layer-list" role="list" aria-label={t("studio.develop.layers.listAria")}>
        {[...masks].reverse().map((mask) => {
          const selected = layer?.id === mask.id;
          const Icon = componentIcon(mask.subMasks[0]) ?? Layers;
          return (
            <Fragment key={mask.id}>
              <div
                role="listitem"
                data-layer-row={mask.id}
                className={`studio-layer ${selected ? "is-selected" : ""} ${mask.visible ? "" : "is-hidden"} ${
                  layerDrag?.id === mask.id ? "is-dragged" : ""
                } ${
                  layerDrag?.target === mask.id && layerDrag.id !== mask.id
                    ? layerDrag.above
                      ? "is-drop-before"
                      : "is-drop-after"
                    : ""
                }`}
                title={t("studio.develop.layers.dragOrder")}
                onContextMenu={(e) => layerMenu(e, mask)}
                onPointerDown={(e) => pressLayer(e, mask)}
              >
                <button
                  className="studio-layer-name"
                  aria-label={t("studio.develop.layers.select", { name: mask.name })}
                  aria-current={selected ? "true" : undefined}
                  onClick={() => selectLayer(mask)}
                  onDoubleClick={() => setRenaming(mask.id)}
                >
                  <MaskThumb
                    kind="layer"
                    url={thumbnails[mask.id]}
                    icon={Icon}
                    busy={mask.subMasks.some((sub) => Boolean(aiTasks[sub.id]))}
                  />
                  {renaming !== mask.id && <span className="studio-layer-label">{mask.name}</span>}
                </button>
                {renaming === mask.id && (
                  <input
                    autoFocus
                    aria-label={t("studio.develop.layers.nameOf", { name: mask.name })}
                    defaultValue={mask.name}
                    onPointerDown={(e) => e.stopPropagation()}
                    onBlur={(e) => renameLayer(mask.id, e.currentTarget.value)}
                    onKeyDown={(e) => {
                      e.stopPropagation();
                      if (e.key === "Enter") renameLayer(mask.id, e.currentTarget.value);
                      if (e.key === "Escape") setRenaming(null);
                    }}
                  />
                )}
                <button
                  className="studio-row-icon studio-layer-eye"
                  aria-label={t("studio.develop.layers.toggle", { name: mask.name })}
                  onClick={() =>
                    setAdjustments((prev) => ({
                      ...prev,
                      masks: prev.masks.map((m) => (m.id === mask.id ? { ...m, visible: !m.visible } : m)),
                    }))
                  }
                >
                  {mask.visible ? <Eye size={12} /> : <EyeOff size={12} />}
                </button>
                <button
                  className="studio-row-icon studio-row-delete"
                  aria-label={t("studio.develop.layers.delete", { name: mask.name })}
                  onClick={() => deleteLayer(mask.id)}
                >
                  <Trash2 size={12} />
                </button>
              </div>
              {selected && <StudioMaskComponents />}
            </Fragment>
          );
        })}
        <div role="listitem" className="studio-layer-background-item">
          <button
            className={`studio-layer studio-layer-background ${!layer ? "is-selected" : ""} ${
              layerDrag && layerDrag.target === BACKGROUND_ROW ? "is-drop-before" : ""
            }`}
            data-layer-row={BACKGROUND_ROW}
            aria-current={!layer ? "true" : undefined}
            onClick={() => setEditor({ activeMaskContainerId: null, activeMaskId: null })}
          >
            <span className="studio-mask-thumb is-layer is-plain" aria-hidden="true">
              <Layers size={13} />
            </span>
            <span className="studio-layer-label">{t("studio.develop.layers.background")}</span>
            <span className="studio-layer-note">{t("studio.develop.layers.image")}</span>
          </button>
        </div>
      </div>
      {layer && (
        <div className="studio-layer-settings">
          <StudioMaskComponentSettings />
          <div className="studio-layer-settings-part" role="group" aria-label={layer.name}>
            <p className="studio-layer-settings-caption" aria-hidden="true">
              {layer.name}
            </p>
            <StudioLayerControls />
          </div>
        </div>
      )}
    </>
  );
}
