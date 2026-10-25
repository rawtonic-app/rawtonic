import { beforeEach, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import { useEditorStore } from '../store/useEditorStore';
import { INITIAL_ADJUSTMENTS, newNegative, type Adjustments } from './adjustments';
import { sameAnalysisInputs, sameFilm, withPickedFilmBase } from './negative';
import type { SelectedImage } from '../components/ui/AppProperties';

const recipe = (): Adjustments => ({
  ...structuredClone(INITIAL_ADJUSTMENTS),
  negative: { ...newNegative([0.7, 0.4, 0.2], [0.1, 0.05, 0.02]), enabled: false },
});

beforeEach(() => {
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    selectedImage: { path: '/s/neg.NEF', isRaw: true, isReady: true } as SelectedImage,
    filmBasePicking: true,
  });
});

it('sets a picked film base without turning the tool on', () => {
  const next = withPickedFilmBase(recipe(), '/s/neg.NEF', [0.8, 0.5, 0.3]);
  expect(next.negative).toMatchObject({ base: [0.8, 0.5, 0.3], enabled: false });
});

it('drops a pick that arrives after the picker was cancelled or the photo changed', () => {
  const prev = recipe();
  useEditorStore.setState({ filmBasePicking: false });
  expect(withPickedFilmBase(prev, '/s/neg.NEF', [0.8, 0.5, 0.3])).toBe(prev);
  useEditorStore.setState({ filmBasePicking: true });
  expect(withPickedFilmBase(prev, '/s/other.NEF', [0.8, 0.5, 0.3])).toBe(prev);
  const without = { ...prev, negative: undefined };
  expect(withPickedFilmBase(without, '/s/neg.NEF', [0.8, 0.5, 0.3])).toBe(without);
});

it('tells a reframed photo and another film apart', () => {
  const a = recipe();
  expect(sameAnalysisInputs(a, { ...a, exposure: 1 })).toBe(true);
  expect(sameAnalysisInputs(a, { ...a, crop: { x: 1, y: 2, width: 30, height: 40, unit: 'px' } })).toBe(false);
  expect(sameAnalysisInputs(a, { ...a, rotation: 2 })).toBe(false);
  expect(sameFilm(a.negative, { ...a.negative!, exposure: 0.5 })).toBe(true);
  expect(sameFilm(a.negative, { ...a.negative!, base: [0.6, 0.4, 0.2] })).toBe(false);
});
