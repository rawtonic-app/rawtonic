// SPDX-License-Identifier: AGPL-3.0-or-later
// The processing backend in use and what the GPU start guard left out after
// a crash (src-tauri gpu_start_guard.rs, task WSF): its notice at start and
// its line in Preferences → Processing; the first start's shader compile as
// a running task, and the way out when no graphics backend starts (G76).
import { invoke } from '@tauri-apps/api/core';
import { relaunch } from '@tauri-apps/plugin-process';
import i18n from 'i18next';
import type { TFunction } from 'i18next';
import { Invokes } from '../ui/AppProperties';
import { notify } from '../../activity/notify';
import { useActivityStore } from '../../store/useActivityStore';
import { relaunchAfterSettingsSaves, useSettingsStore } from '../../store/useSettingsStore';
import { useUIStore } from '../../store/useUIStore';

/** What a start tells about the guard (`gpu_start_guard::Notice`). */
export type GpuStartNotice =
  | { kind: 'fellBack'; crashed: string; stage: string; avoid: string; kept: boolean }
  | { kind: 'openGlPreference' }
  | { kind: 'openGlUnavailable' };

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
  if (notice.kind === 'openGlUnavailable') return t('gpuBackend.openGlUnavailable');
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

/** How long the first start's compile may take before it is shown. */
export const PREPARING_SHOWN_AFTER_MS = 1500;
const PREPARING_ID = 'gpu-preparing';

/**
 * The start's first shader compile as a running task in the activity centre
 * ("Preparing graphics for this computer"), shown once it takes longer than
 * `PREPARING_SHOWN_AFTER_MS`, then logged under Done with its time. A warm
 * start finishes before it is shown. `ready` resolves when the compile is
 * done (`gpu_first_display_ready`): true when it compiled, false when the
 * GPU did not start (that failure is told on its own, `gpu-start-failed`).
 */
export async function watchGraphicsPreparation(
  ready: () => Promise<boolean> = () => invoke<boolean>(Invokes.GpuFirstDisplayReady),
  shownAfterMs = PREPARING_SHOWN_AFTER_MS
): Promise<void> {
  const started = Date.now();
  let shown = false;
  const timer = setTimeout(() => {
    shown = true;
    useActivityStore.getState().start({
      id: PREPARING_ID,
      source: 'app',
      title: i18n.t('gpuBackend.preparing'),
      detail: i18n.t('gpuBackend.preparingDetail')
    });
  }, shownAfterMs);
  const compiled = await ready().catch(() => false);
  clearTimeout(timer);
  if (!shown) return;
  if (!compiled) {
    useActivityStore.getState().finish(PREPARING_ID);
    return;
  }
  const seconds = Math.max(1, Math.round((Date.now() - started) / 1000));
  useActivityStore.getState().finish(PREPARING_ID, { title: i18n.t('gpuBackend.prepared', { seconds }) });
}

/**
 * "Reset graphics settings and retry": nothing left out after a crash, the
 * processing backend back to Auto, then a relaunch once every setting is
 * written. A failure stays in the activity centre.
 */
export async function resetGraphicsAndRetry(restart: () => Promise<void> = relaunch): Promise<void> {
  try {
    await gpuBackendUseAutomatic();
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (appSettings && (appSettings.processingBackend ?? 'auto') !== 'auto') {
      await handleSettingsChange({ ...appSettings, processingBackend: 'auto' });
    }
    const unsaved = await relaunchAfterSettingsSaves(restart);
    if (unsaved.length > 0) throw new Error(unsaved.join(', '));
  } catch (error) {
    notify.error(i18n.t('gpuBackend.resetFailed', { details: error instanceof Error ? error.message : String(error) }), {
      toastId: 'gpu-reset-failed'
    });
  }
}
