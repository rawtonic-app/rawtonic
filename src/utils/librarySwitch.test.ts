import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { debouncedSave, flushRecipeSaves } from '../hooks/useEditorActions';
import { switchLibraryAfterSaves, type UnsavedChoice } from './quit';
import { INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn() } }));

// The backend as the save command sees it: recipes per library, written
// into the library open now.
let active: string;
let recipes: Record<string, Record<string, Adjustments>>;
let failing: boolean;
const PHOTO = '/photos/same.dng';
const edit = (exposure: number) => ({ ...INITIAL_ADJUSTMENTS, exposure });

beforeEach(() => {
  active = 'A';
  failing = false;
  recipes = { A: {}, B: { [PHOTO]: edit(-1) } };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === Invokes.SaveMetadataAndUpdateThumbnail) {
      if (failing) throw new Error('disk full');
      const { path, adjustments } = args as { path: string; adjustments: Adjustments };
      recipes[active][path] = adjustments;
      return undefined;
    }
    if (command === Invokes.SessionOpen) {
      active = (args as { directory: string }).directory;
      return undefined;
    }
    throw new Error(`unexpected ${command}`);
  });
});
afterEach(() => {
  debouncedSave.cancel();
  vi.clearAllMocks();
});

const open = (directory: string) => () => invoke(Invokes.SessionOpen, { directory }).then(() => directory);
/** An edit in A whose save failed: kept as unsaved, as the editor does. */
async function failedEditInA() {
  failing = true;
  debouncedSave(PHOTO, edit(2));
  await debouncedSave.flush();
}

it('refuses a library switch while an edit cannot be saved, until the user decides', async () => {
  await failedEditInA();
  const ask = vi.fn(async (): Promise<UnsavedChoice> => 'cancel');
  expect(await switchLibraryAfterSaves(open('B'), ask)).toBeNull();
  expect(ask).toHaveBeenCalledWith(['Edits of same.dng']);
  expect(active).toBe('A');

  // Retry: saved into A, then B opens.
  ask.mockImplementationOnce(async () => {
    failing = false;
    return 'retry';
  });
  expect(await switchLibraryAfterSaves(open('B'), ask)).toBe('B');
  expect(recipes.A[PHOTO]).toEqual(edit(2));
  expect(recipes.B[PHOTO]).toEqual(edit(-1));
});

it('discarding the unsaved edits switches, and they never reach the new library', async () => {
  await failedEditInA();
  const ask = vi.fn(async (): Promise<UnsavedChoice> => 'discard');
  expect(await switchLibraryAfterSaves(open('B'), ask)).toBe('B');
  expect(active).toBe('B');
  failing = false;
  // Flushes after the switch (an export, a file operation, quitting) do
  // not retry A's edit into B.
  expect(await flushRecipeSaves()).toEqual([]);
  expect(recipes.B[PHOTO]).toEqual(edit(-1));
  expect(recipes.A).toEqual({});
});

it('drops a debounced save made during the switch instead of writing it to the new library', async () => {
  const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
  try {
    const switched = switchLibraryAfterSaves(async () => {
      // The photograph of A is still open while B opens.
      debouncedSave(PHOTO, edit(3));
      await open('B')();
      await debouncedSave.flush();
      return 'B';
    }, vi.fn());
    expect(await switched).toBe('B');
    await debouncedSave.flush();
    expect(await flushRecipeSaves()).toEqual([]);
    expect(recipes.B[PHOTO]).toEqual(edit(-1));
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('made before the library was switched'));
    // B's own edits are saved again.
    debouncedSave(PHOTO, edit(4));
    await debouncedSave.flush();
    expect(recipes.B[PHOTO]).toEqual(edit(4));
  } finally {
    warn.mockRestore();
  }
});
