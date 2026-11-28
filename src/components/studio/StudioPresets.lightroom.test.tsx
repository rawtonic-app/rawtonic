import '../../i18n';
import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { ContextMenuProvider } from '../../context/ContextMenuContext';
import { debouncedSave, debouncedSetHistory } from '../../hooks/useEditorActions';
import { resetPresetLibrary } from '../../hooks/usePresets';
import { useEditorStore } from '../../store/useEditorStore';
import { useLibraryStore } from '../../store/useLibraryStore';
import { initialAdjustmentsFor } from '../../utils/adjustments';
import { resetBuiltinPresets } from '../../utils/builtinPresets';
import { Invokes } from '../ui/AppProperties';
import StudioPresets from './StudioPresets';
import importResponse from './fixtures/lightroom-travel-import.json?raw';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));

afterEach(() => {
  cleanup();
  debouncedSave.cancel();
  debouncedSetHistory.cancel();
});

it.each([
  { name: 'Lightroom travel', dehaze: 30, shadows: 95, whites: 100, localDehaze: undefined },
  { name: 'Lightroom mask travel', dehaze: 0, shadows: 0, whites: 0, localDehaze: 65 }
])(
  'applies $name through the import dialog to the open photo',
  async ({ name, dehaze, shadows, whites, localDehaze }) => {
    resetPresetLibrary();
    resetBuiltinPresets();
    const filePaths = ['/presets/lightroom-travel.xmp', '/presets/lightroom-mask-travel.xmp'];
    vi.mocked(open).mockResolvedValue(filePaths);
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === Invokes.HandleImportPresetsFromFiles) return JSON.parse(importResponse);
      if (command === Invokes.LoadPresets || command === Invokes.LoadBuiltinPresets) return [];
      return undefined;
    });
    const path = '/photos/import-target.dng';
    // An existing photo with the new tone travel and an older Dehaze response.
    const adjustments = { ...initialAdjustmentsFor(true), exposure: 0.5, dehazeVersion: 1 };
    useEditorStore.setState({
      selectedImage: { path, isReady: true, isRaw: true, width: 4000, height: 3000, exif: {}, thumbnailUrl: '' },
      adjustments,
      history: [adjustments],
      historyIndex: 0
    });
    useLibraryStore.setState({ multiSelectedPaths: [path] });

    render(
      <ContextMenuProvider>
        <StudioPresets />
      </ContextMenuProvider>
    );
    fireEvent.click(screen.getByRole('button', { name: 'Import presets' }));
    fireEvent.click(await screen.findByText(name));

    expect(invoke).toHaveBeenCalledWith(Invokes.HandleImportPresetsFromFiles, { filePaths });
    const applied = useEditorStore.getState().adjustments;
    expect(applied).toMatchObject({ exposure: 0.5, dehaze, dehazeVersion: 3 });
    expect(applied.shadows).toBeCloseTo(shadows, 6);
    expect(applied.whites).toBeCloseTo(whites, 6);
    if (localDehaze !== undefined) expect(applied.masks[0].adjustments.dehaze).toBe(localDehaze);
  }
);
