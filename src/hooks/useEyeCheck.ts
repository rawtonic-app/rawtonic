// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import i18n from 'i18next';
import { notify } from '../activity/notify';
import { Invokes } from '../components/ui/AppProperties';
import { useEyesStore } from '../store/useEyesStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { EYES_CHANGED } from './useCatalogSearch';

/** The face models are not installed (`ai_faces::FACE_MODELS_MISSING`). */
const MODELS_MISSING = 'face-models-missing';
/** How long list changes settle before the summaries are read again. */
const SUMMARY_DELAY_MS = 300;

/** The photographer asked to check eyes: turns the background check on for
 * every library and starts it here, downloading the face models if they
 * are missing (an explicit request, PRIVACY.md). */
export async function checkEyes(directory: string) {
  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
  if (appSettings && !appSettings.eyeCheck) await handleSettingsChange({ ...appSettings, eyeCheck: true });
  try {
    await invoke(Invokes.StartEyeCheck, { directory, download: true });
  } catch (error) {
    notify.error(i18n.t('studio.eyes.startFailed', { error: String(error) }));
  }
}

/**
 * Runs the eye check of the open library while it is turned on: starts the
 * background pass (never downloading by itself; without the face models it
 * offers the download), and keeps the Library's per-photograph summaries
 * to the listed photographs and to every reading or call.
 */
export function useEyeCheck() {
  const directory = useLibraryStore((s) => s.sessionDirectory);
  const imageList = useLibraryStore((s) => s.imageList);
  const enabled = useSettingsStore((s) => !!s.appSettings?.eyeCheck);
  const generation = useEyesStore((s) => s.generation);

  useEffect(() => {
    if (!directory || !enabled) return;
    invoke(Invokes.StartEyeCheck, { directory, download: false }).catch((error) => {
      if (String(error) === MODELS_MISSING)
        notify.warn(i18n.t('studio.eyes.modelsMissing'), {
          actions: [{ label: i18n.t('studio.eyes.download'), run: () => checkEyes(directory), settles: true }],
        });
      else notify.warn(i18n.t('studio.eyes.startFailed', { error: String(error) }));
    });
  }, [directory, enabled]);

  useEffect(() => {
    if (!directory) return;
    let gone = false;
    let stop: (() => void) | undefined;
    listen(EYES_CHANGED, () => useEyesStore.getState().changed())
      .then((unlisten) => (gone ? unlisten() : (stop = unlisten)))
      .catch(() => {});
    return () => {
      gone = true;
      stop?.();
    };
  }, [directory]);

  useEffect(() => {
    const store = useEyesStore.getState();
    if (!directory || !enabled) {
      store.reset();
      return;
    }
    const timer = setTimeout(() => {
      store.load(directory, imageList.map((image) => image.path)).catch(() => {});
    }, SUMMARY_DELAY_MS);
    return () => clearTimeout(timer);
  }, [directory, enabled, imageList, generation]);
}
