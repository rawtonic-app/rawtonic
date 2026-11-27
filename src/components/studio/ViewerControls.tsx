import { Ellipsis, SquareSquare } from 'lucide-react';
import { useContextMenu } from '../../context/ContextMenuContext';
import { OPTION_SEPARATOR } from '../ui/AppProperties';
import { cameraPreviewOnScreen, cameraPreviewToggleState } from './CameraPreview';
import { TOGGLE_CAMERA_PREVIEW, toggleCameraPreview } from '../../utils/cameraPreview';
import { useTranslation } from 'react-i18next';
import { useEditorStore } from '../../store/useEditorStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import { shortcutHint } from '../../utils/keyboardUtils';
import { captureInfoParts } from '../../utils/captureInfo';
import { ZOOM_LEVELS, zoomLevelLabel } from '../../utils/viewerDisplay';
import { rememberZoomLevel } from '../../utils/zoomLevel';
import { readoutColor, readoutValues, usePointerSampleStore } from '../../utils/pointerSample';

/** Capture One's proof margin: fit leaves a margin around the photograph (Alt+X). */
export function ProofMarginButton() {
  const { t } = useTranslation();
  const appSettings = useSettingsStore((s) => s.appSettings);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const on = appSettings?.viewerProofMargin ?? false;
  const label = t('studio.workspace.viewer.proofMargin');
  const key = shortcutHint('toggle_proof_margin', appSettings?.keybinds, osPlatform);
  return (
    <button
      className="studio-proof-margin-toggle"
      aria-pressed={on}
      aria-label={label}
      title={t('studio.workspace.withShortcut', {
        label: t('studio.workspace.viewer.proofMarginTitle'),
        shortcut: key ?? t('studio.workspace.noKey')
      })}
      disabled={!appSettings}
      onClick={() => {
        const { appSettings: current, handleSettingsChange } = useSettingsStore.getState();
        if (current) void handleSettingsChange({ ...current, viewerProofMargin: !on });
      }}
    >
      <SquareSquare size={14} />
    </button>
  );
}

export type ZoomHandler = (value: number, fit?: boolean, belowFit?: boolean) => void;

/** A zoom menu level (or the 100 % button): zooms there, below fit too,
 * and is the level Z returns to. */
export function chooseZoomLevel(onZoom: ZoomHandler, level: number): void {
  rememberZoomLevel(level);
  onZoom(level, false, true);
}

/**
 * The viewer bar's zoom menu: Fit and every level (ZOOM_LEVELS), below fit
 * too. It hands the focus back after a choice, so Z, Space and the other
 * plain keys reach the viewer again (a focused select keeps them).
 */
export function ZoomSelect({ onZoom }: { onZoom: ZoomHandler }) {
  const { t } = useTranslation();
  return (
    <select
      className="studio-zoom-select"
      aria-label={t('studio.workspace.viewer.zoom')}
      title={t('studio.workspace.viewer.zoom')}
      value=""
      onChange={(e) => {
        const { value } = e.target;
        e.currentTarget.blur();
        if (value === 'fit') onZoom(1, true);
        else if (value) chooseZoomLevel(onZoom, Number(value));
      }}
    >
      <option value="" disabled>
        {t('studio.workspace.viewer.zoom')}
      </option>
      <option value="fit">{t('studio.workspace.viewer.fit')}</option>
      {ZOOM_LEVELS.map((level) => (
        <option key={level} value={String(level)}>
          {zoomLevelLabel(level)}
        </option>
      ))}
    </select>
  );
}

/** ISO, shutter speed, aperture and focal length under the photograph. */
export function ViewerCaptureInfo() {
  const exif = useEditorStore((s) => s.selectedImage?.exif);
  const parts = captureInfoParts(exif);
  if (parts.length === 0) return null;
  return (
    <span className="studio-capture-info" title={parts.join('  ')}>
      {parts.map((part) => (
        <span key={part}>{part}</span>
      ))}
    </span>
  );
}

/** Capture One's pointer readout: R, G, B of the displayed photograph under
 * the pointer (the colour pickers' 11 × 11 sample, sRGB), when the View
 * menu's Pointer RGB Readout is on. A click switches 0–255 and percent. */
export function PointerReadout() {
  const { t } = useTranslation();
  const on = useSettingsStore((s) => s.appSettings?.pointerReadout ?? false);
  const unit = useSettingsStore((s) => (s.appSettings?.pointerReadoutUnit === 'percent' ? 'percent' : 'rgb255'));
  const patch = usePointerSampleStore((s) => s.sample?.patch ?? null);
  if (!on) return null;
  const color = patch && readoutColor(patch);
  const values = color ? readoutValues(color, unit) : ['–', '–', '–'];
  return (
    <button
      className="studio-pointer-readout"
      data-testid="pointer-readout"
      data-unit={unit}
      aria-label={t('studio.workspace.viewer.pointerReadout')}
      title={t('studio.workspace.viewer.pointerReadoutTitle')}
      onClick={() => {
        const { appSettings: current, handleSettingsChange } = useSettingsStore.getState();
        if (current)
          void handleSettingsChange({ ...current, pointerReadoutUnit: unit === 'percent' ? 'rgb255' : 'percent' });
      }}
    >
      {(['R', 'G', 'B'] as const).map((channel, index) => (
        <span key={channel}>
          <span className="studio-muted">{channel}</span>
          <span className="studio-pointer-readout-value">{values[index]}</span>
        </span>
      ))}
      {unit === 'percent' && <span className="studio-muted">%</span>}
    </button>
  );
}

/**
 * The viewer bar's ⋯ (always shown since VISCALM1): Camera preview, Proof
 * margin, Fit, 100 % and the zoom levels. The bar itself leaves out the zoom
 * list and the two toggles while they are off, and Fit and 100 % once it runs
 * short (useFitOneRow's level 4, UX4-07), so none of them becomes unreachable.
 */
export function ViewerOverflowMenu({ onZoom }: { onZoom: ZoomHandler }) {
  const { t } = useTranslation();
  const { showContextMenu } = useContextMenu();
  const keybinds = useSettingsStore((s) => s.appSettings?.keybinds);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const label = t('studio.workspace.viewer.more');
  return (
    <button
      className="studio-viewer-overflow"
      aria-label={label}
      title={label}
      aria-haspopup="menu"
      onClick={(event) => {
        const rect = event.currentTarget.getBoundingClientRect();
        const editor = useEditorStore.getState();
        const { appSettings, handleSettingsChange } = useSettingsStore.getState();
        const camera = cameraPreviewToggleState(editor.selectedImage);
        const margin = appSettings?.viewerProofMargin ?? false;
        showContextMenu(rect.left, rect.bottom, [
          {
            label: t('studio.workspace.cameraPreview.label'),
            checked: cameraPreviewOnScreen(editor),
            disabled: camera.disabled,
            shortcut: shortcutHint(TOGGLE_CAMERA_PREVIEW, keybinds, osPlatform),
            onClick: toggleCameraPreview
          },
          {
            label: t('studio.workspace.viewer.proofMargin'),
            checked: margin,
            disabled: !appSettings,
            shortcut: shortcutHint('toggle_proof_margin', keybinds, osPlatform),
            onClick: () => {
              const current = useSettingsStore.getState().appSettings;
              if (current) void handleSettingsChange({ ...current, viewerProofMargin: !margin });
            }
          },
          { type: OPTION_SEPARATOR },
          // Fit and 100 % leave the bar one level later (UX4-07).
          { label: t('studio.workspace.viewer.fitTitle'), shortcut: ',', onClick: () => onZoom(1, true) },
          {
            label: t('studio.workspace.viewer.actualPixels'),
            shortcut: '.',
            onClick: () => chooseZoomLevel(onZoom, 1)
          },
          {
            label: t('studio.workspace.viewer.zoom'),
            submenu: [
              { label: t('studio.workspace.viewer.fit'), onClick: () => onZoom(1, true) },
              ...ZOOM_LEVELS.map((level) => ({
                label: zoomLevelLabel(level),
                onClick: () => chooseZoomLevel(onZoom, level)
              }))
            ]
          }
        ]);
      }}
    >
      <Ellipsis size={14} />
    </button>
  );
}
