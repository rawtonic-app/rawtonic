import { useTranslation } from "react-i18next";
import { Ellipsis } from "lucide-react";
import { useContextMenu } from "../../context/ContextMenuContext";
import { useSettingsStore } from "../../store/useSettingsStore";
import { menuCommandEnabled } from "../../hooks/useAppMenu";
import { hasOverflowMenu, overflowCommands } from "../../utils/commandPalette";
import { type Translate } from "../../utils/appMenu";

export default function AppOverflowMenu() {
  const { t } = useTranslation();
  const { showContextMenu } = useContextMenu();
  const platform = useSettingsStore((state) => state.osPlatform);
  const keybinds = useSettingsStore((state) => state.appSettings?.keybinds);
  // Without a native menu bar (Linux, Windows) ⋯ holds the whole menu tree,
  // Help › Report a Problem… included; macOS has it all in the menu bar.
  if (!hasOverflowMenu(platform)) return null;
  return (
    <button
      className="studio-bar-icon"
      title={t("studio.workspace.more")}
      aria-label={t("studio.workspace.more")}
      aria-haspopup="menu"
      onClick={(event) => {
        const rect = event.currentTarget.getBoundingClientRect();
        showContextMenu(
          rect.left,
          rect.bottom,
          overflowCommands(platform, t as unknown as Translate, keybinds, menuCommandEnabled),
        );
      }}
    >
      <Ellipsis size={15} />
    </button>
  );
}
