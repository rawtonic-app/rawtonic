import '../../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import StudioLandscapeMask from './StudioLandscapeMask';
import { useEditorStore } from '../../store/useEditorStore';
import type { SubMask } from '../panel/right/Masks';
import { createComponents, kindOf, type LandscapePart } from '../../utils/maskKinds';
import { LANDSCAPE_VERSION } from '../../utils/maskUtils';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async () => [
    ['sky', 0.4],
    ['mountains', 0.3],
    ['water', 0.2],
    ['vegetation', 0.001],
    ['naturalGround', 0],
    ['artificialGround', 0],
    ['architecture', 0]
  ])
}));
beforeEach(() => {
  vi.mocked(invoke).mockClear();
  useEditorStore.setState({ selectedImage: null });
});
afterEach(cleanup);

const checkbox = (name: RegExp) =>
  screen.getByLabelText<HTMLInputElement>(name, { selector: 'input[type="checkbox"]' });

function sub(parts: LandscapePart[]) {
  return {
    id: 'l',
    type: 'ai-landscape',
    visible: true,
    mode: 'additive',
    parameters: { parts, maskDataBase64: 'data:old', grow: 0, feather: 0 }
  } as unknown as SubMask;
}

it('a new landscape component selects nothing until parts are chosen', () => {
  const [component] = createComponents('landscape', { width: 100, height: 80 });
  expect(component.type).toBe('ai-landscape');
  expect(component.parameters).toMatchObject({ parts: [], maskDataBase64: null, landscapeVersion: LANDSCAPE_VERSION });
  expect(LANDSCAPE_VERSION).toBe(2);
  expect(kindOf(component)).toBe('landscape');
});

it('ticking parts keeps their list order and clears the stale mask', () => {
  const setParameters = vi.fn();
  render(<StudioLandscapeMask sub={sub(['water'])} setParameters={setParameters} onInteraction={() => {}} />);
  fireEvent.click(checkbox(/^Mountains/));
  expect(setParameters).toHaveBeenLastCalledWith({ parts: ['mountains', 'water'], maskDataBase64: null });
  fireEvent.click(checkbox(/^Water/));
  expect(setParameters).toHaveBeenLastCalledWith({ parts: [], maskDataBase64: null });
});

it('marks the parts the photo does not have, but keeps them selectable', async () => {
  useEditorStore.setState({ selectedImage: { path: '/photo.raf' } as never });
  await act(async () => {
    render(<StudioLandscapeMask sub={sub([])} setParameters={() => {}} onInteraction={() => {}} />);
  });
  expect(screen.getByText('Choose the parts to select.')).toBeTruthy();
  const label = (name: RegExp) => checkbox(name).closest('label');
  expect(label(/^Vegetation/)?.textContent).toContain('(not found)');
  expect(checkbox(/^Vegetation/).disabled).toBe(false);
  expect(label(/^Mountains/)?.textContent).toBe('Mountains');
  expect(vi.mocked(invoke)).toHaveBeenCalledWith(
    'detect_landscape_parts',
    expect.objectContaining({ path: '/photo.raf' })
  );
});

it('asks again when a part is ticked after the detection failed (offline on first use)', async () => {
  useEditorStore.setState({ selectedImage: { path: '/photo.raf' } as never });
  vi.mocked(invoke).mockRejectedValueOnce({ code: 'internal', message: 'offline' });
  await act(async () => {
    render(<StudioLandscapeMask sub={sub([])} setParameters={() => {}} onInteraction={() => {}} />);
  });
  expect(screen.getByRole('alert')).toBeTruthy();
  const calls = vi.mocked(invoke).mock.calls.length;
  await act(async () => fireEvent.click(checkbox(/^Mountains/)));
  expect(screen.getByText('Choose the parts to select.')).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.length).toBe(calls + 1);
});
