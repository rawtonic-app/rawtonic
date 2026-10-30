import '../i18n';
import { act, cleanup, fireEvent, render, renderHook, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import TaggingSubMenu from './TaggingSubMenu';
import { useLibraryActions, withTagDelta } from '../hooks/useLibraryActions';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import type { AppSettings, ImageFile } from '../components/ui/AppProperties';

beforeEach(() => {
  invoke.mockReset();
  useSettingsStore.setState({ appSettings: null });
});
afterEach(cleanup);

const file = (path: string, tags: string[] | null, group_id: string | null = null, is_raw = true): ImageFile =>
  ({
    path,
    tags,
    group_id,
    is_raw,
    rating: 0,
    modified: 0,
    is_edited: false,
    exif: null,
    is_virtual_copy: false,
    is_cloud_placeholder: false,
  }) as ImageFile;

const menu = (
  paths = ['/s/a.NEF', '/s/b.NEF'],
  settings: AppSettings | null = null,
  // The photos a group adds to the menu's paths (a variant clicked inside its group).
  initialTags: { tag: string; isUser: boolean }[] = [],
  // What the library does with a partly saved write; `null`: nothing, it is
  // spied on by the caller.
  onTagSaved: ((savedPaths: string[], tag: string, change: 'add' | 'remove') => void) | null = null,
) => {
  useSettingsStore.setState({ appSettings: settings });
  render(
    <TaggingSubMenu
      paths={paths}
      initialTags={initialTags}
      onTagSaved={onTagSaved ?? applyDelta}
      appSettings={settings}
      hideContextMenu={() => {}}
    />,
  );
};

/** The library's own update after a partly saved write: the hook's
 * `applyTagDelta`, taken from a probe render (the callbacks work off the
 * stores, so where they come from does not matter). */
const applyDelta = (() => {
  let delta: ReturnType<typeof useLibraryActions>['applyTagDelta'] | null = null;
  const Probe = () => {
    delta = useLibraryActions().applyTagDelta;
    return null;
  };
  render(<Probe />);
  cleanup();
  return delta as NonNullable<typeof delta>;
})();

const add = async (tag: string) => {
  const input = screen.getByRole('textbox');
  fireEvent.change(input, { target: { value: tag } });
  await act(async () => {
    fireEvent.keyDown(input, { key: 'Enter' });
  });
};

const tagsOf = () => Object.fromEntries(useLibraryStore.getState().imageList.map((image) => [image.path, image.tags]));

it("keeps the menu as it was when a keyword is not saved everywhere, so adding it again retries", async () => {
  menu();
  // b.NEF fails: the library names it in SaveFailure.failed.
  invoke.mockRejectedValueOnce({ message: '1 of 2 photos not saved', failed: ['/s/b.NEF'] });
  await add('client');
  expect(screen.queryByText('client')).toBeNull();
  // Retrying is not rejected as a duplicate: the batch is sent again.
  invoke.mockResolvedValueOnce(undefined);
  await add('client');
  expect(invoke).toHaveBeenCalledTimes(2);
});

it('shows the keyword on the photographs that were saved when the batch partly failed', async () => {
  useLibraryStore.setState({ imageList: [file('/s/a.NEF', ['dog']), file('/s/b.NEF', ['dog'])] });
  const onTagSaved = vi.fn();
  menu(['/s/a.NEF', '/s/b.NEF'], null, [{ tag: 'dog', isUser: false }], onTagSaved);
  // b.NEF fails: the library names it in SaveFailure.failed.
  invoke.mockRejectedValueOnce({ message: '1 of 2 photos not saved', failed: ['/s/b.NEF'] });
  await add('client');
  // Exactly the photograph that saved is told the one keyword to add: not
  // b.NEF (a group-expanding update would mark it too).
  expect(onTagSaved.mock.calls[0]).toEqual([['/s/a.NEF'], 'user:client', 'add']);
  // The group-expanding callback is not asked, and the menu keeps its own
  // state, so the same attempt can be made again.
  expect(screen.queryByText('client')).toBeNull();
  invoke.mockResolvedValueOnce(undefined);
  await add('client');
  expect(invoke.mock.calls).toEqual([
    ['add_tag_for_paths', { paths: ['/s/a.NEF', '/s/b.NEF'], tag: 'user:client' }],
    ['add_tag_for_paths', { paths: ['/s/a.NEF', '/s/b.NEF'], tag: 'user:client' }],
  ]);
  // Both saved: every photo (and its variants) is told the one keyword.
  expect(onTagSaved.mock.calls[1]).toEqual([['/s/a.NEF', '/s/b.NEF'], 'user:client', 'add']);
  expect(onTagSaved).toHaveBeenCalledTimes(2);
});

it('keeps the automatic keyword of a saved photograph when the batch partly failed', async () => {
  // a.NEF has the automatic "dog" and the typed "user:beach"; b.NEF likewise.
  useLibraryStore.setState({
    imageList: [file('/s/a.NEF', ['dog', 'user:beach']), file('/s/b.NEF', ['dog', 'user:beach'])],
  });
  // The menu's common tags are "beach" and "dog"; a third one is added.
  menu(['/s/a.NEF', '/s/b.NEF'], null, [
    { tag: 'beach', isUser: true },
    { tag: 'dog', isUser: false },
  ]);
  invoke.mockRejectedValueOnce({ message: '1 of 2 photos not saved', failed: ['/s/b.NEF'] });
  await add('client');
  // The saved photo gets the one keyword and keeps both it had: the menu's
  // list (beach, dog) does not replace them, so "dog" cannot vanish.
  expect(tagsOf()).toEqual({ '/s/a.NEF': ['dog', 'user:beach', 'user:client'], '/s/b.NEF': ['dog', 'user:beach'] });
});

it('shows a removed keyword gone from the photographs that were saved when the batch partly failed', async () => {
  useLibraryStore.setState({
    imageList: [file('/s/a.NEF', ['dog', 'user:client']), file('/s/b.NEF', ['dog', 'user:client'])],
  });
  useSettingsStore.setState({ appSettings: null });
  const onTagSaved = vi.fn();
  render(
    <TaggingSubMenu
      paths={['/s/a.NEF', '/s/b.NEF']}
      initialTags={[
        { tag: 'client', isUser: true },
        { tag: 'dog', isUser: false },
      ]}
      onTagSaved={onTagSaved}
      appSettings={null}
      hideContextMenu={() => {}}
    />,
  );
  invoke.mockRejectedValueOnce({ message: '1 of 2 photos not saved', failed: ['/s/b.NEF'] });
  expect(invoke.mock.calls.length).toBe(0);
  expect(screen.queryByText('client')).not.toBeNull();
  await act(async () => {
    fireEvent.click(screen.getByText('client'));
  });
  // Exactly the photo that saved loses it; the menu keeps it, so clicking it
  // again retries.
  expect(onTagSaved).toHaveBeenCalledWith(['/s/a.NEF'], 'user:client', 'remove');
  expect(screen.queryByText('client')).not.toBeNull();
  invoke.mockResolvedValueOnce(undefined);
  await act(async () => {
    fireEvent.click(screen.getByText('client'));
  });
  expect(invoke.mock.calls).toEqual([
    ['remove_tag_for_paths', { paths: ['/s/a.NEF', '/s/b.NEF'], tag: 'user:client' }],
    ['remove_tag_for_paths', { paths: ['/s/a.NEF', '/s/b.NEF'], tag: 'user:client' }],
  ]);
  expect(onTagSaved).toHaveBeenLastCalledWith(['/s/a.NEF', '/s/b.NEF'], 'user:client', 'remove');
});

it('keeps the other keywords of a saved photograph when a keyword is removed and the batch partly failed', async () => {
  // a.NEF: the keyword, another typed one the menu does not show, an
  // automatic one and a label.
  useLibraryStore.setState({
    imageList: [
      file('/s/a.NEF', ['color:red', 'dog', 'user:archive', 'user:client']),
      file('/s/b.NEF', ['color:red', 'dog', 'user:archive', 'user:client']),
    ],
  });
  menu(['/s/a.NEF', '/s/b.NEF'], null, [
    { tag: 'client', isUser: true },
    { tag: 'dog', isUser: false },
  ]);
  invoke.mockRejectedValueOnce({ message: '1 of 2 photos not saved', failed: ['/s/b.NEF'] });
  await act(async () => {
    fireEvent.click(screen.getByText('client'));
  });
  // Only the removed keyword goes from the saved photo: not "user:archive",
  // the automatic "dog" or the label, which the menu does not list.
  expect(tagsOf()).toEqual({
    '/s/a.NEF': ['color:red', 'dog', 'user:archive'],
    '/s/b.NEF': ['color:red', 'dog', 'user:archive', 'user:client'],
  });
});

it('changes the one saved tag on exactly the reported photographs, without a group', () => {
  const { result } = renderHook(() => useLibraryActions());
  useSettingsStore.setState({ appSettings: { grouping: 'raw' } as AppSettings });
  useLibraryStore.setState({
    imageList: [
      file('/s/a.NEF', ['color:red', 'dog', 'user:beach'], 'g1'),
      file('/s/a.JPG', ['dog', 'user:beach'], 'g1', false),
    ],
  });
  // The library saved "user:client" for the JPEG, not for the RAW of its group.
  result.current.applyTagDelta(['/s/a.JPG'], 'user:client', 'add');
  expect(tagsOf()).toEqual({
    '/s/a.NEF': ['color:red', 'dog', 'user:beach'],
    '/s/a.JPG': ['dog', 'user:beach', 'user:client'],
  });
  // And took "user:beach" off it: every other tag stays.
  result.current.applyTagDelta(['/s/a.JPG'], 'user:beach', 'remove');
  expect(tagsOf()).toEqual({
    '/s/a.NEF': ['color:red', 'dog', 'user:beach'],
    '/s/a.JPG': ['dog', 'user:client'],
  });
  // The menu's promise, when the whole selection saved, still replaces the
  // keywords as the write saved the menu's list (the tagged menu's own rule).
  result.current.handleTagsChanged(['/s/a.JPG'], [{ tag: 'dog', isUser: true }]);
  expect(useLibraryStore.getState().imageList[1].tags).toEqual(['user:dog']);
});

it('matches tags by the exact string the library adds, so "Dog" and "dog" are two tags', () => {
  // A stored automatic "Dog" is not the typed "user:dog": adding the second
  // adds that exact string, beside the first, and the library shows both.
  expect(withTagDelta(['Dog', 'user:beach'], 'user:dog', 'add')).toEqual(['Dog', 'user:beach', 'user:dog']);
  // Adding the same exact string twice changes nothing; removing is exact too.
  expect(withTagDelta(['Dog', 'user:dog'], 'user:dog', 'add')).toEqual(['Dog', 'user:dog']);
  expect(withTagDelta(['Dog', 'user:dog'], 'user:dog', 'remove')).toEqual(['Dog']);
  expect(withTagDelta(['user:dog'], 'user:Dog', 'remove')).toEqual(['user:dog']);
  expect(withTagDelta(null, 'user:dog', 'remove')).toBeNull();
});

it('with grouping on, a saved photograph does not mark its failed variant', async () => {
  const settings = { grouping: 'raw' } as AppSettings;
  useLibraryStore.setState({
    imageList: [
      file('/s/a.NEF', null, 'g1'),
      file('/s/a.JPG', ['dog'], 'g1', false),
      file('/s/c.NEF', null, 'g2'),
      file('/s/c.JPG', null, 'g2', false),
    ],
  });
  menu(['/s/a.JPG'], settings, [{ tag: 'dog', isUser: false }]);
  // The group adds its NEF, and only that one fails.
  invoke.mockRejectedValueOnce({ message: '1 of 2 photos not saved', failed: ['/s/a.NEF'] });
  await add('client');
  expect(invoke).toHaveBeenCalledWith('add_tag_for_paths', { paths: ['/s/a.JPG', '/s/a.NEF'], tag: 'user:client' });
  // The saved JPEG has it and keeps its own "dog"; its failed NEF does not
  // (it keeps its own, none).
  expect(tagsOf()).toEqual({
    '/s/a.NEF': null,
    '/s/a.JPG': ['dog', 'user:client'],
    '/s/c.NEF': null,
    '/s/c.JPG': null,
  });
});

it('keeps each photo\'s other keywords when a keyword is added to all of them', async () => {
  // Different keywords: the menu's common list is empty.
  useLibraryStore.setState({ imageList: [file('/s/a.NEF', ['user:beach']), file('/s/b.NEF', ['user:mountain'])] });
  menu(['/s/a.NEF', '/s/b.NEF'], null, []);
  invoke.mockResolvedValueOnce(undefined);
  await add('client');
  expect(tagsOf()).toEqual({
    '/s/a.NEF': ['user:beach', 'user:client'],
    '/s/b.NEF': ['user:client', 'user:mountain'],
  });
});
