// SPDX-License-Identifier: AGPL-3.0-or-later
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { toast } from 'react-toastify';
import { useRef } from 'react';
import AppNotices from './AppNotices';
import { useModalFocus } from '../../hooks/useModalFocus';

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

function Modal() {
  const ref = useRef<HTMLDivElement>(null);
  useModalFocus(ref, true);
  return <div ref={ref} aria-modal="true" role="dialog"><button type="button">{OK}</button></div>;
}
const OK = 'ok';

it('a modal (Preferences) leaves the notices usable: a manual check shows its result there', () => {
  render(
    // As in App: the notices beside the workspace that holds Preferences.
    <div className="studio-app">
      <main>
        <Modal />
      </main>
      <AppNotices>
        <aside role="status"><button type="button">{UPDATE}</button></aside>
      </AppNotices>
    </div>,
  );
  expect(screen.getByRole('button', { name: UPDATE }).closest('[inert]')).toBeNull();
});

it('countdown bars stay hidden; a toast with real progress shows its bar', async () => {
  render(<AppNotices />);
  await act(async () => {
    toast.info(UPDATE, { autoClose: 3000 });
    toast.info(SKIP, { autoClose: false, hideProgressBar: false, progress: 0.4 });
  });
  const bars = screen.getAllByRole('progressbar', { hidden: true });
  expect(bars.map((bar) => bar.getAttribute('aria-hidden'))).toEqual(['true', 'false']);
});
