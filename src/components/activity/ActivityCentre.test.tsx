import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import ActivityCentre from './ActivityCentre';
import { resetActivityStore, useActivityStore } from '../../store/useActivityStore';
import { runAppCommand } from '../../utils/appCommands';
import { SHOW_ACTIVITY_PHOTOS_EVENT } from '../../activity/showPhotos';
import '../../i18n';

const store = () => useActivityStore.getState();
const slot = () => document.querySelector<HTMLButtonElement>('.activity-slot-button');

beforeEach(() => resetActivityStore());
afterEach(() => {
  cleanup();
  resetActivityStore();
});

it('is empty when idle', () => {
  render(<ActivityCentre />);
  expect(slot()).toBeNull();
  expect(screen.queryByRole('dialog')).toBeNull();
});

it('shows running work with a ring, the count and the leading task', () => {
  render(<ActivityCentre />);
  act(() => {
    store().start({ id: 'a', source: 'export', title: 'Exporting 3 photos as JPG', progress: { done: 1, total: 3 } });
    store().start({ id: 'b', source: 'ai', title: 'AI mask: Sky' });
  });
  expect(slot()!.textContent).toContain('2');
  expect(slot()!.textContent).toContain('Exporting 3 photos as JPG');
  expect(slot()!.querySelector('.activity-ring')).toBeTruthy();
  expect(slot()!.getAttribute('aria-label')).toBe('Activity: 2 tasks running');
});

it('keeps a dot for a failure until it is dismissed, red before amber', () => {
  render(<ActivityCentre />);
  act(() => void store().report('warning', { id: 'w', source: 'xmp', title: '3 XMP not updated' }));
  expect(slot()!.querySelector('.activity-dot.is-warning')).toBeTruthy();
  act(() => void store().report('failure', { id: 'f', source: 'export', title: '1 photo did not export' }));
  expect(slot()!.querySelector('.activity-dot.is-failure')).toBeTruthy();
  expect(slot()!.getAttribute('aria-label')).toBe('Activity: 2 items need attention');
  act(() => {
    store().dismiss('f');
    store().dismiss('w');
  });
  expect(slot()).toBeNull();
});

it('announces failures assertively and new tasks politely', () => {
  const { container } = render(<ActivityCentre />);
  act(() => void store().start({ id: 'a', source: 'import', title: 'Copying 2 of 10' }));
  act(() => store().update('a', { title: 'Copying 3 of 10' }));
  act(() => void store().report('failure', { id: 'f', source: 'write', title: 'Changes to 1 photo were not saved' }));
  const polite = container.querySelector('[aria-live="polite"]')!;
  const assertive = container.querySelector('[aria-live="assertive"]')!;
  // Progress does not repeat itself; the task's start is said once.
  expect(polite.textContent).toBe('Copying 2 of 10');
  expect(assertive.textContent).toBe('Changes to 1 photo were not saved');
});

it('opens with its shortcut command, lists the three sections and closes with Esc, focus back on the slot', async () => {
  const user = userEvent.setup();
  render(<ActivityCentre />);
  const cancel = vi.fn();
  const tryAgain = vi.fn();
  act(() => {
    store().report('failure', {
      id: 'f',
      source: 'export',
      title: '1 photo did not export',
      paths: ['/s/A.NEF'],
      actions: [{ label: 'Try again', run: tryAgain, settles: true }],
    });
    store().start({ id: 'r', source: 'export', title: 'Exporting', progress: { done: 1, total: 2 }, cancel });
    store().report('done', { id: 'd', source: 'app', title: 'Renamed 3 photos' });
  });
  act(() => void runAppCommand('show_activity'));
  const dialog = screen.getByRole('dialog', { name: 'Activity' });
  const sections = within(dialog).getAllByRole('heading', { level: 3 }).map((h) => h.textContent);
  expect(sections).toEqual(['Needs attention', 'Running', 'Done']);
  expect(within(dialog).getByRole('progressbar', { name: '50 % done' })).toBeTruthy();

  // Every action is a focusable button.
  await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
  expect(cancel).toHaveBeenCalledOnce();
  const shown = vi.fn();
  window.addEventListener(SHOW_ACTIVITY_PHOTOS_EVENT, shown);
  await user.click(within(dialog).getByRole('button', { name: 'Show photos' }));
  expect((shown.mock.calls[0][0] as CustomEvent).detail).toEqual({ name: '1 photo did not export', paths: ['/s/A.NEF'] });
  window.removeEventListener(SHOW_ACTIVITY_PHOTOS_EVENT, shown);
  await user.click(within(dialog).getByRole('button', { name: 'Try again' }));
  expect(tryAgain).toHaveBeenCalledOnce();
  // Try again settles the failure.
  expect(store().entries.some((e) => e.id === 'f')).toBe(false);

  fireEvent.keyDown(window, { key: 'Escape' });
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(document.activeElement).toBe(slot());
});

it('copies a failure’s details', async () => {
  const writeText = vi.fn(async () => {});
  Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
  render(<ActivityCentre />);
  act(() => void store().report('failure', { id: 'f', source: 'write', title: 'Changes to 1 photo were not saved', detail: 'disk full', paths: ['/a.dng'] }));
  // fireEvent: user-event puts its own clipboard in place.
  fireEvent.click(slot()!);
  fireEvent.click(screen.getByRole('button', { name: 'Copy details' }));
  await act(async () => {});
  expect(writeText).toHaveBeenCalledWith('Changes to 1 photo were not saved\ndisk full\n\n/a.dng');
});
