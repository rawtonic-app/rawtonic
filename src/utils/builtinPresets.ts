import { invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import type { TFunction } from 'i18next';
import { Invokes, type Preset } from '../components/ui/AppProperties';

/** Rawtonic's own presets and styles (`resources/presets/builtin.json`),
 * read-only and apart from the user's presets: applying one copies its
 * values, so photos never depend on a built-in's later versions. */
export const useBuiltinPresets = create<{ presets: Preset[] }>(() => ({ presets: [] }));

let loading: Promise<void> | null = null;

/** Loads the built-ins once per run; a failure leaves the list empty. */
export function loadBuiltinPresets(): Promise<void> {
  loading ??= invoke<Omit<Preset, 'builtin'>[]>(Invokes.LoadBuiltinPresets).then(
    (presets) =>
      useBuiltinPresets.setState({
        presets: Array.isArray(presets) ? presets.map((preset) => ({ ...preset, builtin: true })) : [],
      }),
    (error) => {
      console.error('Failed to load built-in presets:', error);
    },
  );
  return loading;
}

/** For tests: forget the loaded built-ins. */
export function resetBuiltinPresets() {
  loading = null;
  useBuiltinPresets.setState({ presets: [] });
}

/** A preset's shown name: a built-in's translation, else its own. */
export const presetName = (t: TFunction, preset: Preset) =>
  preset.builtin ? t(`studio.presets.builtin.${preset.id}`, { defaultValue: preset.name }) : preset.name;
