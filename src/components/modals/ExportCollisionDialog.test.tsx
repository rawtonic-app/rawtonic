import '../../i18n';
import { afterEach, describe, expect, it } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import ExportCollisionDialog from './ExportCollisionDialog';
import { useExportCollisionStore, type CollisionAnswer } from '../../store/useExportCollisionStore';
import type { CollisionSummary } from '../ui/ExportImportProperties';

afterEach(() => {
  cleanup();
  useExportCollisionStore.setState({ pending: null });
});

const summary: CollisionSummary = {
  total: 9,
  unchangedOwn: { count: 2, names: ['a.jpg', 'b.jpg'] },
  changedOwn: { count: 1, names: ['c.jpg'] },
  other: { count: 6, names: ['d.jpg', 'e.jpg', 'f.jpg', 'g.jpg', 'h.jpg'] },
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
    expect(kinds.textContent).toContain('Earlier export, photo or settings changed since: 1');
    expect(kinds.textContent).toContain('Other files (not an export of this photo): 6');
    expect(kinds.textContent).toContain('d.jpg, e.jpg, f.jpg, g.jpg, h.jpg and 1 more');
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
    fireEvent.keyDown(screen.getByRole('dialog').firstElementChild!, { key: 'Escape' });
    await act(async () => {});
    expect(answer()).toBeNull();
  });
});
