import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { platform } from '@tauri-apps/plugin-os';
import { AppSettings, SupportedTypes, Invokes } from '../components/ui/AppProperties';

interface SettingsState {
  appSettings: AppSettings | null;
  supportedTypes: SupportedTypes | null;
  osPlatform: string;

  // Actions
  initPlatform: () => void;
  setAppSettings: (settings: AppSettings | null) => void;
  setSupportedTypes: (types: SupportedTypes | null) => void;
  handleSettingsChange: (newSettings: AppSettings) => Promise<void>;
}

let inFlight: Promise<void> | null = null;
/** The snapshot of the last save when that save failed; null once a later save succeeded. */
let unsaved: AppSettings | null = null;

/** What a settings save that could not be written is called in the quit prompt. */
export const SETTINGS_WRITE_NAME = 'Preferences';
/** How long quitting waits for the settings saves before it reports them unsaved. */
export const SETTINGS_FLUSH_TIMEOUT_MS = 10_000;

/**
 * Saves a settings snapshot, after every earlier save has finished (at once
 * when none is under way). The backend saves off the main thread, so two
 * snapshots sent together could otherwise be written in the other order and
 * the older one win.
 */
export function saveSettings(settings: AppSettings): Promise<void> {
  const send = () => invoke<void>(Invokes.SaveSettings, { settings });
  const save = inFlight ? inFlight.then(send) : Promise.resolve(send());
  // Saves run one after another, so the last to settle decides what is on disk.
  const settled = save.then(
    () => {
      unsaved = null;
    },
    () => {
      unsaved = settings;
    }
  );
  inFlight = settled;
  void settled.then(() => {
    if (inFlight === settled) inFlight = null;
  });
  return save;
}

async function drainSettingsSaves(): Promise<string[]> {
  while (inFlight) await inFlight;
  // The last save failed: write its snapshot once more.
  if (unsaved) await saveSettings(unsaved).catch(() => undefined);
  while (inFlight) await inFlight;
  return unsaved ? [SETTINGS_WRITE_NAME] : [];
}

/**
 * Waits for the queued settings saves before quitting or relaunching
 * (a `QuitFlush`): resolves with `[SETTINGS_WRITE_NAME]` when the newest
 * settings could not be written, also when the saves did not finish within
 * `timeoutMs`, else with nothing.
 */
export function flushSettingsSaves(timeoutMs = SETTINGS_FLUSH_TIMEOUT_MS): Promise<string[]> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const expired = new Promise<string[]>((resolve) => {
    timer = setTimeout(() => resolve([SETTINGS_WRITE_NAME]), timeoutMs);
  });
  return Promise.race([drainSettingsSaves(), expired]).finally(() => clearTimeout(timer));
}

export const useSettingsStore = create<SettingsState>((set) => ({
  appSettings: null,
  supportedTypes: null,
  osPlatform: '',

  initPlatform: () => {
    try {
      set({ osPlatform: platform() });
    } catch (_err) {
      set({ osPlatform: '' });
    }
  },

  setAppSettings: (settings) => set({ appSettings: settings }),

  setSupportedTypes: (types) => set({ supportedTypes: types }),

  handleSettingsChange: async (newSettings: AppSettings) => {
    if (!newSettings) {
      console.error('handleSettingsChange was called with null settings. Aborting save operation.');
      return;
    }

    // `searchCriteria` is not an AppSettings field but may be present at runtime; it is never saved.
    const { searchCriteria: _searchCriteria, ...settingsToSave } = newSettings as AppSettings & {
      searchCriteria?: unknown;
    };
    set({ appSettings: newSettings });

    try {
      await saveSettings(settingsToSave);
    } catch (err) {
      console.error('Failed to save settings:', err);
    }
  }
}));
