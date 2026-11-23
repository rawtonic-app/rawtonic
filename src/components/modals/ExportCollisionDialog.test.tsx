import '../../i18n';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import ExportCollisionDialog from './ExportCollisionDialog';
import { useExportCollisionStore, type CollisionAnswer } from '../../store/useExportCollisionStore';
import type { CollisionSummary } from '../ui/ExportImportProperties';

afterEach(() => {
  cleanup();
  useExportCollisionStore.setState({ pending: null });
});

const summary: CollisionSummary = {
  total: 11,
  unchangedOwn: { count: 2, names: ['a.jpg', 'b.jpg'] },
  changedOwn: { count: 1, names: ['c.jpg'] },
  other: { count: 8, names: ['d.jpg', 'e.jpg', 'f.jpg', 'g.jpg', 'h.jpg', 'i.jpg', 'j.jpg'] },
  originals: 1
};

function ask() {
  if (!screen.queryByTestId('host'))
    render(
      <div data-testid="host">
        <ExportCollisionDialog />
      </div>
    );
  let answer: CollisionAnswer | null | undefined;
  act(() => {
    void useExportCollisionStore
      .getState()
      .ask(summary)
      .then((a) => (answer = a));
  });
  return () => answer;
}

describe('ExportCollisionDialog', () => {
  it('shows nothing until a question is asked', () => {
    render(<ExportCollisionDialog />);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('counts each kind, lists a few names and says how many more', () => {
    ask();
    const kinds = screen.getByTestId('collision-kinds');
    expect(kinds.textContent).toContain('Earlier export, nothing changed since: 2');
    expect(kinds.textContent).toContain('a.jpg, b.jpg');
    expect(kinds.textContent).toContain('Earlier export, photo or settings changed since: 1');
    expect(kinds.textContent).toContain('Other files, or exports with mask files (not replaced): 8');
    expect(kinds.textContent).toContain('d.jpg, e.jpg, f.jpg, g.jpg, h.jpg and 3 more');
    expect(kinds.textContent).not.toContain('i.jpg');
    expect(screen.getByRole('dialog').textContent).toContain('Photos in a library among them, never replaced: 1');
  });

  it('leaves out a kind that has no file', () => {
    render(<ExportCollisionDialog />);
    act(() => {
      void useExportCollisionStore.getState().ask({ ...summary, unchangedOwn: { count: 0, names: [] } });
    });
    expect(screen.getByTestId('collision-kinds').textContent).not.toContain('nothing changed since');
  });

  it.each([
    ['Keep both', 'keepBoth'],
    ['Skip', 'skip'],
    ['Replace if changed', 'replaceIfChanged']
  ])('answers %s once for the run', async (label, expected) => {
    const answer = ask();
    fireEvent.click(screen.getByRole('button', { name: label }));
    await act(async () => {});
    expect(answer()).toBe(expected);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('Keep both, which overwrites nothing, has the focus', () => {
    ask();
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Keep both' }));
  });

  it('Cancel and Escape answer nothing', async () => {
    let answer = ask();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await act(async () => {});
    expect(answer()).toBeNull();
    answer = ask();
    // Anywhere on the page, as the Export dialog behind it would hear it.
    fireEvent.keyDown(document.body, { key: 'Escape' });
    await act(async () => {});
    expect(answer()).toBeNull();
  });

  it('Escape does not reach the dialog behind it', () => {
    const behind = vi.fn();
    window.addEventListener('keydown', behind);
    ask();
    fireEvent.keyDown(document.body, { key: 'Escape' });
    window.removeEventListener('keydown', behind);
    expect(behind).not.toHaveBeenCalled();
  });

  it('Tab stays among the buttons of the question', () => {
    ask();
    const buttons = screen.getAllByRole('button');
    const last = buttons[buttons.length - 1];
    last.focus();
    fireEvent.keyDown(last, { key: 'Tab' });
    expect(document.activeElement).toBe(buttons[0]);
    fireEvent.keyDown(buttons[0], { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(last);
  });
});
