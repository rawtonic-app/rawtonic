import { useTranslation } from "react-i18next";
import { LayoutGrid, Layers, Lock, LockOpen } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { useMultiViewStore } from "../../store/useMultiViewStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { shortcutHint } from "../../utils/keyboardUtils";

/**
 * The viewer toolbar's multi view switch (N) and, while several photographs
 * are shown, the "Edit all selected" switch: visible, off by default, and
 * every cell's frame turns orange while it is on.
 */
export default function MultiViewButtons() {
  const { t } = useTranslation();
  const { enabled, active, editAll, unavailable, toggle, setEditAll, zoomLock, setZoomLock } = useMultiViewStore(
    useShallow((s) => ({
      enabled: s.enabled,
      active: s.active,
      editAll: s.editAll,
      unavailable: s.unavailable,
      toggle: s.toggle,
      setEditAll: s.setEditAll,
      zoomLock: s.zoomLock,
      setZoomLock: s.setZoomLock,
    })),
  );
  const { keybinds, osPlatform, useWgpu } = useSettingsStore(
    useShallow((s) => ({
      keybinds: s.appSettings?.keybinds,
      osPlatform: s.osPlatform,
      useWgpu: s.appSettings?.useWgpuRenderer !== false,
    })),
  );
  const several = useLibraryStore((s) => s.multiSelectedPaths.length > 1);
  const supported = osPlatform !== "linux" && useWgpu && !unavailable;
  const label = t("studio.multiView.toggle");
  const shortcut = shortcutHint("toggle_multi_view", keybinds, osPlatform);
  const title = !supported
    ? t("studio.multiView.unsupported")
    : t("studio.workspace.withShortcut", {
        label: several ? label : `${label} — ${t("studio.multiView.selectSeveral")}`,
        shortcut: shortcut ?? t("studio.workspace.noKey"),
      });
  return (
    <>
      <button
        aria-pressed={supported && enabled}
        aria-label={label}
        title={title}
        disabled={!supported}
        onClick={toggle}
      >
        <LayoutGrid size={14} />
      </button>
      {active && (
        <button
          className={editAll ? "studio-edit-all is-on" : "studio-edit-all"}
          aria-pressed={editAll}
          title={t("studio.multiView.editAllTitle")}
          onClick={() => setEditAll(!editAll)}
        >
          <Layers size={14} />
          <span className="optional-label">{t("studio.multiView.editAll")}</span>
        </button>
      )}
      {active && (
        <button
          aria-pressed={zoomLock}
          aria-label={t("studio.multiView.zoomLock")}
          title={t("studio.multiView.zoomLockTitle")}
          onClick={() => setZoomLock(!zoomLock)}
        >
          {zoomLock ? <Lock size={13} /> : <LockOpen size={13} />}
        </button>
      )}
    </>
  );
}
