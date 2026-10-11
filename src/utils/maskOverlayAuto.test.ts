import { describe, expect, it } from 'vitest';
import { maskOverlayAfterEdit, maskOverlayOpacity } from './maskDisplay';
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER, type MaskContainer } from './adjustments';
import { createSubMask } from './maskUtils';
import { Mask } from '../components/panel/right/Masks';
import { useEditorStore } from '../store/useEditorStore';

// Lightroom's automatic overlay: notes: reference/layers-panel-c1-lr.md.
describe('automatic mask overlay', () => {
  const layer = (id: string, patch: Partial<MaskContainer> = {}): MaskContainer => ({
    ...structuredClone(INITIAL_MASK_CONTAINER),
    id,
    name: id,
    subMasks: [],
    ...patch,
  });
  const a = layer('a');
  const b = layer('b');

  it('shows the overlay when a layer is made or its mask changes', () => {
    expect(maskOverlayAfterEdit('hidden', [a], [a, b], 'a')).toBe('auto');
    expect(maskOverlayAfterEdit('pinned', [a], [{ ...a, subMasks: [] }], 'a')).toBe('auto');
    expect(maskOverlayAfterEdit('hidden', [a], [{ ...a, invert: true }], 'a')).toBe('auto');
  });

  it('switches it off at the first adjustment of the edited layer only', () => {
    const edited = { ...a, adjustments: { ...a.adjustments, exposure: 0.5 } };
    expect(maskOverlayAfterEdit('auto', [a, b], [edited, b], 'a')).toBe('hidden');
    expect(maskOverlayAfterEdit('auto', [a], [{ ...a, opacity: 60 }], 'a')).toBe('hidden');
    // Another layer, or nothing selected, leaves it.
    expect(maskOverlayAfterEdit('auto', [a, b], [edited, b], 'b')).toBe('auto');
    expect(maskOverlayAfterEdit('auto', [a], [edited], null)).toBe('auto');
    // Shown again by the user (M), it stays on through further adjustments.
    expect(maskOverlayAfterEdit('pinned', [a], [edited], 'a')).toBe('pinned');
    // Renaming or hiding the layer is neither.
    expect(maskOverlayAfterEdit('auto', [a], [{ ...a, name: 'x', visible: false }], 'a')).toBe('auto');
    expect(maskOverlayAfterEdit('hidden', [a], [a], 'a')).toBe('hidden');
  });

  it('keeps the overlay hidden until the next stroke or shape drag', () => {
    const base = {
      display: 'always' as const,
      showingOriginal: false,
      sliderDragging: false,
      masking: true,
      drawing: false,
      shapeInteracting: false,
      toolActive: true,
      controlHovered: false,
    };
    expect(maskOverlayOpacity({ ...base, autoHidden: true })).toBe(0);
    expect(maskOverlayOpacity({ ...base, autoHidden: true, drawing: true })).toBe(1);
    expect(maskOverlayOpacity({ ...base, autoHidden: true, shapeInteracting: true })).toBe(1);
    expect(maskOverlayOpacity({ ...base, autoHidden: true, rangeDragging: true })).toBe(1);
    expect(maskOverlayOpacity({ ...base, autoHidden: false })).toBe(1);
  });
});

describe('editor store: automatic overlay and luma eyedropper', () => {
  const image = (path: string) => ({ path, isReady: true, width: 100, height: 100 }) as never;
  const state = () => useEditorStore.getState();
  const layerOf = () => state().adjustments.masks[0];
  const setLayer = (patch: Partial<MaskContainer>) =>
    useEditorStore.setState((s) => ({
      adjustments: { ...s.adjustments, masks: [{ ...s.adjustments.masks[0], ...patch }] },
    }));

  it('follows the edits of the active layer', () => {
    useEditorStore.setState({
      selectedImage: image('/a.raf'),
      adjustments: { ...INITIAL_ADJUSTMENTS, masks: [] },
      activeMaskContainerId: null,
      maskOverlayAuto: 'hidden',
    });
    // A new layer (added, then selected, as the layer tools do) shows its mask.
    useEditorStore.setState((s) => ({
      adjustments: { ...s.adjustments, masks: [{ ...structuredClone(INITIAL_MASK_CONTAINER), id: 'l' }] },
    }));
    state().setEditor({ activeMaskContainerId: 'l' });
    expect(state().maskOverlayAuto).toBe('auto');
    // The first adjustment switches it off; more keep it off.
    setLayer({ adjustments: { ...layerOf().adjustments, exposure: 0.5 } });
    expect(state().maskOverlayAuto).toBe('hidden');
    setLayer({ adjustments: { ...layerOf().adjustments, exposure: 0.7 } });
    expect(state().maskOverlayAuto).toBe('hidden');
    // Painting shows it again.
    setLayer({ subMasks: [createSubMask(Mask.Brush, { width: 100, height: 100 })] });
    expect(state().maskOverlayAuto).toBe('auto');
    // Shown by M, it stays through adjustments.
    state().setEditor({ maskOverlayAuto: 'pinned' });
    setLayer({ adjustments: { ...layerOf().adjustments, exposure: 1 } });
    expect(state().maskOverlayAuto).toBe('pinned');
    // Another photo starts automatic again.
    state().setEditor({ maskOverlayAuto: 'hidden' });
    state().setEditor({ selectedImage: image('/b.raf') });
    expect(state().maskOverlayAuto).toBe('auto');
  });

  it('ends the luma eyedropper when another layer or component is picked', () => {
    useEditorStore.setState({ selectedImage: image('/a.raf'), activeMaskContainerId: 'l', activeMaskId: 'x' });
    // Started together with a selection change, it stays on.
    state().setEditor({ activeMaskId: 'y', lumaPickerLayer: 'l' });
    expect(state().lumaPickerLayer).toBe('l');
    state().setEditor({ zoom: 2 });
    expect(state().lumaPickerLayer).toBe('l');
    state().setEditor({ activeMaskId: 'z' });
    expect(state().lumaPickerLayer).toBeNull();
    state().setEditor({ lumaPickerLayer: 'l' });
    state().setEditor({ activeMaskContainerId: 'm' });
    expect(state().lumaPickerLayer).toBeNull();
  });
});
