// SPDX-License-Identifier: AGPL-3.0-or-later
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { toast } from 'react-toastify';
import AppNotices from './AppNotices';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
const UPDATE = 'update';
const SKIP = 'Skip';
afterEach(() => {
  toast.dismiss();
  cleanup();
});

it('toasts and persistent notices share one column; toasts are polite and close only by their button', async () => {
  render(
    <AppNotices>
      <aside role="status">{UPDATE}</aside>
    </AppNotices>,
  );
  const onClose = vi.fn();
  await act(async () => {
    toast.error(<button type="button">{SKIP}</button>, { onClose, autoClose: false });
  });
  const column = document.querySelector('.app-notices')!;
  const notice = column.querySelector('.app-notice--error')!;
  expect(notice.getAttribute('role')).toBe('status');
  expect(screen.getByRole('region', { name: 'notifications.region' })).toBeTruthy();
  // The toasts come before (above) the persistent notice in the column.
  expect(column.lastElementChild?.textContent).toBe('update');
  fireEvent.click(screen.getByText('Skip'));
  fireEvent.click(notice);
  expect(onClose).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'notifications.close' }));
  expect(onClose).toHaveBeenCalled();
});
