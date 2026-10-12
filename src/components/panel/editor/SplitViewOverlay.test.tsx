import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeAll, beforeEach, expect, it, vi } from 'vitest';
import SplitViewOverlay from './SplitViewOverlay';
import { useEditorStore } from '../../../store/useEditorStore';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key: string) => key }) }));

beforeAll(() => {
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.hasPointerCapture = () => true;
  Element.prototype.releasePointerCapture = () => {};
});
beforeEach(() => useEditorStore.setState({ splitView: true, splitPosition: 0.5, showOriginal: false }));
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
