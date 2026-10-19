import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useEditorStore } from '../store/useEditorStore';
import {
  MASK_THUMB_SETTLE_MS,
  componentThumbnailKey,
  layerThumbnailKey,
  useMaskThumbnailStore,
  useMaskThumbnailUpdates,
} from './useMaskThumbnails';
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER, type Adjustments, type MaskContainer } from '../utils/adjustments';
import { createSubMask } from '../utils/maskUtils';
import { Mask } from '../components/panel/right/Masks';
import type { SelectedImage } from '../components/ui/AppProperties';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

const size = { width: 6000, height: 4000 };
const radial = createSubMask(Mask.Radial, size);
const linear = createSubMask(Mask.Linear, size);
const layer = (id: string, subMasks = [radial]): MaskContainer => ({
  ...structuredClone(INITIAL_MASK_CONTAINER),
  id,
  name: id,
  subMasks: structuredClone(subMasks),
});
const recipe = (masks: MaskContainer[]): Adjustments => ({ ...structuredClone(INITIAL_ADJUSTMENTS), masks });
const urlOf = (id: string) => `data:image/png;base64,${id}`;

/** Answers every request with a thumbnail per requested id. */
function answer() {
  invoke.mockImplementation(async (_command: string, args: { layerIds: string[]; componentIds: string[] }) => ({
    layers: Object.fromEntries(args.layerIds.map((id) => [id, urlOf(id)])),
    components: Object.fromEntries(args.componentIds.map((id) => [id, urlOf(id)])),
  }));
}

async function settle(ms = MASK_THUMB_SETTLE_MS) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockReset();
  answer();
  useMaskThumbnailStore.setState({ path: null, layers: {}, components: {} });
  useEditorStore.setState({
    selectedImage: { path: '/a.cr3', isReady: true, ...size } as SelectedImage,
    adjustments: recipe([layer('l1'), layer('l2', [radial, linear])]),
    activeMaskContainerId: 'l2',
    activeMaskId: radial.id,
    isSliderDragging: false,
    patchesSentToBackend: new Set(),
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('mask thumbnails', () => {
  it('requests every layer and the selected layer’s components, then only what changed', async () => {
    renderHook(() => useMaskThumbnailUpdates());
    await settle(0);
    expect(invoke).toHaveBeenCalledTimes(1);
    const [command, args] = invoke.mock.calls[0];
    expect(command).toBe('generate_mask_thumbnails');
    expect(args.layerIds).toEqual(['l1', 'l2']);
    expect(args.componentIds).toEqual([radial.id, linear.id]);
    expect(useMaskThumbnailStore.getState().layers).toEqual({ l1: urlOf('l1'), l2: urlOf('l2') });

    // Renaming or changing a layer's adjustments changes no thumbnail.
    act(() =>
      useEditorStore.setState((s) => ({
        adjustments: { ...s.adjustments, masks: s.adjustments.masks.map((m) => ({ ...m, name: 'Renamed', opacity: 40 })) },
      })),
    );
    await settle();
    expect(invoke).toHaveBeenCalledTimes(1);

    // Moving the linear gradient re-makes layer 2 and that component only.
    act(() =>
      useEditorStore.setState((s) => ({
        adjustments: {
          ...s.adjustments,
          masks: s.adjustments.masks.map((m) =>
            m.id === 'l2'
              ? { ...m, subMasks: m.subMasks.map((sub) => (sub.id === linear.id ? { ...sub, parameters: { ...sub.parameters, startX: 10 } } : sub)) }
              : m,
          ),
        },
      })),
    );
    await settle();
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(invoke.mock.calls[1][1].layerIds).toEqual(['l2']);
    expect(invoke.mock.calls[1][1].componentIds).toEqual([linear.id]);

    // A crop changes every thumbnail's frame.
    act(() =>
      useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, crop: { x: 10, y: 10, width: 100, height: 100 } } as Adjustments })),
    );
    await settle();
    expect(invoke.mock.calls[2][1].layerIds).toEqual(['l1', 'l2']);
  });

  it('waits for edits to settle and for the pointer to be released', async () => {
    renderHook(() => useMaskThumbnailUpdates());
    await settle(0);
    invoke.mockClear();
    window.dispatchEvent(new Event('pointerdown'));
    for (let step = 0; step < 5; step++) {
      act(() =>
        useEditorStore.setState((s) => ({
          adjustments: {
            ...s.adjustments,
            masks: s.adjustments.masks.map((m) =>
              m.id === 'l1' ? { ...m, subMasks: m.subMasks.map((sub) => ({ ...sub, parameters: { ...sub.parameters, radiusX: 100 + step } })) } : m,
            ),
          },
        })),
      );
      await settle(100);
    }
    await settle(1000);
    // Still dragging: nothing requested.
    expect(invoke).not.toHaveBeenCalled();
    window.dispatchEvent(new Event('pointerup'));
    await settle(200);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][1].layerIds).toEqual(['l1']);
  });

  it('drops a deleted layer’s thumbnail and starts over for another photo', async () => {
    renderHook(() => useMaskThumbnailUpdates());
    await settle(0);
    act(() => useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, masks: s.adjustments.masks.slice(1) } })));
    await settle();
    expect(Object.keys(useMaskThumbnailStore.getState().layers)).toEqual(['l2']);
    act(() => useEditorStore.setState({ selectedImage: { path: '/b.cr3', isReady: true, ...size } as SelectedImage }));
    await settle(0);
    expect(useMaskThumbnailStore.getState().path).toBe('/b.cr3');
    expect(invoke.mock.calls.at(-1)?.[1].layerIds).toEqual(['l2']);
  });

  it('keys AI bitmaps by a short summary, not their text', () => {
    const subject = { ...createSubMask(Mask.AiForeground, size), parameters: { maskDataBase64: `data:image/png;base64,${'A'.repeat(200_000)}` } };
    const key = componentThumbnailKey(subject);
    expect(key.length).toBeLessThan(1000);
    const changed = { ...subject, parameters: { maskDataBase64: `data:image/png;base64,${'A'.repeat(199_999)}B` } };
    expect(componentThumbnailKey(changed)).not.toBe(key);
    expect(layerThumbnailKey(layer('x', [subject]))).not.toBe(layerThumbnailKey(layer('x', [changed])));
  });

  it('makes a layer’s thumbnail again when its Refine edge or mask version changes', () => {
    const plain = layer('x');
    const refined = { ...plain, maskVersion: 3, refineEdge: 40 };
    expect(layerThumbnailKey(refined)).not.toBe(layerThumbnailKey(plain));
    expect(layerThumbnailKey({ ...refined, refineEdge: 80 })).not.toBe(layerThumbnailKey(refined));
    expect(layerThumbnailKey({ ...refined, maskVersion: 2 })).not.toBe(layerThumbnailKey(refined));
  });
});
