import { cleanup, render } from '@testing-library/react';
import '../../../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import ImageCanvas from './ImageCanvas';
import { INITIAL_ADJUSTMENTS } from '../../../utils/adjustments';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

const size = {
  width: 400,
  height: 300,
  scale: 0.1,
  offsetX: 10,
  offsetY: 20,
  containerWidth: 420,
  containerHeight: 340
};

function renderCanvas(photoHost: HTMLElement | null, picker: boolean) {
  const noop = () => {};
  const props = {
    appSettings: null,
    activeMaskContainerId: null,
    activeMaskId: null,
    adjustments: INITIAL_ADJUSTMENTS,
    brushSettings: null,
    crop: null,
    finalPreviewUrl: 'blob:frame',
    handleCropComplete: noop,
    imageRenderSize: size,
    isCropping: false,
    isMaskControlHovered: false,
    isMasking: false,
    isSliderDragging: false,
    isStraightenActive: false,
    maskOverlayUrl: null,
    onGenerateAiMask: noop,
    onSelectMask: noop,
    onStraighten: noop,
    selectedImage: { path: '/a.raw', width: 4000, height: 3000, isReady: true },
    setCrop: noop,
    setIsMaskHovered: noop,
    setIsMaskTouchInteracting: noop,
    showOriginal: false,
    uncroppedAdjustedPreviewUrl: null,
    updateSubMask: noop,
    isWbPickerActive: picker,
    setAdjustments: noop,
    cursorStyle: 'default',
    transformState: { scale: 4, positionX: 0, positionY: 0 },
    photoHost,
    hasRenderedFirstFrame: false
  };
  return render(<ImageCanvas {...(props as never)} />);
}

beforeEach(() => {
  // jsdom has no 2D canvas; Konva's stage only needs calls that do nothing.
  const context = new Proxy({} as Record<PropertyKey, unknown>, {
    get: (target, key) => (key in target ? target[key] : () => ({ data: [] })),
    set: (target, key, value) => {
      target[key] = value;
      return true;
    }
  });
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(context as never);
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

it("draws the photograph in the photo pane, the picker's stage in the viewer", () => {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const { container } = renderCanvas(host, true);
  const photo = host.querySelector('svg.pointer-events-none');
  // The photograph's frames at the fit's place, outside the viewer whose
  // drawing stage WebKitGTK composites.
  expect(photo).not.toBeNull();
  expect(container.contains(photo)).toBe(false);
  expect((photo as SVGElement).style.left).toBe('10px');
  expect((photo as SVGElement).style.top).toBe('20px');
  expect(container.querySelector('.konvajs-content')).not.toBeNull();
  host.remove();
});

it('draws the photograph in place without a photo pane', () => {
  const { container } = renderCanvas(null, false);
  expect(container.querySelector('svg.pointer-events-none')).not.toBeNull();
});
