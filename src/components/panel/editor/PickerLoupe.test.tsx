import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import '../../../i18n';
import { useRef } from 'react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { PickerLoupe, usePointerSampling } from './PickerLoupe';
import { PointerReadout } from '../../studio/ViewerControls';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { usePointerSampleStore, type PreviewPatch } from '../../../utils/pointerSample';
import type { AppSettings } from '../../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
const frameReady = vi.hoisted(() => ({ handler: null as null | ((event: { payload: { path: string } }) => void) }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((_name: string, handler: (event: { payload: { path: string } }) => void) => {
    frameReady.handler = handler;
    return Promise.resolve(() => {});
  }),
}));

const patch: PreviewPatch = {
  column: 0,
  row: 0,
  width: 11,
  height: 11,
  pixels: Array.from({ length: 121 * 3 }, () => 128),
  center: [5, 5],
  mean: [127.5, 51, 255],
  frame: [400, 300],
};

function Viewer({ enabled, picker }: { enabled: boolean; picker: boolean }) {
  const root = useRef<HTMLDivElement>(null);
  const image = useRef<HTMLDivElement>(null);
  usePointerSampling(root, image, '/a.raw', enabled);
  return (
    <div className="studio-viewer" data-testid="viewer">
      <div ref={root} data-testid="root">
        <div ref={image} data-testid="image" />
        {picker && <PickerLoupe unit="rgb255" anchorRef={root} />}
      </div>
    </div>
  );
}

const rect = (left: number, top: number, width: number, height: number) =>
  ({ left, top, width, height, right: left + width, bottom: top + height, x: left, y: top, toJSON: () => ({}) }) as DOMRect;

beforeEach(() => {
  vi.stubGlobal('requestAnimationFrame', (run: () => void) => {
    run();
    return 1;
  });
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockResolvedValue(patch);
  usePointerSampleStore.setState({ sample: null });
  useSettingsStore.setState({
    appSettings: { pointerReadout: true, pointerReadoutUnit: 'rgb255' } as AppSettings,
    handleSettingsChange: vi.fn(async (next: AppSettings) => {
      useSettingsStore.setState({ appSettings: next });
    }),
  } as never);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const setup = (enabled = true, picker = true) => {
  render(<Viewer enabled={enabled} picker={picker} />);
  // The viewer's box on screen; the zoomed canvas inside it is larger.
  screen.getByTestId('viewer').getBoundingClientRect = () => rect(0, 0, 1000, 800);
  screen.getByTestId('root').getBoundingClientRect = () => rect(-2000, -1500, 5000, 4000);
  // The photograph shown 400 × 300 at (100, 50), as zoomed and panned.
  screen.getByTestId('image').getBoundingClientRect = () => rect(100, 50, 400, 300);
};

it('samples the shown photograph under the pointer and shows the loupe beside it', async () => {
  setup();
  await act(async () => {
    fireEvent.pointerMove(screen.getByTestId('root'), { clientX: 300, clientY: 125, buttons: 0 });
  });
  expect(invoke).toHaveBeenCalledWith('sample_preview_patch', { path: '/a.raw', normalizedX: 0.5, normalizedY: 0.25 });
  const loupe = await screen.findByTestId('picker-loupe');
  expect(loupe.textContent).toContain('R 128');
  expect(loupe.textContent).toContain('G 51');
  expect(loupe.textContent).toContain('B 255');
  // Below right of the pointer, not over it, in the window's coordinates
  // (outside the zoomed canvas).
  expect(loupe.parentElement).toBe(document.body);
  expect(loupe.style.left).toBe('324px');
  expect(loupe.style.top).toBe('149px');
});

it('hides off the photograph and does not sample during a drag', async () => {
  setup();
  const root = screen.getByTestId('root');
  await act(async () => {
    fireEvent.pointerMove(root, { clientX: 300, clientY: 125, buttons: 0 });
  });
  await screen.findByTestId('picker-loupe');
  await act(async () => {
    fireEvent.pointerMove(root, { clientX: 50, clientY: 125, buttons: 0 });
  });
  expect(screen.queryByTestId('picker-loupe')).toBeNull();
  vi.mocked(invoke).mockClear();
  await act(async () => {
    fireEvent.pointerMove(root, { clientX: 300, clientY: 125, buttons: 1 });
  });
  expect(invoke).not.toHaveBeenCalled();
  await act(async () => {
    fireEvent.pointerMove(root, { clientX: 300, clientY: 125, buttons: 0 });
  });
  await act(async () => {
    fireEvent.pointerLeave(root);
  });
  expect(screen.queryByTestId('picker-loupe')).toBeNull();
});

it('samples again when a new frame is shown under a resting pointer', async () => {
  setup();
  await act(async () => {
    fireEvent.pointerMove(screen.getByTestId('root'), { clientX: 300, clientY: 125, buttons: 0 });
  });
  expect(invoke).toHaveBeenCalledTimes(1);
  await act(async () => {
    frameReady.handler?.({ payload: { path: '/b.raw' } });
  });
  expect(invoke).toHaveBeenCalledTimes(1);
  await act(async () => {
    frameReady.handler?.({ payload: { path: '/a.raw' } });
  });
  expect(invoke).toHaveBeenCalledTimes(2);
});

it('does nothing while off', async () => {
  setup(false);
  await act(async () => {
    fireEvent.pointerMove(screen.getByTestId('root'), { clientX: 300, clientY: 125, buttons: 0 });
  });
  expect(invoke).not.toHaveBeenCalled();
  expect(screen.queryByTestId('picker-loupe')).toBeNull();
});

it('shows the readout in the viewer bar and switches its unit on click', async () => {
  usePointerSampleStore.setState({ sample: { clientX: 0, clientY: 0, patch } });
  render(<PointerReadout />);
  const readout = screen.getByTestId('pointer-readout');
  expect(readout.textContent).toBe('R128G51B255');
  await act(async () => {
    fireEvent.click(readout);
  });
  expect(useSettingsStore.getState().appSettings?.pointerReadoutUnit).toBe('percent');
  expect(screen.getByTestId('pointer-readout').textContent).toBe('R50.0G20.0B100.0%');
  act(() => usePointerSampleStore.setState({ sample: null }));
  expect(screen.getByTestId('pointer-readout').textContent).toBe('R–G–B–%');
  act(() => useSettingsStore.setState({ appSettings: { pointerReadout: false } as AppSettings }));
  expect(screen.queryByTestId('pointer-readout')).toBeNull();
});
