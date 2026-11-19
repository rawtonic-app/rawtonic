import { act, cleanup, render } from '@testing-library/react';
import type { ComponentProps } from 'react';
import '../../../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import ImageCanvas from './ImageCanvas';
import { INITIAL_ADJUSTMENTS } from '../../../utils/adjustments';
import { useSettingsStore } from '../../../store/useSettingsStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

// The decoded frame is shown only once its decode resolves (jsdom decodes no
// pixels); the stub holds every decode so a test settles the ones it means.
const decode = vi.hoisted(() => {
  const pending: (() => void)[] = [];
  class StubImage {
    src = '';
    naturalWidth = 2560;
    onload: (() => void) | null = null;
    decode() {
      return new Promise<void>((resolve) => pending.push(resolve));
    }
  }
  return { pending, StubImage };
});

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
  // Typed as ImageCanvas' own props (its props interface is not exported), so
  // the compiler checks every prop the test passes.
  const props: ComponentProps<typeof ImageCanvas> = {
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
    selectedImage: {
      path: '/a.raw',
      width: 4000,
      height: 3000,
      exif: null,
      isRaw: true,
      isReady: true,
      thumbnailUrl: 'blob:thumbnail'
    },
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
  return render(<ImageCanvas {...props} />);
}

/** Settles the decode that has not resolved yet and renders its result. */
async function decodeNextFrame() {
  await act(async () => {
    decode.pending.shift()?.();
  });
}

/** The photograph's frame in the pane's drawing, or null before one is shown. */
function decodedFrame(pane: ParentNode) {
  return pane.querySelector('svg.pointer-events-none image');
}

beforeEach(() => {
  // The Linux editor draws the photograph in the photo pane outside its
  // zoomed viewer (Editor.tsx), which the canvas receives as `photoHost`.
  useSettingsStore.setState({ osPlatform: 'linux' });
  vi.stubGlobal('Image', decode.StubImage);
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

it("shows the decoded preview in the photo pane, the picker's stage in the viewer", async () => {
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
  // Nothing is drawn before the decode resolves, then the decoded frame is,
  // at the pane's whole size.
  expect(decode.pending).toHaveLength(1);
  expect(decodedFrame(host)).toBeNull();
  await decodeNextFrame();
  const frame = decodedFrame(host);
  expect(frame).not.toBeNull();
  expect(frame!.getAttribute('href')).toBe('blob:frame');
  expect(frame!.getAttribute('width')).toBe('100%');
  expect(frame!.getAttribute('height')).toBe('100%');
  host.remove();
});

it('draws the photograph in place without a photo pane', async () => {
  const { container } = renderCanvas(null, false);
  const photo = container.querySelector('svg.pointer-events-none');
  expect(photo).not.toBeNull();
  expect(decode.pending).toHaveLength(1);
  await decodeNextFrame();
  expect(decodedFrame(container)?.getAttribute('href')).toBe('blob:frame');
});
