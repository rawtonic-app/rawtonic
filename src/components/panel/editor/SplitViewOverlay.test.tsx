import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeAll, beforeEach, expect, it, vi } from 'vitest';
import SplitViewOverlay, { ViewerBeforeBadge } from './SplitViewOverlay';
import { useEditorStore } from '../../../store/useEditorStore';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key: string, options?: { tool?: string }) => (options?.tool ? `${key}:${options.tool}` : key) }) }));

beforeAll(() => {
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.hasPointerCapture = () => true;
  Element.prototype.releasePointerCapture = () => {};
});
beforeEach(() => useEditorStore.setState({ splitView: true, splitOrientation: 'left-right', splitPosition: 0.5, showOriginal: false, toolBefore: null }));
afterEach(cleanup);

/** The viewer spans x 100…900 on screen. */
const placeViewer = () => {
  const root = screen.getByTestId('split-view-overlay');
  root.getBoundingClientRect = () =>
    ({ left: 100, top: 0, width: 800, height: 600, right: 900, bottom: 600, x: 100, y: 0 }) as DOMRect;
};

it('shows nothing while the split view is off', () => {
  useEditorStore.setState({ splitView: false });
  render(<SplitViewOverlay />);
  expect(screen.queryByTestId('split-view-overlay')).toBeNull();
});

it('hides the divider and labels while the whole photograph shows before (held \\)', () => {
  useEditorStore.setState({ showOriginal: true });
  render(<SplitViewOverlay />);
  expect(screen.queryByTestId('split-view-overlay')).toBeNull();
});

it('labels the sides and puts the divider at the kept position', () => {
  useEditorStore.setState({ splitPosition: 0.3 });
  render(<SplitViewOverlay />);
  expect(screen.getByText('studio.workspace.viewer.splitBefore')).toBeTruthy();
  expect(screen.getByText('studio.workspace.viewer.splitAfter')).toBeTruthy();
  const divider = screen.getByRole('slider');
  expect(divider.style.left).toBe('30%');
  expect(divider.getAttribute('aria-valuenow')).toBe('30');
});

it('follows a drag across the whole viewer and stays within it', () => {
  render(<SplitViewOverlay />);
  placeViewer();
  const divider = screen.getByRole('slider');
  fireEvent.pointerDown(divider, { button: 0, clientX: 500, pointerId: 1 });
  expect(document.activeElement).toBe(divider);
  fireEvent.pointerMove(divider, { clientX: 300, pointerId: 1 });
  expect(useEditorStore.getState().splitPosition).toBe(0.25);
  fireEvent.pointerMove(divider, { clientX: 20, pointerId: 1 });
  expect(useEditorStore.getState().splitPosition).toBe(0);
  fireEvent.pointerMove(divider, { clientX: 2000, pointerId: 1 });
  expect(useEditorStore.getState().splitPosition).toBe(1);
  fireEvent.pointerUp(divider, { pointerId: 1 });
  // Moves after the release do nothing.
  fireEvent.pointerMove(divider, { clientX: 500, pointerId: 1 });
  expect(useEditorStore.getState().splitPosition).toBe(1);
});

it('keeps the drag from reaching the viewer beneath', () => {
  const viewerDown = vi.fn();
  render(
    <div onPointerDown={viewerDown}>
      <SplitViewOverlay />
    </div>,
  );
  fireEvent.pointerDown(screen.getByRole('slider'), { button: 0, clientX: 500, pointerId: 1 });
  expect(viewerDown).not.toHaveBeenCalled();
});

it('moves with the arrow keys and returns to the middle on double-click', () => {
  render(<SplitViewOverlay />);
  const divider = screen.getByRole('slider');
  fireEvent.keyDown(divider, { key: 'ArrowRight', shiftKey: true });
  expect(useEditorStore.getState().splitPosition).toBeCloseTo(0.6);
  fireEvent.keyDown(divider, { key: 'ArrowLeft' });
  expect(useEditorStore.getState().splitPosition).toBeCloseTo(0.59);
  fireEvent.doubleClick(divider);
  expect(useEditorStore.getState().splitPosition).toBe(0.5);
});

it('names the snapshot whose before is shown on the before side', async () => {
  const { useSnapshotStore } = await import('../../../store/useSnapshotStore');
  const { useSettingsStore } = await import('../../../store/useSettingsStore');
  const { INITIAL_ADJUSTMENTS } = await import('../../../utils/adjustments');
  useSnapshotStore.setState({
    path: '/p.dng',
    snapshots: [{ id: 'a', name: 'Bright', createdAt: '2026-10-21T08:00:00Z', adjustments: INITIAL_ADJUSTMENTS }],
  });
  useEditorStore.setState({
    selectedImage: { path: '/p.dng', isReady: true, isRaw: true } as never,
    // Framed and developed as the snapshot (beforeSnapshot).
    adjustments: INITIAL_ADJUSTMENTS,
    beforeSnapshot: { path: '/p.dng', id: 'a' },
    splitBeforeShows: null,
  });
  // The native display: named once its before is in place.
  useSettingsStore.setState({ osPlatform: 'macos' });
  render(<SplitViewOverlay />);
  expect(screen.getByText('studio.workspace.viewer.splitBefore')).toBeTruthy();
  act(() => useEditorStore.setState({ splitBeforeShows: 'a' }));
  expect(screen.getByText('studio.workspace.viewer.splitBeforeSnapshot')).toBeTruthy();
  // The browser split (Linux) waits for the chosen one's frame: named at once.
  cleanup();
  useEditorStore.setState({ splitBeforeShows: null });
  useSettingsStore.setState({ osPlatform: 'linux' });
  render(<SplitViewOverlay />);
  expect(screen.getByText('studio.workspace.viewer.splitBeforeSnapshot')).toBeTruthy();
  // Another photograph shown: the unedited before again.
  cleanup();
  useEditorStore.setState({ selectedImage: { path: '/q.dng', isReady: true } as never });
  render(<SplitViewOverlay />);
  expect(screen.getByText('studio.workspace.viewer.splitBefore')).toBeTruthy();
  useSettingsStore.setState({ osPlatform: null as never });
  useEditorStore.setState({ beforeSnapshot: null, selectedImage: null });
});

it('moves the top/bottom divider vertically with pointer and keyboard', () => {
  useEditorStore.setState({ splitOrientation: 'top-bottom', splitPosition: 0.3 });
  render(<SplitViewOverlay />);
  placeViewer();
  const divider = screen.getByRole('slider');
  expect(divider.getAttribute('aria-orientation')).toBe('vertical');
  expect(divider.style.top).toBe('30%');
  fireEvent.pointerDown(divider, { button: 0, clientY: 150, pointerId: 1 });
  expect(useEditorStore.getState().splitPosition).toBe(0.25);
  fireEvent.keyDown(divider, { key: 'ArrowDown', shiftKey: true });
  expect(useEditorStore.getState().splitPosition).toBeCloseTo(0.35);
  fireEvent.keyDown(divider, { key: 'ArrowUp' });
  expect(useEditorStore.getState().splitPosition).toBeCloseTo(0.34);
  fireEvent.keyDown(divider, { key: 'ArrowRight' });
  expect(useEditorStore.getState().splitPosition).toBeCloseTo(0.34);
  fireEvent.keyDown(divider, { key: 'End' });
  expect(useEditorStore.getState().splitPosition).toBe(1);
});

it('side by side labels both panes and has no divider to drag', () => {
  useEditorStore.setState({ splitOrientation: 'side-by-side' });
  render(<SplitViewOverlay />);
  expect(screen.getByText('studio.workspace.viewer.splitBefore')).toBeTruthy();
  expect(screen.getByText('studio.workspace.viewer.splitAfter')).toBeTruthy();
  expect(screen.queryByRole('slider')).toBeNull();
  expect(screen.getByTestId('side-by-side-seam')).toBeTruthy();
});

it('names the tool left out while its reset is held, and the whole before', () => {
  const { rerender } = render(<ViewerBeforeBadge />);
  expect(screen.queryByTestId('viewer-before-badge')).toBeNull();
  act(() => useEditorStore.setState({ toolBefore: 'Exposure' }));
  rerender(<ViewerBeforeBadge />);
  expect(screen.getByText('studio.workspace.viewer.withoutTool:Exposure')).toBeTruthy();
  act(() => useEditorStore.setState({ toolBefore: null, showOriginal: true }));
  rerender(<ViewerBeforeBadge />);
  expect(screen.getByText('studio.workspace.viewer.splitBefore')).toBeTruthy();
});
