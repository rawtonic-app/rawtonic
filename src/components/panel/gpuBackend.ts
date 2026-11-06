// SPDX-License-Identifier: AGPL-3.0-or-later
// The processing backend in use and what the GPU start guard left out after
// a crash (src-tauri gpu_start_guard.rs, task WSF): its notice at start and
// its line in Preferences → Processing.
import { invoke } from '@tauri-apps/api/core';
import i18n from 'i18next';
import type { TFunction } from 'i18next';
import { Invokes } from '../ui/AppProperties';
import { notify } from '../../activity/notify';
import { useUIStore } from '../../store/useUIStore';

/** What a start tells about the guard (`gpu_start_guard::Notice`). */
export type GpuStartNotice =
  { kind: 'fellBack'; crashed: string; stage: string; avoid: string; kept: boolean } | { kind: 'openGlPreference' };

/** `gpu_processing::GpuBackendStatus`. */
export interface GpuBackendStatus {
  inUse: string | null;
  adapter: string | null;
  step: 'clean' | 'fellBack' | 'retrying' | 'kept';
  avoided: string;
  crashed: string;
  crashedOn: string | null;
  notice: GpuStartNotice | null;
}

export const gpuBackendStatus = () => invoke<GpuBackendStatus>(Invokes.GpuBackendStatus);
export const gpuBackendUseAutomatic = () => invoke<void>(Invokes.GpuBackendUseAutomatic);

const KNOWN = ['vulkan', 'dx12', 'metal', 'gl'] as const;

/** "vulkan,dx12" → "Vulkan / DirectX 12" (wgpu's names as the backend list shows them). */
export function backendNames(list: string | null | undefined, t: TFunction): string {
  return (list ?? '')
    .split(',')
    .map((name) => name.trim())
    .filter(Boolean)
    .map((name) =>
      (KNOWN as readonly string[]).includes(name)
        ? t(`settings.processing.backends.${name as (typeof KNOWN)[number]}`)
        : name
    )
    .join(' / ');
}

/** The notice's text, or null when this start has nothing to tell. */
export function gpuNoticeText(status: GpuBackendStatus, t: TFunction): string | null {
  const notice = status.notice;
  if (!notice) return null;
  if (notice.kind === 'openGlPreference') return t('gpuBackend.openGl');
  const used = status.inUse ? backendNames(status.inUse, t) : t('gpuBackend.none');
  const crashed = backendNames(notice.crashed, t);
  if (!crashed) return t('gpuBackend.fellBackUnknown', { used });
  return notice.kept ? t('gpuBackend.kept', { crashed, used }) : t('gpuBackend.fellBack', { crashed, used });
}

let noticeShown = false;

/** At start: the guard's notice, once per run, with a way to Preferences. */
export function showGpuBackendNotice(status: GpuBackendStatus): void {
  const text = gpuNoticeText(status, i18n.t);
  if (!text || noticeShown) return;
  noticeShown = true;
  notify.warning(text, {
    toastId: 'gpu-backend-fallback',
    actions: [
      {
        label: i18n.t('gpuBackend.openPreferences'),
        run: () => useUIStore.getState().setUI({ isSettingsOpen: true, settingsCategory: 'processing' })
      }
    ]
  });
}
