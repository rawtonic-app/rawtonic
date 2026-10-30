import '../i18n';
import { act, cleanup, fireEvent, render, renderHook, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import TaggingSubMenu from './TaggingSubMenu';
import { useLibraryActions } from '../hooks/useLibraryActions';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import type { AppSettings, ImageFile } from '../components/ui/AppProperties';

/** The library's own keyword update, which the store follows. */
const handleTagsSaved = (savedPaths: string[], newTags: { tag: string; isUser: boolean }[]) => {
  useLibraryStore.getState().setLibrary((state) => ({
    imageList: state.imageList.map((image) =>
      savedPaths.includes(image.path)
        ? { ...image, tags: newTags.map((t) => (t.isUser ? `user:${t.tag}` : t.tag)) }
        : image,
    ),
  }));
};

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
  onTagsChanged = vi.fn(),
  paths = ['/s/a.NEF', '/s/b.NEF'],
  settings: AppSettings | null = null,
  // The photos a group adds to the menu's paths (a variant clicked inside its group).
  initialTags: { tag: string; isUser: boolean }[] = [],
  // What the library does with a partly saved write; `null`: nothing, it is
  // spied on.
  onTagsSaved: ((savedPaths: string[], newTags: { tag: string; isUser: boolean }[]) => void) | null = null,
) => {
  useSettingsStore.setState({ appSettings: settings });
  render(
    <TaggingSubMenu
      paths={paths}
      initialTags={initialTags}
      onTagsChanged={onTagsChanged}
      onTagsSaved={onTagsSaved ?? handleTagsSaved}
      appSettings={settings}
      hideContextMenu={() => {}}
    />,
  );
  return onTagsChanged;
};

const add = async (tag: string) => {
  const input = screen.getByRole('textbox');
  fireEvent.change(input, { target: { value: tag } });
  await act(async () => {
    fireEvent.keyDown(input, { key: 'Enter' });
  });
};

const tagsOf = () => Object.fromEntries(useLibraryStore.getState().imageList.map((image) => [image.path, image.tags]));

it("keeps the menu as it was when a keyword is not saved everywhere, so adding it again retries", async () => {
  const onTagsChanged = menu();
  // b.NEF fails: the library names it in SaveFailure.failed.
  invoke.mockRejectedValueOnce({ message: '1 of 2 photos not saved', failed: ['/s/b.NEF'] });
  await add('client');
  expect(onTagsChanged).not.toHaveBeenCalled();
  expect(screen.queryByText('client')).toBeNull();
  // Retrying is not rejected as a duplicate: the batch is sent again.
  invoke.mockResolvedValueOnce(undefined);
  await add('client');
  expect(invoke).toHaveBeenCalledTimes(2);
});

it('shows the keyword on the photographs that were saved when the batch partly failed', async () => {
  useLibraryStore.setState({ imageList: [file('/s/a.NEF', ['dog']), file('/s/b.NEF', ['dog'])] });
  const onTagsSaved = vi.fn();
  const onTagsChanged = menu(vi.fn(), ['/s/a.NEF', '/s/b.NEF'], null, [{ tag: 'dog', isUser: false }], onTagsSaved);
  // b.NEF fails: the library names it in SaveFailure.failed.
  invoke.mockRejectedValueOnce({ message: '1 of 2 photos not saved', failed: ['/s/b.NEF'] });
  await add('client');
  // Exactly the photograph that saved is reported, with the menu's tags and
  // the keyword: not b.NEF (a group-expanding update would mark it too).
  expect(onTagsSaved.mock.calls[0][0]).toEqual(['/s/a.NEF']);
  // The group-expanding callback is not asked, and the menu keeps its own
  // state, so the same attempt can be made again.
  expect(onTagsChanged).not.toHaveBeenCalled();
  expect(screen.queryByText('client')).toBeNull();
  invoke.mockResolvedValueOnce(undefined);
  await add('client');
  expect(invoke.mock.calls).toEqual([
    ['add_tag_for_paths', { paths: ['/s/a.NEF', '/s/b.NEF'], tag: 'user:client' }],
    ['add_tag_for_paths', { paths: ['/s/a.NEF', '/s/b.NEF'], tag: 'user:client' }],
  ]);
  // Both saved: nothing of the batch failed, so the whole selection goes, with
  // every tag of the menu.
  expect(onTagsChanged.mock.calls[0][0]).toEqual(['/s/a.NEF', '/s/b.NEF']);
  expect(onTagsChanged.mock.calls[0][1].map((tag: { tag: string }) => tag.tag)).toEqual(['client', 'dog']);
  expect(onTagsSaved).toHaveBeenCalledTimes(1);
});

it('marks exactly the reported photographs in the library, without a group', () => {
  const { result } = renderHook(() => useLibraryActions());
  useSettingsStore.setState({ appSettings: { grouping: 'raw' } as AppSettings });
  useLibraryStore.setState({
    imageList: [file('/s/a.NEF', ['dog'], 'g1'), file('/s/a.JPG', ['dog'], 'g1', false)],
  });
  result.current.handleTagsSaved(['/s/a.JPG'], [{ tag: 'client', isUser: true }]);
  // Replaced, not added to what the store shows for that photograph.
  expect(tagsOf()).toEqual({ '/s/a.NEF': ['dog'], '/s/a.JPG': ['user:client'] });
  result.current.handleTagsChanged(['/s/a.JPG'], [{ tag: 'dog', isUser: true }]);
  expect(useLibraryStore.getState().imageList[1].tags).toEqual(['user:dog']);
});

it('shows a removed keyword gone from the photographs that were saved when the batch partly failed', async () => {
  useLibraryStore.setState({
    imageList: [file('/s/a.NEF', ['dog', 'user:client']), file('/s/b.NEF', ['dog', 'user:client'])],
  });
  useSettingsStore.setState({ appSettings: null });
  const onTagsSaved = vi.fn();
  const onTagsChanged = vi.fn();
  render(
    <TaggingSubMenu
      paths={['/s/a.NEF', '/s/b.NEF']}
      initialTags={[
        { tag: 'client', isUser: true },
        { tag: 'dog', isUser: false },
      ]}
      onTagsChanged={onTagsChanged}
      onTagsSaved={onTagsSaved}
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
  expect(onTagsSaved).toHaveBeenCalledWith(['/s/a.NEF'], [{ tag: 'dog', isUser: false }]);
  expect(onTagsChanged).not.toHaveBeenCalled();
  expect(screen.queryByText('client')).not.toBeNull();
  invoke.mockResolvedValueOnce(undefined);
  await act(async () => {
    fireEvent.click(screen.getByText('client'));
  });
  expect(invoke.mock.calls).toEqual([
    ['remove_tag_for_paths', { paths: ['/s/a.NEF', '/s/b.NEF'], tag: 'user:client' }],
    ['remove_tag_for_paths', { paths: ['/s/a.NEF', '/s/b.NEF'], tag: 'user:client' }],
  ]);
  expect(onTagsChanged).toHaveBeenCalledWith(['/s/a.NEF', '/s/b.NEF'], [{ tag: 'dog', isUser: false }]);
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
  const onTagsChanged = menu(vi.fn(), ['/s/a.JPG'], settings, [{ tag: 'dog', isUser: false }]);
  // The group adds its NEF, and only that one fails.
  invoke.mockRejectedValueOnce({ message: '1 of 2 photos not saved', failed: ['/s/a.NEF'] });
  await add('client');
  expect(invoke).toHaveBeenCalledWith('add_tag_for_paths', { paths: ['/s/a.JPG', '/s/a.NEF'], tag: 'user:client' });
  // The saved JPEG has it; its failed NEF does not (it keeps its own, none).
  expect(tagsOf()).toEqual({
    '/s/a.NEF': null,
    '/s/a.JPG': ['user:client'],
    '/s/c.NEF': null,
    '/s/c.JPG': null,
  });
  expect(onTagsChanged).not.toHaveBeenCalled();
});
