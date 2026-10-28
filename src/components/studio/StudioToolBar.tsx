import {
  Bandage,
  Brush,
  Circle,
  Crop,
  Eraser,
  Minus,
  MousePointer2,
  Pipette,
  Ruler,
  type LucideIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { useStudioTools, type StudioTool } from "./useStudioTools";
import { useSettingsStore } from "../../store/useSettingsStore";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { shortcutHint } from "../../utils/keyboardUtils";
import { useRadialOptionsMenu } from "./StudioLayers";

/** Names and hints are `studio.tools.bar.<tool>.label` / `.title`. */
const TOOLS: {
  tool: StudioTool;
  icon: LucideIcon;
}[] = [
  { tool: "select", icon: MousePointer2 },
  { tool: "crop", icon: Crop },
  { tool: "straighten", icon: Ruler },
  { tool: "linear", icon: Minus },
  { tool: "radial", icon: Circle },
  { tool: "brush", icon: Brush },
  { tool: "erase", icon: Eraser },
  { tool: "spot", icon: Bandage },
  { tool: "wb", icon: Pipette },
];

const withKey = (title: string, key: string | null) =>
  key ? `${title} (${key})` : title;

/** Cursor tools in the middle of the viewer bar, after Capture One's tool
 * bar. Right-click the photograph for the active tool's settings. */
export default function StudioToolBar() {
  const { t } = useTranslation();
  const { activeTool, selectTool, layer } = useStudioTools();
  // Keeps its look while the next photograph loads; selectTool waits for it.
  const ready = useEditorStore(panelsShowReady);
  const keybinds = useSettingsStore((s) => s.appSettings?.keybinds);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  // Right-click on Radial: Draw Mask Inside for new radials (Capture One).
  const radialMenu = useRadialOptionsMenu();
  return (
    <div
      className="studio-viewer-tools"
      role="toolbar"
      aria-label={t("studio.tools.bar.aria")}
    >
      {TOOLS.map(({ tool, icon: Icon }) => (
        <button
          key={tool}
          aria-label={t(`studio.tools.bar.${tool}.label`)}
          title={withKey(
            ["linear", "radial", "brush", "erase", "spot"].includes(tool)
              ? t("studio.tools.bar.rightClickHint", {
                  hint: t(`studio.tools.bar.${tool}.title`),
                })
              : t(`studio.tools.bar.${tool}.title`),
            shortcutHint(`tool_${tool}`, keybinds, osPlatform),
          )}
          aria-pressed={activeTool === tool}
          disabled={!ready || (tool === "erase" && !layer)}
          onClick={() => selectTool(tool)}
          onContextMenu={tool === "radial" ? radialMenu : undefined}
        >
          <Icon size={15} />
        </button>
      ))}
    </div>
  );
}
