import { act, renderHook } from '@testing-library/react';
import { beforeEach, expect, it, vi } from 'vitest';

const { invoke, exit } = vi.hoisted(() => ({ invoke: vi.fn(), exit: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@tauri-apps/plugin-process', () => ({ exit }));

import { useExternalEditSession } from './useExternalEditSession';
import { useProcessStore, type ExportJob } from '../store/useProcessStore';
import { useEditorStore } from '../store/useEditorStore';

const job = (id: string, state: ExportJob['state']): ExportJob => ({
  id,
  label: id,
  state,
  createdAt: '',
  total: 1,
  done: state === 'done' ? 1 : 0,
  failed: state === 'failed' ? 1 : 0,
  failures: [],
});

beforeEach(() => {
  invoke.mockReset();
  exit.mockReset();
  invoke.mockResolvedValue('mine');
  useProcessStore.setState({
    exportQueue: [],
    externalEditSession: { source: '/a.dng', output: '/out/a.tif', format: 'tif', jpegQuality: 90 } as never,
  });
  useEditorStore.setState({ selectedImage: { path: '/a.dng', isReady: true } as never });
});

it('quits only when its own export is done, not when a batch queued before it completes', async () => {
  const { result } = renderHook(() => useExternalEditSession(vi.fn()));
  await act(() => result.current.finishExternalEdit());
  // The batch ahead of it finishes first.
  act(() => useProcessStore.setState({ exportQueue: [job('batch', 'done'), job('mine', 'queued')] }));
  act(() => useProcessStore.getState().setExportState({ status: 'success' as never }));
  expect(exit).not.toHaveBeenCalled();
  expect(result.current.isFinishing).toBe(true);
  act(() => useProcessStore.setState({ exportQueue: [job('batch', 'done'), job('mine', 'done')] }));
  expect(exit).toHaveBeenCalledWith(0);
});

it('stays open when its export fails', async () => {
  const { result } = renderHook(() => useExternalEditSession(vi.fn()));
  await act(() => result.current.finishExternalEdit());
  act(() => useProcessStore.setState({ exportQueue: [job('mine', 'failed')] }));
  expect(exit).not.toHaveBeenCalled();
  expect(result.current.isFinishing).toBe(false);
});

it('can be pressed again when its waiting export was removed from the queue', async () => {
  const { result } = renderHook(() => useExternalEditSession(vi.fn()));
  await act(() => result.current.finishExternalEdit());
  // Not listed yet: still waiting.
  expect(result.current.isFinishing).toBe(true);
  act(() => useProcessStore.setState({ exportQueue: [job('batch', 'running'), job('mine', 'queued')] }));
  expect(result.current.isFinishing).toBe(true);
  act(() => useProcessStore.setState({ exportQueue: [job('batch', 'running')] }));
  expect(result.current.isFinishing).toBe(false);
  expect(exit).not.toHaveBeenCalled();
});
