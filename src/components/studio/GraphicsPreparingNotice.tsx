// SPDX-License-Identifier: AGPL-3.0-or-later
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import { useActivityStore } from '../../store/useActivityStore';
import { COMPILING_ID, PREPARING_ID } from '../panel/gpuBackend';

/**
 * "Preparing graphics for this computer" over the viewer while a shader
 * compile the photo waits for runs (the activity centre's task of the
 * same name, `createCompileActivity`/`watchGraphicsPreparation`): the
 * preview, Auto and the sliders' effect wait for it, on some drivers for
 * minutes (WFQ/WPFC, Radeon 680M: 72–282 s), and the top bar's activity
 * slot alone is easy to miss.
 */
export default function GraphicsPreparingNotice() {
  const { t } = useTranslation();
  const preparing = useActivityStore((s) =>
    s.entries.some((e) => (e.id === COMPILING_ID || e.id === PREPARING_ID) && e.status === 'running')
  );
  if (!preparing) return null;
  return (
    <div className="studio-graphics-preparing" role="status" title={t('gpuBackend.preparingDetail')}>
      <Loader2 size={13} className="animate-spin" aria-hidden />
      {t('gpuBackend.preparing')}
    </div>
  );
}
