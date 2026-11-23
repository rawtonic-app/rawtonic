import '../../i18n';
import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import IngestMetadata, { type ImportMetadata } from './IngestMetadata';
import { useSettingsStore } from '../../store/useSettingsStore';
import type { AppSettings } from '../ui/AppProperties';

import { invoke } from '@tauri-apps/api/core';
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => {}) }));
const save = vi.mocked(invoke);
const change = (label: string, value: string) => fireEvent.change(screen.getByLabelText(label), { target: { value } });
const clickPresetButton = (name: string) => act(async () => fireEvent.click(screen.getByRole('button', { name })));

beforeEach(() => {
  save.mockClear();
  useSettingsStore.setState({ appSettings: {} as AppSettings });
});
afterEach(cleanup);
function Form() {
  const [value, setValue] = useState<ImportMetadata | null>(null);
  return (
    <>
      <IngestMetadata value={value} onChange={setValue} />
      <output data-testid="value">{JSON.stringify(value)}</output>
    </>
  );
}
it('starts at None and creates, edits, selects and deletes saved presets', async () => {
  render(<Form />);
  expect((screen.getByLabelText('Metadata preset') as HTMLSelectElement).value).toBe('');
  expect(screen.getByTestId('value').textContent).toBe('null');
  change('Metadata preset', 'new');
  expect((screen.getByLabelText('Include creator contact details') as HTMLInputElement).checked).toBe(false);
  expect(screen.queryByLabelText('Email')).toBeNull();
  change('Preset name', 'My rights');
  change('Creator', 'Photographer');
  change('Copyright notice', 'Copyright {year}');
  change('Keywords (comma separated)', 'harbour, sea, harbour');
  await clickPresetButton('Save preset');
  expect(save).toHaveBeenCalledOnce();
  expect(useSettingsStore.getState().appSettings?.importMetadataPresets).toEqual([
    expect.objectContaining({
      name: 'My rights',
      creator: 'Photographer',
      copyright: 'Copyright {year}',
      includeCreatorContact: false,
      overwrite: [],
      keywords: ['harbour', 'sea']
    })
  ]);
  fireEvent.click(screen.getByLabelText('Overwrite Creator'));
  fireEvent.click(screen.getByLabelText('Include creator contact details'));
  change('Email', 'creator@example.org');
  await clickPresetButton('Save preset');
  expect(useSettingsStore.getState().appSettings?.importMetadataPresets).toHaveLength(1);
  change('Metadata preset', '');
  expect(screen.getByTestId('value').textContent).toBe('null');
  const preset = useSettingsStore.getState().appSettings!.importMetadataPresets![0];
  change('Metadata preset', preset.id);
  expect((screen.getByLabelText('Creator') as HTMLInputElement).value).toBe('Photographer');
  expect((screen.getByLabelText('Overwrite Creator') as HTMLInputElement).checked).toBe(true);
  expect((screen.getByLabelText('Email') as HTMLInputElement).value).toBe('creator@example.org');
  await clickPresetButton('Delete preset');
  expect(useSettingsStore.getState().appSettings?.importMetadataPresets).toEqual([]);
  expect(screen.getByTestId('value').textContent).toBe('null');
});

it('reports a settings-write failure and leaves saved presets unchanged', async () => {
  render(<Form />);
  change('Metadata preset', 'new');
  change('Preset name', 'Rights');
  save.mockRejectedValueOnce(new Error('Disk full'));
  await clickPresetButton('Save preset');
  expect(screen.getByRole('alert').textContent).toContain('Disk full');
  expect(useSettingsStore.getState().appSettings?.importMetadataPresets).toBeUndefined();
});

it('keeps a saved preset in its picker position when edited', async () => {
  const preset = {
    creator: '',
    copyright: '',
    description: '',
    iptc: {},
    keywords: [],
    overwrite: [],
    includeCreatorContact: false
  };
  useSettingsStore.setState({
    appSettings: {
      ...useSettingsStore.getState().appSettings!,
      importMetadataPresets: [
        { ...preset, id: 'first', name: 'First' },
        { ...preset, id: 'second', name: 'Second' }
      ]
    }
  });
  render(<Form />);
  change('Metadata preset', 'first');
  change('Creator', 'Creator');
  await clickPresetButton('Save preset');
  expect(useSettingsStore.getState().appSettings?.importMetadataPresets).toEqual([
    expect.objectContaining({ id: 'first', creator: 'Creator' }),
    expect.objectContaining({ id: 'second', creator: '' })
  ]);
});
