// SPDX-License-Identifier: AGPL-3.0-or-later
import { useTranslation } from 'react-i18next';
import { useEditorStore } from '../../store/useEditorStore';

/**
 * Whether the photo's imported Lightroom edit was made in HDR mode
 * (`crs:HDREditMode` = 1, kept by the import as `lightroomHdr`,
 * `rawtonic_interop::lightroom_hdr`). Rawtonic renders and exports SDR only;
 * the HDR settings stay in the recipe and the XMP sidecar.
 */
export function isLightroomHdrEdit(adjustments: { [index: string]: unknown } | null | undefined): boolean {
  const kept = adjustments?.lightroomHdr;
  if (!kept || typeof kept !== 'object') return false;
  const mode = (kept as Record<string, unknown>).HDREditMode;
  return Number(String(mode ?? '').trim()) === 1;
}

export default function LightroomHdrNotice() {
  const { t } = useTranslation();
  const hdr = useEditorStore((s) => isLightroomHdrEdit(s.adjustments));
  if (!hdr) return null;
  return (
    <div className="studio-raw-preview-notice" role="status">
      {t('studio.workspace.lightroomHdr')}
    </div>
  );
}
