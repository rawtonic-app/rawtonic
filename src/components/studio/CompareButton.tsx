import { ChevronDown, Columns2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useContextMenu } from '../../context/ContextMenuContext';
import { useSettingsStore } from '../../store/useSettingsStore';
import { shortcutHint } from '../../utils/keyboardUtils';
import { useEditorStore } from '../../store/useEditorStore';
import { useEditorActions } from '../../hooks/useEditorActions';
import { COMPARE_MODES } from '../../utils/beforeAfter';

/** Toggle the remembered comparison; the adjacent chevron also works from the keyboard. */
export default function CompareButton({ disabled, title }: { disabled: boolean; title: string }) {
  const { t } = useTranslation();
  const cycleKey = useSettingsStore((s) => shortcutHint('cycle_compare_mode', s.appSettings?.keybinds, s.osPlatform));
  const enabled = useEditorStore((s) => s.splitView);
  const orientation = useEditorStore((s) => s.splitOrientation);
  const { toggleSplitView, setCompareMode } = useEditorActions();
  const { showContextMenu } = useContextMenu();
  const open = (x: number, y: number) => {
    showContextMenu(x, y, COMPARE_MODES.map((mode) => ({
      label: t(`studio.workspace.viewer.compareMode.${mode}`),
      checked: mode === (enabled ? orientation : 'off'),
      onClick: () => setCompareMode(mode),
    })));
  };
  return (
    <>
      <button
        aria-pressed={enabled}
        title={title}
        disabled={disabled}
        onClick={toggleSplitView}
        onContextMenu={(event) => { event.preventDefault(); if (!disabled) open(event.clientX, event.clientY); }}
      >
        <Columns2 size={14} />
        <span className="optional-label">{t('studio.workspace.viewer.beforeAfter')}</span>
      </button>
      <button
        aria-label={t('studio.workspace.viewer.compareModes')}
        title={cycleKey ? t('studio.workspace.withShortcut', { label: t('studio.workspace.viewer.compareModes'), shortcut: cycleKey }) : t('studio.workspace.viewer.compareModes')}
        aria-haspopup="menu"
        disabled={disabled}
        onClick={(event) => { const rect = event.currentTarget.getBoundingClientRect(); open(rect.left, rect.bottom); }}
      >
        <ChevronDown size={12} />
      </button>
    </>
  );
}
