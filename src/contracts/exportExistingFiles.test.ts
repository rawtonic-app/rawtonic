// The "If a file exists" choice as the Export form sends it to
// `export_images` and saves it in a recipe. The same payloads are
// deserialized by Rust (`export_existing_files_contract` in
// src-tauri/src/app_settings.rs), so both sides agree on the names.
import { expect, it } from 'vitest';
import payloads from './exportExistingFiles.json';
import { DEFAULT_RECIPE_DRAFT, EXISTING_FILES, exportSettingsFromDraft, presetFromDraft } from '../utils/exportRecipes';

it('the form sends each choice as the Rust side reads it', () => {
  expect(EXISTING_FILES).toEqual(['keepBoth', 'skip', 'replaceIfChanged']);
  EXISTING_FILES.forEach((existingFiles, i) => {
    expect(exportSettingsFromDraft({ ...DEFAULT_RECIPE_DRAFT, existingFiles }).settings).toEqual(payloads.settings[i]);
  });
});

it('a recipe saves the choice as the Rust side reads it', () => {
  const preset = presetFromDraft({ ...DEFAULT_RECIPE_DRAFT, existingFiles: 'replaceIfChanged' }, 'recipe-1', 'Web');
  expect(JSON.parse(JSON.stringify(preset))).toEqual(payloads.preset);
});
