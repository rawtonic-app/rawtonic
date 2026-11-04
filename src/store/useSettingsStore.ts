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

/**
 * Saves a settings snapshot, after every earlier save has finished (at once
 * when none is under way). The backend saves off the main thread, so two
 * snapshots sent together could otherwise be written in the other order and
 * the older one win.
 */
export function saveSettings(settings: AppSettings): Promise<void> {
  const send = () => invoke<void>(Invokes.SaveSettings, { settings });
  const save = inFlight ? inFlight.then(send) : Promise.resolve(send());
  const settled = save.then(
    () => undefined,
    () => undefined
  );
  inFlight = settled;
  void settled.then(() => {
    if (inFlight === settled) inFlight = null;
  });
  return save;
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
