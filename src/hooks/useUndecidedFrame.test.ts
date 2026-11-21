import { cleanup, renderHook, waitFor, act } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const { invoke, notify } = vi.hoisted(() => ({
  invoke: vi.fn(),
  notify: { warn: vi.fn(), dismiss: vi.fn(), info: vi.fn(), error: vi.fn() }
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../activity/notify', () => ({ notify }));

import '../i18n';
import { useUndecidedFrame } from './useUndecidedFrame';
import { useEditorStore } from '../store/useEditorStore';
import { INITIAL_ADJUSTMENTS } from '../utils/adjustments';

afterEach(() => {
  cleanup();
  invoke.mockReset();
  vi.clearAllMocks();
});

const open = (path: string) => {
  const initial = { ...structuredClone(INITIAL_ADJUSTMENTS), crop: { x: 10, y: 10, width: 100, height: 80 } };
  useEditorStore.setState({
    selectedImage: { path, isReady: true, isRaw: true } as never,
    adjustments: initial as never,
    history: [initial as never],
    historyIndex: 0
  });
};

describe('a camera TIFF edit that cannot tell its frame', () => {
  it('opens as saved and offers the move to the full image as one undo step', async () => {
    open('/s/a.tif');
    const moved = { ...structuredClone(INITIAL_ADJUSTMENTS), crop: { x: 87, y: 87, width: 867, height: 693 } };
    invoke.mockResolvedValue(moved);
    renderHook(() => useUndecidedFrame());
    await waitFor(() => expect(notify.warn).toHaveBeenCalledTimes(1));
    expect(useEditorStore.getState().adjustments.crop).toEqual({ x: 10, y: 10, width: 100, height: 80 });
    const [, options] = notify.warn.mock.calls[0];
    await act(async () => options.actions[0].run());
    expect(invoke).toHaveBeenLastCalledWith('undecided_frame_move', expect.objectContaining({ path: '/s/a.tif' }));
    expect(useEditorStore.getState().adjustments.crop).toEqual(moved.crop);
    expect(useEditorStore.getState().history).toHaveLength(2);
    act(() => useEditorStore.getState().undo());
    expect(useEditorStore.getState().adjustments.crop).toEqual({ x: 10, y: 10, width: 100, height: 80 });
  });

  it('asks nothing for other files and says nothing when the recipe tells', async () => {
    open('/s/a.NEF');
    renderHook(() => useUndecidedFrame());
    expect(invoke).not.toHaveBeenCalled();
    cleanup();
    invoke.mockResolvedValue(null);
    open('/s/b.TIFF');
    renderHook(() => useUndecidedFrame());
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    expect(notify.warn).not.toHaveBeenCalled();
  });
});
