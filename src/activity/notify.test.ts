import { createElement } from 'react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { toast } from 'react-toastify';
import { CONFIRMATION_MS, notify, plainText } from './notify';
import { resetActivityStore, useActivityStore } from '../store/useActivityStore';
import '../i18n';

vi.mock('react-toastify', () => ({ toast: { success: vi.fn(), info: vi.fn(), error: vi.fn(), warn: vi.fn(), dismiss: vi.fn() } }));

const entries = () => useActivityStore.getState().entries;

beforeEach(() => resetActivityStore());
afterEach(() => vi.clearAllMocks());

it('puts a failure under Needs attention and shows no toast for it', () => {
  notify.error('Failed to move 2 files: disk full');
  notify.warn('3 XMP files were changed by another program');
  expect(entries().map((e) => [e.status, e.title])).toEqual([
    ['failure', 'Failed to move 2 files: disk full'],
    ['warning', '3 XMP files were changed by another program'],
  ]);
  expect(toast.error).not.toHaveBeenCalled();
  expect(toast.warn).not.toHaveBeenCalled();
});

it('confirms the user’s own action with a short toast, also logged under Done', () => {
  notify.success('Renamed 3 photos');
  expect(toast.success).toHaveBeenCalledWith('Renamed 3 photos', expect.objectContaining({ autoClose: CONFIRMATION_MS }));
  expect(entries()).toEqual([expect.objectContaining({ status: 'done', title: 'Renamed 3 photos' })]);
});

it('keeps one entry for the same failure reported again', () => {
  notify.error('Failed to save changes: disk full');
  notify.error('Failed to save changes: disk full');
  expect(entries()).toHaveLength(1);
});

it('settles an entry by its id: dismiss, isActive and update', () => {
  notify.error('Undo failed', { toastId: 'undo-1' });
  expect(notify.isActive('undo-1')).toBe(true);
  notify.update('undo-1', { render: 'Undo failed again' });
  expect(entries()[0].title).toBe('Undo failed again');
  notify.dismiss('undo-1');
  expect(notify.isActive('undo-1')).toBe(false);
});

it('carries the actions and photographs of a failure', () => {
  const run = vi.fn();
  notify.error('2 photos not saved', { source: 'write', paths: ['/a.dng'], actions: [{ label: 'Try again', run }] });
  expect(entries()[0]).toMatchObject({ source: 'write', paths: ['/a.dng'] });
  entries()[0].actions![0].run();
  expect(run).toHaveBeenCalledOnce();
});

it('reads a rich notice’s words without its buttons', () => {
  const content = createElement('span', null, 'Copies kept on Backup ', createElement('button', null, 'Show in Finder'));
  expect(plainText(content).trim()).toBe('Copies kept on Backup');
  notify.warn(content);
  expect(entries()[0]).toMatchObject({ title: 'Copies kept on Backup', body: content });
});

it('never titles a notice "[object Object]": a component without words gets a generic line', () => {
  const Custom = () => null;
  notify.error(createElement(Custom));
  expect(entries()[0].title).toBe('Something needs your attention');
  notify.error(createElement(Custom), { title: 'Export failed', toastId: 'x' });
  expect(entries().find((e) => e.id === 'x')?.title).toBe('Export failed');
});
