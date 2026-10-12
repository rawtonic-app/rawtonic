import { beforeEach, expect, it, vi } from 'vitest';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { toggleWindowFullScreen } from './photoFullScreen';

vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));

let native = false;
const setFullscreen = vi.fn(async (value: boolean) => {
  native = value;
});
const isFullscreen = vi.fn(async () => native);

beforeEach(() => {
  native = false;
  vi.clearAllMocks();
  vi.mocked(getCurrentWindow).mockReturnValue({ isFullscreen, setFullscreen } as never);
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
