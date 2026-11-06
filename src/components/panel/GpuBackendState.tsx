// SPDX-License-Identifier: AGPL-3.0-or-later
// Preferences → Processing: the backend in use and, after a crash at start,
// what is left out and the way back (src-tauri gpu_start_guard.rs).
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import Button from '../ui/Button';
import Text from '../ui/Text';
import { TextColors } from '../../types/typography';
import { GpuBackendStatus, backendNames, gpuBackendStatus, gpuBackendUseAutomatic } from './gpuBackend';

interface GpuBackendStateProps {
  /** After "Use automatic again": the preference becomes Auto (a relaunch applies it). */
  onUseAutomatic(): void;
}

export default function GpuBackendState({ onUseAutomatic }: GpuBackendStateProps) {
  const { t } = useTranslation();
  const [status, setStatus] = useState<GpuBackendStatus | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let active = true;
    gpuBackendStatus()
      .then((s) => {
        if (active) setStatus(s);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, []);

  if (!status) return null;
  const leftOut = status.step === 'fellBack' || status.step === 'kept';
  const useAutomatic = async () => {
    try {
      await gpuBackendUseAutomatic();
      setFailed(false);
      setStatus({ ...status, step: 'clean', avoided: '', notice: null });
      onUseAutomatic();
    } catch {
      setFailed(true);
    }
  };

  return (
    <div className="space-y-2" data-testid="gpu-backend-state">
      <Text as="p" color={TextColors.secondary}>
        {status.inUse
          ? t('gpuBackend.inUse', { backend: backendNames(status.inUse, t), adapter: status.adapter ?? '' })
          : t('gpuBackend.inUseNone')}
      </Text>
      {leftOut && (
        <>
          <Text as="p" color={TextColors.accent} role="status">
            {t(status.step === 'kept' ? 'gpuBackend.leftOutKept' : 'gpuBackend.leftOut', {
              crashed: backendNames(status.crashed, t) || t('gpuBackend.unknownBackend'),
              avoided: backendNames(status.avoided, t) || t('gpuBackend.unknownBackend'),
              date: status.crashedOn ?? '?'
            })}
          </Text>
          <div className="flex justify-end">
            <Button onClick={useAutomatic}>{t('gpuBackend.useAutomatic')}</Button>
          </div>
          {failed && (
            <Text as="p" color={TextColors.error} role="alert">
              {t('gpuBackend.useAutomaticFailed')}
            </Text>
          )}
        </>
      )}
    </div>
  );
}
