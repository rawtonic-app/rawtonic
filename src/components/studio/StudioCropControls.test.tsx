import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import StudioCropControls from './StudioCropControls';
import { useEditorStore } from '../../store/useEditorStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import { debouncedSetHistory } from '../../hooks/useEditorActions';
import { INITIAL_ADJUSTMENTS } from '../../utils/adjustments';
import { useGeometryOffers } from '../../utils/sourceGeometry';
import { noteHandDrawnCrop } from '../../utils/cropReferenceActions';
import { useLibraryStore } from '../../store/useLibraryStore';
import { invoke } from '@tauri-apps/api/core';
import type { AppSettings, SelectedImage } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));

beforeEach(() => {
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    selectedImage: { path: '/s/a.NEF', width: 6000, height: 4500, isRaw: true, isReady: true } as SelectedImage
  });
  useSettingsStore.setState({
    appSettings: { customAspectRatios: [] } as unknown as AppSettings,
    handleSettingsChange: vi.fn((settings: AppSettings) => {
      useSettingsStore.setState({ appSettings: settings });
      return Promise.resolve();
    })
  } as never);
});
afterEach(() => {
  cleanup();
  debouncedSetHistory.cancel();
});

const adjustments = () => useEditorStore.getState().adjustments;

describe('crop controls (Lightroom and Capture One)', () => {
  it('flips the photo horizontally and vertically', () => {
    render(<StudioCropControls />);
    fireEvent.click(screen.getByRole('button', { name: 'Flip horizontal' }));
    fireEvent.click(screen.getByRole('button', { name: 'Flip vertical' }));
    expect(adjustments().flipHorizontal).toBe(true);
    expect(adjustments().flipVertical).toBe(true);
  });

  it("offers the photo's original ratio and the shared print and screen ratios", () => {
    render(<StudioCropControls />);
    const select = screen.getByRole('combobox', { name: 'Crop aspect ratio' });
    const names = [...select.querySelectorAll('option')].map((o) => o.textContent);
    expect(names).toEqual(expect.arrayContaining(['Original', '1 × 1', '7 × 5', '11 × 8.5', '16 × 10', '65 × 24']));
    const original = [...select.querySelectorAll('option')].find((o) => o.textContent === 'Original')!;
    fireEvent.change(select, { target: { value: original.value } });
    expect(adjustments().aspectRatio).toBeCloseTo(6000 / 4500);
  });

  it("keeps a ratio of the user's own and applies it", () => {
    render(<StudioCropControls />);
    fireEvent.click(screen.getByRole('button', { name: 'Add a crop ratio' }));
    fireEvent.change(screen.getByRole('textbox', { name: 'Ratio width' }), { target: { value: '6' } });
    fireEvent.change(screen.getByRole('textbox', { name: 'Ratio height' }), { target: { value: '17' } });
    fireEvent.click(screen.getByRole('button', { name: 'Add' }));
    expect(useSettingsStore.getState().appSettings?.customAspectRatios).toEqual([{ width: 6, height: 17 }]);
    expect(adjustments().aspectRatio).toBeCloseTo(17 / 6);
  });

  it('chooses and turns the composition overlay', () => {
    render(<StudioCropControls />);
    fireEvent.change(screen.getByRole('combobox', { name: 'Crop overlay' }), { target: { value: 'goldenSpiral' } });
    expect(useEditorStore.getState().overlayMode).toBe('goldenSpiral');
    fireEvent.click(screen.getByRole('button', { name: 'Turn the overlay' }));
    expect(useEditorStore.getState().overlayRotation).toBe(1);
  });
});

it('returns focus to the photograph after choosing a crop ratio', () => {
  const { container } = render(
    <>
      <div className="studio-viewer" tabIndex={0} />
      <StudioCropControls />
    </>
  );
  const select = screen.getByRole('combobox', { name: 'Crop aspect ratio' });
  select.focus();
  fireEvent.change(select, { target: { value: '1' } });
  expect(document.activeElement).toBe(container.querySelector('.studio-viewer'));
});

it('offers Capture One straightening and crop only for a photo the import named, and applies it on a click', async () => {
  useLibraryStore.setState({ sessionDirectory: '/jobs/Acme' });
  useGeometryOffers.setState({ offers: [] });
  const { rerender } = render(<StudioCropControls />);
  expect(screen.queryByRole('button', { name: 'Apply Capture One straightening and crop' })).toBeNull();
  useGeometryOffers.setState({
    offers: [
      {
        directory: '/jobs/Acme',
        source: '/C1/Trip.cosessiondb',
        relinks: [],
        photos: [{ path: '/s/a.NEF', rotation: 5, orientationSteps: 0 }]
      }
    ]
  });
  rerender(<StudioCropControls />);
  const button = await screen.findByRole('button', { name: 'Apply Capture One straightening and crop' });
  // Not applied by itself.
  expect(invoke).not.toHaveBeenCalledWith('library_apply_source_geometry', expect.anything());
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === 'library_apply_source_geometry' ? { recipeChanges: [], unchanged: ['/s/a.NEF'], errors: [] } : undefined
  );
  // The angle the report showed is on the entry.
  expect(screen.getByLabelText('5°')).toBeTruthy();
  fireEvent.click(button);
  await vi.waitFor(() =>
    expect(invoke).toHaveBeenCalledWith('library_apply_source_geometry', {
      source: '/C1/Trip.cosessiondb',
      directory: '/jobs/Acme',
      relinks: [],
      offers: [{ path: '/s/a.NEF', rotation: 5, orientationSteps: 0 }]
    })
  );
  await vi.waitFor(() => expect(useGeometryOffers.getState().offers).toEqual([]));
});

describe('changing the crop ratio (ASP1: Capture One and Lightroom re-fit from the photo)', () => {
  // The C1R12 frame: a D800 7360 × 4912.
  beforeEach(() => {
    useEditorStore.setState({
      selectedImage: { path: '/s/d800.NEF', width: 7360, height: 4912, isRaw: true, isReady: true } as SelectedImage
    });
  });
  const choose = (value: number) =>
    fireEvent.change(screen.getByRole('combobox', { name: 'Crop aspect ratio' }), { target: { value: String(value) } });
  const size = () => {
    const c = adjustments().crop!;
    return [c.width, c.height];
  };

  it('does not depend on the ratios chosen in between: through a square it grows back', () => {
    render(<StudioCropControls />);
    choose(16 / 9);
    expect(size()).toEqual([7360, 4140]);
    choose(1);
    expect(size()).toEqual([4912, 4912]);
    choose(5 / 4);
    expect(size()).toEqual([6140, 4912]);
    choose(1);
    choose(3 / 2);
    expect(size()).toEqual([7360, 4907]);
    choose(1);
    choose(4 / 3);
    expect(size()).toEqual([6549, 4912]);
    choose(1);
    choose(16 / 9);
    expect(adjustments().crop).toEqual({ unit: 'px', x: 0, y: 386, width: 7360, height: 4140 });
  });

  it('keeps the centre of the crop it started from', () => {
    useEditorStore.setState({
      adjustments: { ...adjustments(), crop: { unit: 'px', x: 0, y: 0, width: 3000, height: 2000 } }
    });
    render(<StudioCropControls />);
    choose(1);
    // The largest square, as near the crop's centre (1500, 1000) as the photo allows.
    expect(adjustments().crop).toEqual({ unit: 'px', x: 0, y: 0, width: 4912, height: 4912 });
    choose(16 / 9);
    expect(adjustments().crop).toEqual({ unit: 'px', x: 0, y: 0, width: 7360, height: 4140 });
  });

  it('fits every ratio inside the crop the user drew, until they draw another', () => {
    render(<StudioCropControls />);
    choose(16 / 9);
    // The user draws a free 3000 × 2000 crop by hand.
    const drawn = { unit: 'px' as const, x: 1000, y: 1000, width: 3000, height: 2000 };
    noteHandDrawnCrop(adjustments().crop, drawn);
    useEditorStore.setState({ adjustments: { ...adjustments(), aspectRatio: null, crop: drawn } });
    choose(1);
    expect(adjustments().crop).toEqual({ unit: 'px', x: 1500, y: 1000, width: 2000, height: 2000 });
    choose(16 / 9);
    expect(adjustments().crop).toEqual({ unit: 'px', x: 1000, y: 1156, width: 3000, height: 1688 });
    choose(3 / 2);
    expect(adjustments().crop).toEqual(drawn);
    // Moving the crop by hand moves the reference with it.
    const moved = { ...drawn, x: 1200, y: 900 };
    noteHandDrawnCrop(adjustments().crop, moved);
    useEditorStore.setState({ adjustments: { ...adjustments(), crop: moved } });
    choose(1);
    expect(adjustments().crop).toEqual({ unit: 'px', x: 1700, y: 900, width: 2000, height: 2000 });
    choose(3 / 2);
    expect(adjustments().crop).toEqual(moved);
  });

  it('forgets the drawn crop on another photograph', () => {
    render(<StudioCropControls />);
    const drawn = { unit: 'px' as const, x: 1000, y: 1000, width: 3000, height: 2000 };
    noteHandDrawnCrop(null, drawn);
    useEditorStore.setState({
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), crop: null },
      selectedImage: { path: '/s/other.NEF', width: 7360, height: 4912, isRaw: true, isReady: true } as SelectedImage
    });
    choose(1);
    expect(adjustments().crop).toEqual({ unit: 'px', x: 1224, y: 0, width: 4912, height: 4912 });
  });
});

it('a crop reset forgets the drawn crop (ASP1)', () => {
  useEditorStore.setState({
    selectedImage: { path: '/s/d800.NEF', width: 7360, height: 4912, isRaw: true, isReady: true } as SelectedImage
  });
  render(<StudioCropControls />);
  const drawn = { unit: 'px' as const, x: 1000, y: 1000, width: 3000, height: 2000 };
  noteHandDrawnCrop(null, drawn);
  useEditorStore.setState({ adjustments: { ...adjustments(), crop: drawn } });
  fireEvent.click(screen.getByRole('button', { name: 'Reset' }));
  // The crop tool fits the whole frame to an uncropped photo.
  useEditorStore.setState({
    adjustments: { ...adjustments(), crop: { unit: 'px', x: 0, y: 0, width: 7360, height: 4912 } }
  });
  fireEvent.change(screen.getByRole('combobox', { name: 'Crop aspect ratio' }), { target: { value: '1' } });
  expect(adjustments().crop).toEqual({ unit: 'px', x: 1224, y: 0, width: 4912, height: 4912 });
});

it('undoing an enlarged hand-drawn crop fits the next ratio into the restored crop (ASP1)', () => {
  useEditorStore.setState({
    selectedImage: { path: '/s/d800.NEF', width: 7360, height: 4912, isRaw: true, isReady: true } as SelectedImage
  });
  render(<StudioCropControls />);
  const small = { unit: 'px' as const, x: 1000, y: 1000, width: 1200, height: 800 };
  const big = { unit: 'px' as const, x: 1000, y: 1000, width: 3000, height: 2000 };
  const a0 = { ...adjustments(), crop: small };
  const a1 = { ...adjustments(), crop: big };
  noteHandDrawnCrop(small, big);
  useEditorStore.setState({ history: [a0, a1], historyIndex: 1, adjustments: a1 });
  useEditorStore.getState().undo();
  fireEvent.change(screen.getByRole('combobox', { name: 'Crop aspect ratio' }), { target: { value: '1' } });
  expect(adjustments().crop).toEqual({ unit: 'px', x: 1200, y: 1000, width: 800, height: 800 });
});
