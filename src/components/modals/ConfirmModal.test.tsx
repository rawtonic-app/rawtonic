import '../../i18n';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import ConfirmModal from './ConfirmModal';

afterEach(cleanup);

const classes = (element: Element | null) => [...(element?.classList ?? [])];

function open(confirmVariant?: string) {
  const onConfirm = vi.fn();
  const onClose = vi.fn();
  render(
    <ConfirmModal
      isOpen
      title="Empty the Session Trash?"
      message="3 files are removed from disk."
      confirmText="Delete Permanently"
      cancelText="Cancel"
      confirmVariant={confirmVariant}
      onConfirm={onConfirm}
      onClose={onClose}
    />,
  );
  return { onConfirm, onClose };
}

describe('ConfirmModal', () => {
  it('uses the studio dialog and buttons', () => {
    open();
    expect(classes(screen.getByRole('dialog').firstElementChild)).toEqual(expect.arrayContaining(['studio-dialog']));
    expect(classes(screen.getByRole('heading', { name: 'Empty the Session Trash?' }))).toEqual(expect.arrayContaining(['studio-dialog-title']));
    expect(classes(screen.getByRole('button', { name: 'Cancel' }))).toEqual(expect.arrayContaining(['studio-button']));
    expect(classes(screen.getByRole('button', { name: 'Delete Permanently' }))).toEqual(expect.arrayContaining(['studio-button', 'is-primary']));
  });

  it('confirms an ordinary confirmation with Enter', () => {
    const { onConfirm, onClose } = open();
    const confirm = screen.getByRole('button', { name: 'Delete Permanently' });
    expect(document.activeElement).toBe(confirm);
    fireEvent.keyDown(confirm, { key: 'Enter' });
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('never confirms a destructive confirmation with a plain Enter: Cancel is the default', () => {
    const { onConfirm, onClose } = open('destructive');
    const cancel = screen.getByRole('button', { name: 'Cancel' });
    const confirm = screen.getByRole('button', { name: 'Delete Permanently' });
    expect(document.activeElement).toBe(cancel);
    expect(classes(confirm)).toEqual(expect.arrayContaining(['studio-button', 'is-destructive']));
    expect(classes(confirm)).not.toContain('is-primary');
    fireEvent.keyDown(cancel, { key: 'Enter' });
    expect(onConfirm).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('confirms a destructive confirmation only from its own button', () => {
    const { onConfirm } = open('destructive');
    const confirm = screen.getByRole('button', { name: 'Delete Permanently' });
    confirm.focus();
    fireEvent.keyDown(confirm, { key: 'Enter' });
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it('closes with Escape without confirming', () => {
    const { onConfirm, onClose } = open('destructive');
    fireEvent.keyDown(screen.getByRole('button', { name: 'Cancel' }), { key: 'Escape' });
    expect(onConfirm).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
