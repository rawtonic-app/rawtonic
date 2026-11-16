import { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Ellipsis } from 'lucide-react';
import { useContextMenu } from '../../context/ContextMenuContext';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useUIStore } from '../../store/useUIStore';
import { historyMenuLabels, menuCommandEnabled } from '../../hooks/useAppMenu';
import { hasOverflowMenu, overflowCommands } from '../../utils/commandPalette';
import { type Translate } from '../../utils/appMenu';

/** Whether a dialog holds the app (its menu waits, as a menu bar does). */
const dialogOpen = () =>
  useUIStore.getState().isSettingsOpen || !!document.querySelector('dialog[open], [aria-modal="true"]');

/**
 * Tracks the keys that open the menu bar on Windows and in KDE/GNOME
 * applications (UXR4L-04): F10, and Alt pressed and released alone (any
 * other key, click or wheel in between makes it a modifier, e.g. Alt+click
 * to solo a layer). Calls `toggle` on either.
 */
export function menuBarKeys(toggle: () => void): () => void {
  let armed = false;
  const disarm = () => {
    armed = false;
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === 'Alt' && !event.repeat && !event.ctrlKey && !event.metaKey && !event.shiftKey) {
      armed = true;
      return;
    }
    armed = false;
    if (event.key === 'F10' && !event.ctrlKey && !event.metaKey && !event.shiftKey && !event.altKey) {
      event.preventDefault();
      event.stopImmediatePropagation();
      toggle();
    }
  };
  const onKeyUp = (event: KeyboardEvent) => {
    if (event.key !== 'Alt') return;
    const wasArmed = armed;
    armed = false;
    if (!wasArmed) return;
    // Chromium (WebView2) would otherwise hand Alt to the window's own menu.
    event.preventDefault();
    toggle();
  };
  window.addEventListener('keydown', onKeyDown, true);
  window.addEventListener('keyup', onKeyUp, true);
  window.addEventListener('pointerdown', disarm, true);
  window.addEventListener('wheel', disarm, true);
  window.addEventListener('blur', disarm);
  return () => {
    window.removeEventListener('keydown', onKeyDown, true);
    window.removeEventListener('keyup', onKeyUp, true);
    window.removeEventListener('pointerdown', disarm, true);
    window.removeEventListener('wheel', disarm, true);
    window.removeEventListener('blur', disarm);
  };
}

export default function AppOverflowMenu() {
  const { t } = useTranslation();
  const { showContextMenu, hideContextMenu, menuState } = useContextMenu();
  const platform = useSettingsStore((state) => state.osPlatform);
  const keybinds = useSettingsStore((state) => state.appSettings?.keybinds);
  const button = useRef<HTMLButtonElement>(null);
  const open = (focusFirst: boolean) => {
    const rect = button.current?.getBoundingClientRect();
    if (!rect) return;
    showContextMenu(
      rect.left,
      rect.bottom,
      overflowCommands(
        platform,
        t as unknown as Translate,
        keybinds,
        menuCommandEnabled,
        historyMenuLabels(t as unknown as Translate)
      ),
      { focusFirst }
    );
  };
  const latest = useRef({ open, hideContextMenu, visible: !!menuState?.isVisible });
  latest.current = { open, hideContextMenu, visible: !!menuState?.isVisible };
  const shown = hasOverflowMenu(platform);
  useEffect(() => {
    if (!shown) return;
    return menuBarKeys(() => {
      const { open, hideContextMenu, visible } = latest.current;
      if (visible) hideContextMenu();
      else if (!dialogOpen()) open(true);
    });
  }, [shown]);
  // Without a native menu bar (Linux, Windows) ⋯ holds the whole menu tree,
  // Help › Report a Problem… included; macOS has it all in the menu bar.
  if (!shown) return null;
  return (
    <button
      ref={button}
      className="studio-bar-icon"
      title={t('studio.workspace.moreWithKey')}
      aria-label={t('studio.workspace.more')}
      aria-haspopup="menu"
      aria-keyshortcuts="F10"
      onClick={() => open(false)}
    >
      <Ellipsis size={15} />
    </button>
  );
}
