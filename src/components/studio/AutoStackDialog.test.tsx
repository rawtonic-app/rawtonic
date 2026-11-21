import '../../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import i18n from 'i18next';
import { AutoStackDialogHost, INTERVALS, intervalText } from './AutoStackDialog';
import { useAutoStackDialog } from '../../utils/stacks';
import { canUndo, useCatalogUndoStore } from '../../utils/appUndo';
import { useLibraryStore } from '../../store/useLibraryStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../../activity/notify', () => ({
  notify: {
    error: vi.fn(),
    info: vi.fn(),
    success: vi.fn(),
    warning: vi.fn(),
    dismiss: vi.fn(),
    update: vi.fn(),
    isActive: vi.fn(() => false)
  }
}));

const paths = ['/job/Capture/1.ARW', '/job/Capture/2.ARW', '/job/Capture/3.ARW', '/job/Capture/4.ARW'];

beforeEach(() => {
  localStorage.clear();
  vi.mocked(invoke).mockReset();
  useCatalogUndoStore.setState(useCatalogUndoStore.getInitialState());
  useLibraryStore.setState({ sessionDirectory: '/job' });
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const { options } = (args ?? {}) as {
      options?: { intervalSecs: number; useCameraData: boolean; similarity: number | null };
    };
    if (command === 'stacks_auto_plan') {
      // A wider interval joins all four; camera data makes a bracket.
      const wide = options!.intervalSecs >= 120;
      return {
        library: '/job',
        groups: wide
          ? [{ kind: 'time', assets: ['1', '2', '3', '4'] }]
          : [{ kind: options!.useCameraData ? 'bracket' : 'time', assets: ['1', '2'] }],
        photos: wide ? 4 : 2,
        considered: 3,
        unread: options!.similarity === null ? 0 : 1
      };
    }
    if (command === 'stacks_auto_apply') return { library: '/job', change: { before: [], after: [{ id: 'x' }] } };
    return [];
  });
});
afterEach(() => {
  cleanup();
  useAutoStackDialog.getState().close();
});

it('previews the stacks as the interval moves and makes them as one undo step', async () => {
  useAutoStackDialog.getState().open(paths);
  render(<AutoStackDialogHost />);
  await screen.findByText('1 stack from 2 photos');
  expect(screen.getByText('1 bracket')).toBeTruthy();
  expect(screen.getByText('1 photo stays single')).toBeTruthy();
  expect(screen.getByText('1 photo without a capture time or already in a stack is left out')).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith('stacks_auto_plan', {
    paths,
    options: { intervalSecs: 2, useCameraData: true, similarity: 0.78 }
  });
  expect(screen.getByText('1 photo not read yet is grouped by time alone')).toBeTruthy();

  const slider = screen.getByRole('slider', { name: /time/i });
  fireEvent.change(slider, { target: { value: String(INTERVALS.indexOf(300)) } });
  await screen.findByText('1 stack from 4 photos');
  fireEvent.click(screen.getByRole('checkbox', { name: /camera/i }));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith('stacks_auto_plan', {
      paths,
      options: { intervalSecs: 300, useCameraData: false, similarity: 0.78 }
    })
  );

  fireEvent.click(screen.getByRole('button', { name: 'Make 1 stack' }));
  await waitFor(() => expect(useAutoStackDialog.getState().paths).toBeNull());
  expect(invoke).toHaveBeenCalledWith('stacks_auto_apply', {
    library: '/job',
    groups: [{ kind: 'time', assets: ['1', '2', '3', '4'] }]
  });
  expect(canUndo()).toBe(true);
  // The next run starts at the interval used (Lightroom remembers it).
  useAutoStackDialog.getState().open(paths);
  await waitFor(() =>
    expect(invoke).toHaveBeenLastCalledWith('stacks_auto_plan', {
      paths,
      options: { intervalSecs: 300, useCameraData: true, similarity: 0.78 }
    })
  );
});

it('compares the look of consecutive photos unless turned off, and remembers it', async () => {
  useAutoStackDialog.getState().open(paths);
  render(<AutoStackDialogHost />);
  await screen.findByText('1 stack from 2 photos');
  const look = screen.getByRole('slider', { name: /alike/i });
  fireEvent.change(look, { target: { value: '0.9' } });
  await waitFor(() =>
    expect(invoke).toHaveBeenLastCalledWith('stacks_auto_plan', {
      paths,
      options: { intervalSecs: 2, useCameraData: true, similarity: 0.9 }
    })
  );
  fireEvent.click(screen.getByRole('checkbox', { name: /look alike/i }));
  await waitFor(() =>
    expect(invoke).toHaveBeenLastCalledWith('stacks_auto_plan', {
      paths,
      options: { intervalSecs: 2, useCameraData: true, similarity: null }
    })
  );
  expect(screen.queryByRole('slider', { name: /alike/i })).toBeNull();
  expect(screen.queryByText(/not read yet/)).toBeNull();
  // Back on at the likeness last chosen.
  fireEvent.click(screen.getByRole('checkbox', { name: /look alike/i }));
  await waitFor(() =>
    expect(invoke).toHaveBeenLastCalledWith('stacks_auto_plan', {
      paths,
      options: { intervalSecs: 2, useCameraData: true, similarity: 0.9 }
    })
  );
  fireEvent.click(screen.getByRole('checkbox', { name: /look alike/i }));
  await screen.findByText('1 stack from 2 photos');
  fireEvent.click(screen.getByRole('button', { name: 'Make 1 stack' }));
  await waitFor(() => expect(useAutoStackDialog.getState().paths).toBeNull());
  expect(localStorage.getItem('rawtonic.autoStack.similarity')).toBe('off');
});

it('names intervals from a second to an hour', () => {
  const t = i18n.t.bind(i18n);
  expect(INTERVALS[0]).toBe(1);
  expect(INTERVALS.at(-1)).toBe(3600);
  expect(intervalText(1, t)).toBe('1 second');
  expect(intervalText(90, t)).toBe('1 min 30 s');
  expect(intervalText(3600, t)).toBe('60 minutes');
});

it('shows the message of a structured backend refusal', async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === 'stacks_auto_plan') throw { code: 'not_found', message: 'These photos are not in a library' };
    return [];
  });
  useAutoStackDialog.getState().open(paths);
  render(<AutoStackDialogHost />);
  await screen.findByText(/These photos are not in a library/);
  expect(screen.queryByText(/\[object Object\]/)).toBeNull();
});
