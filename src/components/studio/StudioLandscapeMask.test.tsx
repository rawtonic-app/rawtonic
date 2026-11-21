import '../../i18n';
import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import StudioLandscapeMask from './StudioLandscapeMask';
import { useEditorStore } from '../../store/useEditorStore';
import type { SubMask } from '../panel/right/Masks';
import { createComponents, kindOf, type LandscapePart } from '../../utils/maskKinds';

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
afterEach(() => {
  cleanup();
  useEditorStore.setState({ selectedImage: null });
});

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
  expect(component.parameters).toMatchObject({ parts: [], maskDataBase64: null });
  expect(kindOf(component)).toBe('landscape');
});

it('ticking parts keeps their list order and clears the stale mask', () => {
  const setParameters = vi.fn();
  render(<StudioLandscapeMask sub={sub(['water'])} setParameters={setParameters} onInteraction={() => {}} />);
  fireEvent.click(screen.getByRole('checkbox', { name: /^Mountains/ }));
  expect(setParameters).toHaveBeenLastCalledWith({ parts: ['mountains', 'water'], maskDataBase64: null });
  fireEvent.click(screen.getByRole('checkbox', { name: /^Water/ }));
  expect(setParameters).toHaveBeenLastCalledWith({ parts: [], maskDataBase64: null });
});

it('marks the parts the photo does not have, but keeps them selectable', async () => {
  useEditorStore.setState({ selectedImage: { path: '/photo.raf' } as never });
  render(<StudioLandscapeMask sub={sub([])} setParameters={() => {}} onInteraction={() => {}} />);
  await screen.findByText('Choose the parts to select.');
  const label = (name: RegExp) => screen.getByRole('checkbox', { name }).closest('label');
  expect(label(/^Vegetation/)?.textContent).toContain('(not found)');
  expect((screen.getByRole('checkbox', { name: /^Vegetation/ }) as HTMLInputElement).disabled).toBe(false);
  expect(label(/^Mountains/)?.textContent).toBe('Mountains');
  expect(vi.mocked(invoke)).toHaveBeenCalledWith(
    'detect_landscape_parts',
    expect.objectContaining({ path: '/photo.raf' })
  );
});
