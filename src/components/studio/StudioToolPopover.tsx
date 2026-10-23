import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import StudioLayerControls from "./StudioLayerControls";
import StudioCropControls from "./StudioCropControls";
import StudioSpotControls from "./StudioSpotControls";
import { RotateCcw } from "lucide-react";
import { useStudioTools, type StudioTool } from "./useStudioTools";
import { useEditorStore } from "../../store/useEditorStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { usePopoverFocus } from "./usePopoverFocus";
import { DEFAULT_BRUSH_SIZE } from "../../utils/brushSize";

/** Horizontal distance of the pointer from the panel's left edge on opening. */
const TITLE_POINTER_INSET = 32;

/** The panel's title per cursor tool (`studio.tools.popover.titles.*`). */
const TITLES = {
  brush: "brush",
  erase: "eraser",
  linear: "linear",
  radial: "radial",
  select: "layer",
  crop: "crop",
  straighten: "crop",
  spot: "spot",
} as const satisfies Partial<Record<StudioTool, string>>;

/** Whether a right-click on the photograph opens the tool settings instead of
 * the photograph's context menu: with the crop tool or while a layer's mask
 * tool is active, as in Capture One, where a right-click with a cursor tool
 * shows that tool's settings. The select tool keeps the photograph's menu. */
export function toolPopoverApplies(tools: ReturnType<typeof useStudioTools>) {
  if (tools.activeTool === "crop" || tools.activeTool === "straighten") return tools.ready;
  if (tools.activeTool === "spot") return tools.ready;
  return Boolean(tools.layer) && tools.activeTool !== "select";
}

/** The selected layer's tool settings at the pointer, opened by a right-click
 * on the photograph. Closes on Escape, an outside press or when the layer is
 * deselected. */
export default function StudioToolPopover({
  x,
  y,
  onClose,
}: {
  x: number;
  y: number;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const tools = useStudioTools();
  const { setAdjustments } = useEditorActions();
  const ref = useRef<HTMLDivElement>(null);
  usePopoverFocus(ref);
  const [position, setPosition] = useState({ left: x, top: y });
  const open = toolPopoverApplies(tools);

  // Open with the pointer in the title bar (Capture One), so the first
  // slider is right below it; keep the panel inside the window.
  useLayoutEffect(() => {
    const box = ref.current?.getBoundingClientRect();
    if (!box) return;
    const title = ref.current?.querySelector(".studio-tool-popover-title");
    const titleHeight = title?.getBoundingClientRect().height ?? 24;
    const margin = 8;
    setPosition({
      left: Math.max(
        margin,
        Math.min(x - TITLE_POINTER_INSET, window.innerWidth - box.width - margin),
      ),
      top: Math.max(
        margin,
        Math.min(y - titleHeight / 2, window.innerHeight - box.height - margin),
      ),
    });
  }, [x, y, tools.activeTool, tools.brushActive]);

  useEffect(() => {
    if (!open) onClose();
  }, [open, onClose]);

  useEffect(() => {
    // A press beside the panel closes it and still acts: painting or
    // dragging a shape starts at once, as in Capture One.
    const onPointerDown = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    // Capture phase, so the viewer's own Escape handling (deselecting the
    // layer) does not run for the same key.
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      onClose();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
    };
  }, [onClose]);

  if (!open) return null;
  const titleId: (typeof TITLES)[keyof typeof TITLES] =
    tools.activeTool in TITLES
      ? TITLES[tools.activeTool as keyof typeof TITLES]
      : "layer";
  const title = t(`studio.tools.popover.titles.${titleId}`);
  const cropping = titleId === "crop";
  // The title's reset, as in a tool header: the values this panel shows.
  const reset = (() => {
    if (cropping)
      return {
        label: t("studio.tools.popover.resetCrop"),
        run: () =>
          setAdjustments((prev) => ({ ...prev, crop: null, aspectRatio: null, rotation: 0 })),
      };
    if (tools.brushActive)
      return {
        label: t("studio.tools.popover.resetBrush"),
        run: () =>
          useEditorStore.getState().setEditor({
            brushSettings: {
              ...tools.brushSettings!,
              size: DEFAULT_BRUSH_SIZE,
              hardness: 50,
              opacity: 100,
              flow: 100,
            },
          }),
      };
    if (tools.activeTool === "spot")
      return {
        label: t("studio.tools.popover.resetSpot"),
        run: () =>
          useEditorStore.getState().setEditor({
            spotSettings: { mode: "heal", size: 60, feather: 0, opacity: 100 },
          }),
      };
    const layerId = tools.layer?.id;
    const radialId = tools.radialMask?.id;
    return {
      label: radialId
        ? t("studio.tools.popover.resetOpacityFeather")
        : t("studio.tools.popover.resetOpacity"),
      run: () =>
        setAdjustments((prev) => ({
          ...prev,
          masks: prev.masks.map((m) =>
            m.id !== layerId
              ? m
              : {
                  ...m,
                  opacity: 100,
                  subMasks: m.subMasks.map((sub) =>
                    sub.id === radialId
                      ? { ...sub, parameters: { ...sub.parameters, feather: 0.5 } }
                      : sub,
                  ),
                },
          ),
        })),
    };
  })();
  return createPortal(
    <div
      ref={ref}
      tabIndex={-1}
      className="studio-tool-popover"
      role="dialog"
      aria-label={t("studio.tools.popover.settings", { tool: title })}
      style={{ left: position.left, top: position.top }}
      onContextMenu={(e) => e.preventDefault()}
    >
      <div className="studio-tool-popover-title">
        {title}
        {!cropping && tools.activeTool !== "spot" && (
          <span>{tools.layer?.name}</span>
        )}
        <button
          type="button"
          className="studio-tool-popover-reset"
          title={reset.label}
          aria-label={reset.label}
          onClick={reset.run}
        >
          <RotateCcw size={12} />
        </button>
      </div>
      {cropping ? (
        <StudioCropControls inPopover />
      ) : tools.activeTool === "spot" ? (
        <StudioSpotControls />
      ) : (
        <StudioLayerControls compact />
      )}
    </div>,
    // Inside the workspace, so its variables and control styles apply.
    document.querySelector(".studio-workspace") ?? document.body,
  );
}
