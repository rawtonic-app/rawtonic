import '../../../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import FaceBoxOverlay from './FaceBoxOverlay';
import { LoupeFaces } from '../../studio/StudioEyes';
import { useEditorStore } from '../../../store/useEditorStore';
import { useEyesStore, type PhotoEyes } from '../../../store/useEyesStore';
import { useLibraryStore } from '../../../store/useLibraryStore';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { Invokes, type AppSettings } from '../../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
const mockInvoke = vi.mocked(invoke);

const PATH = '/job/a.arw';
const face = (index: number, state: 'open' | 'closed' | 'uncertain') => ({
  face: index,
  bbox: [0.1 + 0.3 * index, 0.2, 0.1, 0.2] as [number, number, number, number],
  state,
  ear: 0.2,
  accessories: 0,
  overrideState: null,
});
let eyes: PhotoEyes = { faces: [face(0, 'open'), face(1, 'closed'), face(2, 'uncertain')], error: null };
let shownCalls: { path: string; jsAdjustments: Record<string, unknown>; width: number; height: number; boxes: number[][] }[];

beforeEach(() => {
  eyes = { faces: [face(0, 'open'), face(1, 'closed'), face(2, 'uncertain')], error: null };
  shownCalls = [];
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command, args) => {
    if (command === Invokes.PhotoEyes) return eyes;
    if (command === Invokes.FaceEyeCrops) return (args as { boxes: unknown[] }).boxes.map(() => 'data:image/jpeg;base64,');
    if (command === Invokes.FaceBoxesShown) {
      const a = args as (typeof shownCalls)[number];
      shownCalls.push(a);
      // The backend's answer without geometry: the box in base pixels.
      return a.boxes.map(([x, y, w, h]) => [
        [x * a.width, y * a.height],
        [(x + w) * a.width, y * a.height],
        [(x + w) * a.width, (y + h) * a.height],
        [x * a.width, (y + h) * a.height],
      ]);
    }
    return null;
  });
  useLibraryStore.setState({ sessionDirectory: '/job' });
  useSettingsStore.setState({ appSettings: { eyeCheck: true } as AppSettings });
  useEyesStore.setState({ summaries: {}, generation: 0, faceMark: null });
  useEditorStore.setState({
    selectedImage: { path: PATH, width: 1000, height: 500, isReady: true } as never,
    adjustments: { ...useEditorStore.getState().adjustments, rotation: 4, orientationSteps: 0 },
  });
});
afterEach(cleanup);

/** The strip and the photograph's overlay as the Loupe shows them: the
 * photograph fitted at half size (500 × 250 CSS pixels), cropped from
 * (100, 50) of the mask frame, at zoom 2. */
function renderLoupe() {
  return render(
    <>
      <div style={{ position: 'relative' }}>
        <FaceBoxOverlay rect={{ left: 10, top: 20, width: 500, height: 250 }} cropX={100} cropY={50} scale={0.5} zoom={2} />
      </div>
      <LoupeFaces path={PATH} />
    </>,
  );
}

const crop = (n: number) => screen.getByRole('button', { name: `Show face ${n} on the photo` });
const overlay = () => screen.queryByTestId('face-box-overlay');
const polygonPoints = () => overlay()!.querySelector('.face-box-line')!.getAttribute('points');

it('shows a face box while its crop is hovered, through the geometry, never by colour alone', async () => {
  renderLoupe();
  await waitFor(() => expect(shownCalls).toHaveLength(1));
  // The recipe and base size go with the boxes; the backend applies the geometry.
  expect(shownCalls[0].path).toBe(PATH);
  expect(shownCalls[0].width).toBe(1000);
  expect(shownCalls[0].jsAdjustments.rotation).toBe(4);
  expect(shownCalls[0].boxes).toEqual(eyes.faces!.map((f) => f.bbox));
  expect(overlay()).toBeNull();

  fireEvent.mouseEnter(crop(2));
  await waitFor(() => expect(overlay()).not.toBeNull());
  // Face 2: base (400, 100)-(500, 200), minus the crop, at half size.
  expect(polygonPoints()).toBe('150,25 200,25 200,75 150,75');
  expect(overlay()!.className).toContain('is-closed');
  // Its call in words and icon, as the strip's badge.
  expect(screen.getByRole('img', { name: 'Face 2: Eyes closed or partly closed' }).querySelector('svg')).not.toBeNull();
  // Line width and label keep their size on screen at zoom 2.
  expect(overlay()!.querySelector('.face-box-line')!.getAttribute('stroke-width')).toBe('0.875');

  fireEvent.mouseLeave(crop(2));
  expect(overlay()).toBeNull();
});

it('pins a face by a click; again, Esc or another face lets go', async () => {
  const user = userEvent.setup();
  renderLoupe();
  await waitFor(() => expect(shownCalls).toHaveLength(1));
  await user.click(crop(1));
  expect(crop(1).getAttribute('aria-pressed')).toBe('true');
  await user.unhover(crop(1));
  expect(overlay()).not.toBeNull();
  expect(polygonPoints()).toBe('0,25 50,25 50,75 0,75');

  // Hover shows another face over the pinned one, leaving returns to it.
  fireEvent.mouseEnter(crop(3));
  expect(screen.getByRole('img', { name: 'Face 3: Uncertain: please check' })).toBeTruthy();
  expect(overlay()!.querySelector('.face-box-line')!.getAttribute('stroke-dasharray')).toBeTruthy();
  fireEvent.mouseLeave(crop(3));
  expect(screen.getByRole('img', { name: 'Face 1: Eyes open' })).toBeTruthy();

  // Another face moves the pin.
  await user.click(crop(2));
  await user.unhover(crop(2));
  expect(crop(1).getAttribute('aria-pressed')).toBe('false');
  expect(crop(2).getAttribute('aria-pressed')).toBe('true');
  expect(screen.getByRole('img', { name: 'Face 2: Eyes closed or partly closed' })).toBeTruthy();

  // Again: unpinned.
  await user.click(crop(2));
  await user.unhover(crop(2));
  expect(overlay()).toBeNull();

  // Esc lets go, without leaving the Loupe.
  await user.click(crop(3));
  await user.unhover(crop(3));
  const outside = vi.fn();
  window.addEventListener('keydown', outside);
  await user.keyboard('{Escape}');
  window.removeEventListener('keydown', outside);
  expect(outside).not.toHaveBeenCalled();
  expect(overlay()).toBeNull();
  expect(crop(3).getAttribute('aria-pressed')).toBe('false');
});

it('moves the box from face to face with the arrow keys, not the photograph', async () => {
  const user = userEvent.setup();
  renderLoupe();
  await waitFor(() => expect(shownCalls).toHaveLength(1));
  await user.click(crop(1));
  await user.unhover(crop(1));
  const outside = vi.fn();
  window.addEventListener('keydown', outside);
  await user.keyboard('{ArrowRight}');
  expect(document.activeElement).toBe(crop(2));
  expect(screen.getByRole('img', { name: 'Face 2: Eyes closed or partly closed' })).toBeTruthy();
  await user.keyboard('{ArrowRight}{ArrowRight}');
  expect(document.activeElement).toBe(crop(3));
  await user.keyboard('{ArrowLeft}');
  expect(crop(2).getAttribute('aria-pressed')).toBe('true');
  window.removeEventListener('keydown', outside);
  expect(outside).not.toHaveBeenCalled();
});

it('asks again when the geometry changes and drops the box with the strip', async () => {
  const view = renderLoupe();
  await waitFor(() => expect(shownCalls).toHaveLength(1));
  fireEvent.click(crop(1));
  useEditorStore.setState({ adjustments: { ...useEditorStore.getState().adjustments, rotation: -2 } });
  await waitFor(() => expect(shownCalls).toHaveLength(2));
  expect(shownCalls[1].jsAdjustments.rotation).toBe(-2);
  // The crop is not geometry: no new question.
  useEditorStore.setState({
    adjustments: { ...useEditorStore.getState().adjustments, crop: { x: 1, y: 1, width: 10, height: 10, unit: 'px' } },
  });
  view.rerender(
    <>
      <div style={{ position: 'relative' }}>
        <FaceBoxOverlay rect={{ left: 10, top: 20, width: 500, height: 250 }} cropX={100} cropY={50} scale={0.5} zoom={2} />
      </div>
    </>,
  );
  expect(shownCalls).toHaveLength(2);
  expect(useEyesStore.getState().faceMark).toBeNull();
  expect(overlay()).toBeNull();
});

it('shows nothing for another photograph than the strip’s', async () => {
  renderLoupe();
  await waitFor(() => expect(shownCalls).toHaveLength(1));
  fireEvent.click(crop(1));
  expect(overlay()).not.toBeNull();
  useEditorStore.setState({ selectedImage: { path: '/job/b.arw', width: 1000, height: 500, isReady: true } as never });
  await waitFor(() => expect(overlay()).toBeNull());
});

it('the keys take over from a pointer still resting on a crop', async () => {
  const user = userEvent.setup();
  renderLoupe();
  await waitFor(() => expect(shownCalls).toHaveLength(1));
  // Click and leave the pointer there.
  await user.click(crop(1));
  await user.keyboard('{ArrowRight}');
  expect(screen.getByRole('img', { name: 'Face 2: Eyes closed or partly closed' })).toBeTruthy();
});

it('keeps the pinned face when the faces are read again (a call, a background reading)', async () => {
  const user = userEvent.setup();
  renderLoupe();
  await waitFor(() => expect(shownCalls).toHaveLength(1));
  await user.click(crop(2));
  await user.unhover(crop(2));
  // A call on face 2 refreshes the strip's faces.
  eyes = { ...eyes, faces: eyes.faces!.map((f) => (f.face === 1 ? { ...f, overrideState: 'open' as const } : f)) };
  act(() => useEyesStore.getState().changed());
  await waitFor(() => expect(screen.getByRole('img', { name: 'Face 2: Eyes open' })).toBeTruthy());
  expect(crop(2).getAttribute('aria-pressed')).toBe('true');
});
