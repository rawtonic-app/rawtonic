// These payloads are also deserialized by Rust's library_contract_tests.
// Pin the commands themselves so a test-only argument type cannot mask drift.
// @ts-expect-error node:fs is untyped in the frontend tsconfig
import { readFileSync } from 'node:fs';
import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import payloads from './libraryFiles.json';
import { libraryChanges, savePresets } from '../store/libraryFiles';
import { changeLibraryAlbums, type AlbumChange } from '../components/studio/libraryAlbums';
import type { AlbumItem } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'linux' }));
vi.mock('../store/useLibraryStore', () => ({ useLibraryStore: { getState: () => ({ setLibrary: vi.fn() }) } }));

const read = (path: string): string => readFileSync(path, 'utf8') as string;
const squash = (s: string) => s.replace(/\s+/g, ' ').replace(/,\s*$/, '').trim();
const signatures = {
  library_change: ['change: AlbumChange, app_handle: AppHandle', 'Vec<AlbumItem>'],
  add_to_album: ['album_id: String, paths: Vec<String>, app_handle: AppHandle', '()'],
  save_presets: ['presets: Vec<PresetItem>, app_handle: AppHandle', '()'],
  library_flush: ['app_handle: AppHandle', 'Vec<String>'],
  handle_import_presets_from_file: ['file_path: String, app_handle: AppHandle', 'Vec<PresetItem>'],
  handle_import_legacy_presets_from_file: ['file_path: String, app_handle: AppHandle', 'Vec<PresetItem>'],
  handle_import_presets_from_files: ['file_paths: Vec<String>, app_handle: AppHandle', 'PresetImportResult']
};
const sources = ['albums', 'presets'].map((name) => read(`src-tauri/src/file_management/${name}.rs`)).join('\n');

it.each(Object.entries(signatures))('%s keeps its argument names and result shape', (name, [args, result]) => {
  const declaration = sources.match(
    new RegExp(`#\\[tauri::command\\]\\s+pub async fn ${name}\\(([\\s\\S]*?)\\) -> Result<([\\s\\S]*?), String> \\{`)
  );
  expect(declaration, name).not.toBeNull();
  expect(squash(declaration![1])).toBe(args);
  expect(squash(declaration![2])).toBe(result);
  // Tauri converts top-level argument names to camelCase by default.
  const keys = args
    .split(', ')
    .filter((arg) => !arg.startsWith('app_handle:'))
    .map((arg) => arg.split(':')[0].replace(/_([a-z])/g, (_, c: string) => c.toUpperCase()));
  expect(Object.keys(payloads[name as keyof typeof payloads]).sort()).toEqual(keys.sort());
});

it('read commands keep their no-argument contracts', () => {
  expect(sources).toMatch(/pub async fn get_albums\(app_handle: AppHandle\) -> Result<Vec<AlbumItem>, String>/);
  expect(sources).toMatch(/pub async fn load_presets\(app_handle: AppHandle\) -> Result<Vec<PresetItem>, String>/);
  expect(sources).not.toContain('pub fn queue_ticket(');
});

beforeEach(() => {
  libraryChanges.reset();
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockResolvedValue([]);
});

it('the frontend sends the shared save and empty flush payloads', async () => {
  await savePresets(payloads.save_presets.presets);
  await libraryChanges.flush();
  expect(vi.mocked(invoke).mock.calls).toEqual([
    ['save_presets', payloads.save_presets],
    ['library_flush', payloads.library_flush]
  ]);
});

it('the Albums tool sends the shared nested albumId payload', async () => {
  const tree = [{ ...payloads.album_tree[0], images: [] }] as AlbumItem[];
  vi.mocked(invoke).mockImplementation(async () => tree);
  await changeLibraryAlbums(payloads.library_change.change as AlbumChange);
  expect(invoke).toHaveBeenCalledWith('library_change', payloads.library_change);
});

it('preset import calls keep their camelCase payload and read the pinned result types', () => {
  const frontend = read('src/hooks/usePresets.ts');
  expect(frontend).toMatch(
    /importPresets<Array<UserPreset>>\(Invokes.HandleImportPresetsFromFile,\s*\{\s*filePath\s*\}\)/
  );
  expect(frontend).toMatch(
    /importPresets<Array<UserPreset>>\(Invokes.HandleImportLegacyPresetsFromFile,\s*\{\s*filePath\s*\}\)/
  );
  expect(frontend).toMatch(
    /importPresets<PresetImportResult>\(Invokes.HandleImportPresetsFromFiles,\s*\{ filePaths \}\)/
  );
});

it.each(payloads.album_changes)('the Albums tool sends the shared $change.kind payload', async (payload) => {
  await changeLibraryAlbums(payload.change as AlbumChange);
  expect(invoke).toHaveBeenCalledExactlyOnceWith('library_change', payload);
});

it('whole-tree album replacement is no longer exposed', () => {
  expect(sources).not.toMatch(/pub async fn save_albums\(/);
  expect(sources).not.toContain('fn save_albums_blocking(');
  expect(read('src-tauri/src/lib.rs')).not.toContain('file_management::save_albums');
  expect(read('src/components/ui/AppProperties.tsx')).not.toContain("SaveAlbums = 'save_albums'");
  expect(read('src/store/libraryFiles.ts')).not.toContain('export const saveAlbums');
});

it('deleted-album and wait-timeout errors remain strings and the frontend can flush after rejection', async () => {
  for (const error of Object.values(payloads.errors)) {
    libraryChanges.reset();
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'library_change') throw error;
      return [];
    });
    await expect(changeLibraryAlbums(payloads.library_change.change as AlbumChange)).rejects.toBe(error);
    expect(await libraryChanges.flush()).toEqual([]);
  }
});
