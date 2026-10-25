import { beforeEach, expect, it, vi } from 'vitest';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { followPhotoFullScreen, toggleWindowFullScreen } from './photoFullScreen';

vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));
const webviewFocus = vi.fn(async () => {});
vi.mock('@tauri-apps/api/webview', () => ({ getCurrentWebview: () => ({ setFocus: webviewFocus }) }));

let native = false;
const setFullscreen = vi.fn(async (value: boolean) => {
  native = value;
});
const isFullscreen = vi.fn(async () => native);
const windowFocus = vi.fn(async () => {});

beforeEach(() => {
  native = false;
  vi.clearAllMocks();
  vi.mocked(getCurrentWindow).mockReturnValue({ isFullscreen, setFullscreen, setFocus: windowFocus } as never);
});

it('puts the window into full screen and back', async () => {
  await toggleWindowFullScreen();
  expect(native).toBe(true);
  await toggleWindowFullScreen();
  expect(native).toBe(false);
});

it('leaves a window that is already in full screen (green button, ⌃⌘F)', async () => {
  native = true;
  await toggleWindowFullScreen();
  expect(setFullscreen).toHaveBeenCalledWith(false);
});

it('keeps the order of quick presses', async () => {
  void toggleWindowFullScreen();
  await toggleWindowFullScreen();
  expect(setFullscreen.mock.calls).toEqual([[true], [false]]);
  expect(native).toBe(false);
});

it('survives a failing window call', async () => {
  setFullscreen.mockRejectedValueOnce(new Error('denied'));
  const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
  await toggleWindowFullScreen();
  await toggleWindowFullScreen();
  expect(native).toBe(true);
  warn.mockRestore();
});

it('takes the keyboard focus back after each change, so the next F works without a click', async () => {
  await toggleWindowFullScreen();
  await toggleWindowFullScreen();
  expect(windowFocus).toHaveBeenCalledTimes(2);
  expect(webviewFocus).toHaveBeenCalledTimes(2);
});

it('takes the whole screen for the photograph alone and gives the window back afterwards', async () => {
  await followPhotoFullScreen(true);
  expect(native).toBe(true);
  await followPhotoFullScreen(false);
  expect(native).toBe(false);
});

it('leaves a window that was in full screen before the photograph alone in full screen', async () => {
  native = true;
  await followPhotoFullScreen(true);
  await followPhotoFullScreen(false);
  expect(native).toBe(true);
  expect(setFullscreen).not.toHaveBeenCalled();
});
