import '../i18n';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import TaggingSubMenu from './TaggingSubMenu';

beforeEach(() => invoke.mockReset());
afterEach(cleanup);

const menu = (onTagsChanged = vi.fn()) => {
  render(
    <TaggingSubMenu
      paths={['/s/a.NEF', '/s/b.NEF']}
      initialTags={[]}
      onTagsChanged={onTagsChanged}
      appSettings={null}
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
  expect(onTagsChanged).toHaveBeenCalledWith(['/s/a.NEF', '/s/b.NEF'], [{ tag: 'client', isUser: true }]);
});
