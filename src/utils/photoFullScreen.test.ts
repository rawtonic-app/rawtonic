import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useUIStore } from '../store/useUIStore';
import { useEditorStore } from '../store/useEditorStore';
import { forgetPhotoFullScreen, photoFullScreenOwnsWindow, photoFullScreenSettled } from './photoFullScreen';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));

// A window whose full screen changes as the native one does: the resize
// listener (App.tsx) reports each change to the store.
let native = false;
const setFullscreen = vi.fn(async (value: boolean) => {
  native = value;
  useUIStore.getState().noteWindowFullScreen(value);
});
const isFullscreen = vi.fn(async () => native);

beforeEach(() => {
  native = false;
  vi.mocked(getCurrentWindow).mockReturnValue({ isFullscreen, setFullscreen } as never);
  useEditorStore.setState({ selectedImage: { path: '/photos/a.dng' } as never, zoom: 1 });
  useUIStore.setState({ isFullScreen: false, isWindowFullScreen: false });
});

afterEach(async () => {
  await photoFullScreenSettled();
  forgetPhotoFullScreen();
  vi.clearAllMocks();
});

const press = async () => {
  useUIStore.getState().toggleFullScreen();
  await photoFullScreenSettled();
};
const ui = () => {
  const { isFullScreen, isWindowFullScreen } = useUIStore.getState();
  return { isFullScreen, isWindowFullScreen };
};

it('F shows the photo alone in native full screen and F again restores the window', async () => {
  await press();
  expect(ui()).toEqual({ isFullScreen: true, isWindowFullScreen: true });
  expect(setFullscreen.mock.calls).toEqual([[true]]);
  expect(photoFullScreenOwnsWindow()).toBe(true);

  await press();
  expect(ui()).toEqual({ isFullScreen: false, isWindowFullScreen: false });
  expect(setFullscreen.mock.calls).toEqual([[true], [false]]);
  expect(photoFullScreenOwnsWindow()).toBe(false);
});

it('a window already in full screen stays in full screen; only the panels come back', async () => {
  native = true;
  useUIStore.getState().noteWindowFullScreen(true);

  await press();
  expect(ui()).toEqual({ isFullScreen: true, isWindowFullScreen: true });
  await press();
  expect(ui()).toEqual({ isFullScreen: false, isWindowFullScreen: true });
  expect(setFullscreen).not.toHaveBeenCalled();
});

it('leaving full screen by the green button or ⌃⌘F ends the photo-only view', async () => {
  await press();
  // The window leaves full screen without F.
  native = false;
  useUIStore.getState().noteWindowFullScreen(false);
  expect(ui()).toEqual({ isFullScreen: false, isWindowFullScreen: false });
  expect(photoFullScreenOwnsWindow()).toBe(false);

  // F then enters again instead of taking the window out a second time.
  await press();
  expect(ui()).toEqual({ isFullScreen: true, isWindowFullScreen: true });
  expect(setFullscreen.mock.calls).toEqual([[true], [true]]);
});

it('leaving an earlier full screen externally also ends the photo-only view', async () => {
  native = true;
  useUIStore.getState().noteWindowFullScreen(true);
  await press();
  native = false;
  useUIStore.getState().noteWindowFullScreen(false);
  expect(ui()).toEqual({ isFullScreen: false, isWindowFullScreen: false });
  expect(setFullscreen).not.toHaveBeenCalled();
});

it('resizes in full screen and the entering animation do not end the view', async () => {
  // The window reports windowed size changes before full screen is reached.
  setFullscreen.mockImplementationOnce(async (value: boolean) => {
    useUIStore.getState().noteWindowFullScreen(false);
    native = value;
    useUIStore.getState().noteWindowFullScreen(value);
    useUIStore.getState().noteWindowFullScreen(value);
  });
  await press();
  expect(ui()).toEqual({ isFullScreen: true, isWindowFullScreen: true });
});

it('a quick F F leaves the window as it was', async () => {
  useUIStore.getState().toggleFullScreen();
  useUIStore.getState().toggleFullScreen();
  await photoFullScreenSettled();
  expect(ui()).toEqual({ isFullScreen: false, isWindowFullScreen: false });
  expect(setFullscreen).not.toHaveBeenCalled();
});

it('Escape-style exit before the window reached full screen still restores it', async () => {
  let finish!: () => void;
  setFullscreen.mockImplementationOnce(
    (value: boolean) =>
      new Promise<void>((resolve) => {
        finish = () => {
          native = value;
          useUIStore.getState().noteWindowFullScreen(value);
          resolve();
        };
      }),
  );
  useUIStore.getState().toggleFullScreen();
  await vi.waitFor(() => expect(setFullscreen).toHaveBeenCalledTimes(1));
  useUIStore.getState().toggleFullScreen();
  finish();
  await photoFullScreenSettled();
  expect(ui()).toEqual({ isFullScreen: false, isWindowFullScreen: false });
  expect(setFullscreen.mock.calls).toEqual([[true], [false]]);
});

it('without a photograph F does nothing', async () => {
  useEditorStore.setState({ selectedImage: null });
  await press();
  expect(ui()).toEqual({ isFullScreen: false, isWindowFullScreen: false });
  expect(setFullscreen).not.toHaveBeenCalled();
});
