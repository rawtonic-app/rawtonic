import { useTranslation } from "react-i18next";
import { Ellipsis, MessageSquareWarning } from "lucide-react";
import { useContextMenu } from "../../context/ContextMenuContext";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import { menuCommandEnabled } from "../../hooks/useAppMenu";
import { overflowCommands } from "../../utils/commandPalette";
import { type Translate } from "../../utils/appMenu";

export default function AppOverflowMenu() {
  const { t } = useTranslation();
  const { showContextMenu } = useContextMenu();
  const platform = useSettingsStore((state) => state.osPlatform);
  const keybinds = useSettingsStore((state) => state.appSettings?.keybinds);
  return (
    <button
      className="studio-bar-icon"
      title={t("studio.workspace.more")}
      aria-label={t("studio.workspace.more")}
      aria-haspopup="menu"
      onClick={(event) => {
        const rect = event.currentTarget.getBoundingClientRect();
        showContextMenu(rect.left, rect.bottom, [
          ...overflowCommands(
            platform,
            t as unknown as Translate,
            keybinds,
            menuCommandEnabled,
          ),
          {
            label: t("contribute.report.menu"),
            icon: MessageSquareWarning,
            onClick: () => useUIStore.getState().setProblemReportOpen(true),
          },
        ]);
      }}
    >
      <Ellipsis size={15} />
    </button>
  );
}
