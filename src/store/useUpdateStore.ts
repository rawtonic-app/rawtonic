// SPDX-License-Identifier: AGPL-3.0-or-later
import { create } from 'zustand';
import i18n from 'i18next';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { relaunch } from '@tauri-apps/plugin-process';
import { Invokes } from '../components/ui/AppProperties';
import { relaunchAfterSettingsSaves } from './useSettingsStore';
import { useEditorStore } from './useEditorStore';
import { useLibraryStore } from './useLibraryStore';
import { confirmWhileRunning, runningWork, type AskToQuitWhileRunning } from '../utils/quit';

/** The photo the restart reopens: the one shown when no library is open (a
 * library reopens by itself; a photo opened on its own had nothing to come
 * back from, and the Windows setup restarts without arguments). */
export function photoToReopen(): string | null {
  if (useLibraryStore.getState().sessionDirectory) return null;
  return useEditorStore.getState().selectedImage?.path ?? null;
}

/** Restarting to update stops running exports and imports, as quitting does. */
const confirmRestartWhileRunning: AskToQuitWhileRunning = (work, restart, cancel) =>
  confirmWhileRunning(
    (running) => i18n.t('updates.restartRunningMessage', { work: running }),
    i18n.t('updates.restartAnyway'),
    work,
    restart,
    cancel
  );

/** Why the notice links to the browser instead of offering "Install update". */
export type UpdateFallback =
  'noManifest' | 'noPackage' | 'allUsers' | 'moveToApplications' | 'portable' | 'package' | 'notInstalled';

export interface UpdateInfo {
  version: string;
  releaseUrl: string;
  appImage: boolean;
  /** The installer for this computer; null when the release has none (the release page is offered). */
  downloadUrl: string | null;
  assetName: string | null;
  assetSize: number | null;
  /** The release notes (Markdown) from the same response. */
  notes: string;
  /** "Install update": the release has a signed update package for this copy. */
  inApp: boolean;
  fallback: UpdateFallback | null;
}

/** Why a download or install failed (`UpdateFailure` in update_check/install.rs). */
export type UpdateFailure =
  | {
      kind:
        | 'network'
        | 'manifestUnavailable'
        | 'signature'
        | 'noPackage'
        | 'disk'
        | 'permission'
        | 'changed'
        | 'notOffered'
        | 'stopped'
        | 'other';
    }
  | { kind: 'notRestored'; path: string }
  /** Edits that could not be saved: the app was not restarted (nothing installed). */
  | { kind: 'unsaved'; items: string[] };

/** The in-app update of the offered version, after the photographer's click. */
export type InstallState =
  | { phase: 'idle' }
  | { phase: 'downloading'; downloaded: number; total: number | null }
  | { phase: 'ready' }
  | { phase: 'installing' }
  | { phase: 'failed'; failure: UpdateFailure; during: 'download' | 'install' };

const IDLE: InstallState = { phase: 'idle' };

const asFailure = (error: unknown): UpdateFailure =>
  error && typeof error === 'object' && 'kind' in error ? (error as UpdateFailure) : { kind: 'other' };

interface UpdateState {
  update: UpdateInfo | null;
  checking: boolean;
  checked: boolean;
  failed: boolean;
  started: boolean;
  /** The in-app update; it belongs to `installVersion` and survives "Later". */
  install: InstallState;
  installVersion: string | null;
  check(manual: boolean): Promise<void>;
  dismiss(): void;
  /** "Install update": downloads and verifies the package; nothing is replaced yet. */
  download(): Promise<void>;
  cancelDownload(): Promise<void>;
  /** "Restart now": asks while exports or imports run, saves every edit as a
   * quit does, then puts the update in place and restarts. */
  restart(askWhileRunning?: AskToQuitWhileRunning): Promise<void>;
}

export const useUpdateStore = create<UpdateState>((set, get) => ({
  update: null,
  checking: false,
  checked: false,
  failed: false,
  started: false,
  install: IDLE,
  installVersion: null,
  check: async (manual) => {
    if (get().checking || (!manual && get().started)) return;
    set({ checking: true, started: true, checked: false, failed: false });
    try {
      const update = await invoke<UpdateInfo | null>(Invokes.CheckUpdates, { manual });
      set({ update, checked: manual && update === null });
      // A newer release than the one being installed starts over.
      if (update && get().installVersion !== update.version && get().install.phase !== 'installing') {
        set({ install: IDLE, installVersion: null });
      }
    } catch (error) {
      console.debug('Update check failed:', error);
      set({ failed: manual });
    } finally {
      set({ checking: false });
    }
  },
  dismiss: () => set({ update: null }),
  download: async () => {
    const update = get().update;
    if (!update?.inApp || get().install.phase === 'downloading' || get().install.phase === 'installing') return;
    const version = update.version;
    set({ install: { phase: 'downloading', downloaded: 0, total: null }, installVersion: version });
    const stop = await listen<{ version: string; downloaded: number; total: number | null }>(
      'update-download-progress',
      ({ payload }) => {
        const state = get();
        if (payload.version !== version || state.installVersion !== version || state.install.phase !== 'downloading')
          return;
        set({ install: { phase: 'downloading', downloaded: payload.downloaded, total: payload.total } });
      }
    ).catch(() => () => undefined);
    try {
      await invoke(Invokes.UpdateDownload, { version });
      if (get().installVersion === version) set({ install: { phase: 'ready' } });
    } catch (error) {
      if (get().installVersion !== version) return;
      const failure = asFailure(error);
      set({ install: failure.kind === 'stopped' ? IDLE : { phase: 'failed', failure, during: 'download' } });
    } finally {
      stop();
    }
  },
  cancelDownload: async () => {
    if (get().install.phase !== 'downloading') return;
    set({ install: IDLE });
    await invoke(Invokes.UpdateCancelDownload).catch(console.error);
  },
  restart: async (askWhileRunning = confirmRestartWhileRunning) => {
    const version = get().installVersion;
    if (!version || get().install.phase !== 'ready') return;
    // Running exports and imports would stop: ask, as quitting does.
    const work = runningWork();
    if (work.length > 0) {
      const go = await new Promise<boolean>((resolve) =>
        askWhileRunning(
          work,
          () => resolve(true),
          () => resolve(false)
        )
      );
      if (!go) return;
    }
    set({ install: { phase: 'installing' } });
    try {
      // Every edit, setting and library change is written first (the
      // restart's own hold keeps editing closed); only then is the update
      // put in place. Windows: the setup ends the app and starts the new one.
      const unsaved = await relaunchAfterSettingsSaves(async () => {
        await invoke(Invokes.UpdateInstall, { version, reopen: photoToReopen() });
        await relaunch();
      });
      if (unsaved.length > 0) {
        set({ install: { phase: 'failed', failure: { kind: 'unsaved', items: unsaved }, during: 'install' } });
      }
    } catch (error) {
      set({ install: { phase: 'failed', failure: asFailure(error), during: 'install' } });
    }
  }
}));
