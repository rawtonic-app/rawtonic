import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import StudioCropControls from './StudioCropControls';
import { useEditorStore } from '../../store/useEditorStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import { debouncedSetHistory } from '../../hooks/useEditorActions';
import { INITIAL_ADJUSTMENTS } from '../../utils/adjustments';
import { useGeometryOffers } from '../../utils/sourceGeometry';
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
    offers: [{ directory: '/jobs/Acme', source: '/C1/Trip.cosessiondb', relinks: [], paths: ['/s/a.NEF'] }]
  });
  rerender(<StudioCropControls />);
  const button = await screen.findByRole('button', { name: 'Apply Capture One straightening and crop' });
  // Not applied by itself.
  expect(invoke).not.toHaveBeenCalledWith('library_apply_source_geometry', expect.anything());
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === 'library_apply_source_geometry' ? { recipeChanges: [], unchanged: ['/s/a.NEF'], errors: [] } : undefined
  );
  fireEvent.click(button);
  await vi.waitFor(() =>
    expect(invoke).toHaveBeenCalledWith('library_apply_source_geometry', {
      source: '/C1/Trip.cosessiondb',
      directory: '/jobs/Acme',
      relinks: [],
      paths: ['/s/a.NEF']
    })
  );
  await vi.waitFor(() => expect(useGeometryOffers.getState().offers).toEqual([]));
});
