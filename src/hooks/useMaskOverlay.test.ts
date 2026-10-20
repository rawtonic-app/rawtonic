import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useEditorStore } from '../store/useEditorStore';
import { useMaskOverlay } from './useMaskOverlay';
import type { SelectedImage } from '../components/ui/AppProperties';
import type { SubMask } from '../components/panel/right/Masks';
import type { Adjustments, MaskContainer } from '../utils/adjustments';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('./usePerformanceDiagnostics', () => ({ performanceDiagnosticsEnabled: false, reportPerformance: vi.fn(), trackMaskOverlayLoad: vi.fn(), forgetMaskOverlayLoad: vi.fn(), registerDiagnosticQueue: vi.fn(), notePointerSessionFrame: vi.fn(), epochOf: (time: number) => time }));

const size = { width: 800, height: 600, scale: 0.5 };
const recipe = { masks: [] as MaskContainer[] } as Adjustments;
const mask = (id: string) => ({ id, visible: true, subMasks: [] });
const bytes = () => new Uint8Array([137, 80, 78, 71]).buffer;
const select = (path: string) => useEditorStore.setState({ selectedImage: { path, isReady: true } as SelectedImage });

describe('binary mask overlays', () => {
  beforeEach(() => {
    invoke.mockReset();
    select('/first.raf');
    useEditorStore.setState({ activeMaskContainerId: 'mask', activeMaskId: 'child', patchesSentToBackend: new Map() });
    URL.createObjectURL = vi.fn().mockReturnValueOnce('blob:first').mockReturnValueOnce('blob:second');
    URL.revokeObjectURL = vi.fn();
  });
  afterEach(cleanup);

  it('keeps the latest pending pointer state and releases replaced and unmounted URLs', async () => {
    let resolveFirst!: (bytes: ArrayBuffer) => void;
    let resolveSecond!: (bytes: ArrayBuffer) => void;
    invoke.mockImplementationOnce(() => new Promise((resolve) => { resolveFirst = resolve; }))
      .mockImplementationOnce(() => new Promise((resolve) => { resolveSecond = resolve; }));
    const { result, unmount } = renderHook(() => useMaskOverlay());
    act(() => {
      result.current.requestMaskOverlay(mask('a'), size, recipe);
      result.current.requestMaskOverlay(mask('b'), size, recipe);
      result.current.requestMaskOverlay(mask('c'), size, recipe);
    });
    expect(invoke).toHaveBeenCalledTimes(1);
    await act(async () => resolveFirst(bytes()));
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    expect(invoke.mock.calls[1][1].maskDef.id).toBe('c');
    expect(result.current.maskOverlayUrl).toBe('blob:first');
    await act(async () => resolveSecond(bytes()));
    expect(result.current.maskOverlayUrl).toBe('blob:second');
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:first');
    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:second');
  });

  it('ignores an old photo response and clears its pending strokes on selection', async () => {
    let resolve!: (bytes: ArrayBuffer) => void;
    invoke.mockImplementation(() => new Promise((done) => { resolve = done; }));
    const { result } = renderHook(() => useMaskOverlay());
    act(() => {
      result.current.requestMaskOverlay(mask('old'), size, recipe);
      result.current.requestMaskOverlay(mask('pending-old'), size, recipe);
    });
    act(() => select('/second.raf'));
    await act(async () => resolve(bytes()));
    expect(URL.createObjectURL).not.toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(result.current.maskOverlayUrl).toBeNull();
  });

  it('does not create a URL after unmount and strips already-cached masks without mutating the recipe', async () => {
    let resolve!: (bytes: ArrayBuffer) => void;
    invoke.mockImplementation(() => new Promise((done) => { resolve = done; }));
    useEditorStore.setState({ patchesSentToBackend: new Map([['cached', 'large-data']]) });
    const source = { ...mask('active'), subMasks: [{ id: 'cached', parameters: { maskDataBase64: 'large-data' } } as SubMask] };
    const { result, unmount } = renderHook(() => useMaskOverlay());
    act(() => result.current.requestMaskOverlay(source, size, { masks: [source] } as Adjustments));
    expect(invoke.mock.calls[0][1].maskDef.subMasks[0].parameters.maskDataBase64).toBeNull();
    expect(source.subMasks[0].parameters.maskDataBase64).toBe('large-data');
    unmount();
    await act(async () => resolve(bytes()));
    expect(URL.createObjectURL).not.toHaveBeenCalled();
  });

  it.each(['activeMaskId'] as const)(
    'discards old responses and pending strokes when %s changes within the same container', async (field) => {
      let finish!: (bytes: ArrayBuffer) => void;
      invoke.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
      const { result } = renderHook(() => useMaskOverlay());
      const previousRequest = result.current.requestMaskOverlay;
      act(() => {
        result.current.requestMaskOverlay(mask('old-child'), size, recipe);
        result.current.requestMaskOverlay(mask('old-pending'), size, recipe);
      });
      await act(async () => {
        useEditorStore.setState({ [field]: 'next-child' });
        finish(bytes());
      });
      expect(URL.createObjectURL).not.toHaveBeenCalled();
      expect(invoke).toHaveBeenCalledTimes(1);
      expect(result.current.maskOverlayUrl).toBeNull();
      expect(result.current.requestMaskOverlay).not.toBe(previousRequest);
    },
  );

  it('releases intermediate URLs even when both responses finish before React commits', async () => {
    invoke.mockResolvedValue(bytes());
    const { result, unmount } = renderHook(() => useMaskOverlay());
    await act(async () => {
      result.current.requestMaskOverlay(mask('first'), size, recipe);
      result.current.requestMaskOverlay(mask('second'), size, recipe);
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(result.current.maskOverlayUrl).toBe('blob:second');
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:first');
    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:second');
  });

  it('hides immediately and invalidates an active response when the overlay is dismissed', async () => {
    let finish!: (bytes: ArrayBuffer) => void;
    invoke.mockResolvedValueOnce(bytes()).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const { result } = renderHook(() => useMaskOverlay());
    await act(async () => result.current.requestMaskOverlay(mask('shown'), size, recipe));
    expect(result.current.maskOverlayUrl).toBe('blob:first');
    act(() => {
      result.current.requestMaskOverlay(mask('busy'), size, recipe);
      result.current.requestMaskOverlay(mask('pending'), size, recipe);
      result.current.requestMaskOverlay(null, size, recipe);
    });
    expect(result.current.maskOverlayUrl).toBeNull();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:first');
    await act(async () => finish(bytes()));
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledTimes(2);
  });
});
