// A Lightroom or Capture One import compares each photograph's saved recipe
// with the last import's (C1RI1): it must not run while any edit is still
// unsaved, including the failed writes of another writer (Edit All
// Selected), which `flushRecipeSaves` retries and names.
import { expect, it, vi } from 'vitest';
import { closeOpenPhotoForLibraryWrite, registerRecipeFlush } from './useEditorActions';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => null) }));

it('counts edits another writer could not save before a library import (C1RI1)', async () => {
  const flush = vi.fn(async () => ['/b.nef?vc=a1b2c3']);
  const unregister = registerRecipeFlush(flush);
  try {
    expect(await closeOpenPhotoForLibraryWrite()).toBe(1);
    expect(flush).toHaveBeenCalled();
  } finally {
    unregister();
  }
  expect(await closeOpenPhotoForLibraryWrite()).toBe(0);
});
