import { beforeEach, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import { useEditorStore } from '../store/useEditorStore';
import { INITIAL_ADJUSTMENTS, newNegative, type Adjustments } from './adjustments';
import { sameAnalysisInputs, sameFilm, setFilmBasePicking, startFilmBasePick, withPickedFilmBase } from './negative';
import type { SelectedImage } from '../components/ui/AppProperties';

const recipe = (): Adjustments => ({
  ...structuredClone(INITIAL_ADJUSTMENTS),
  negative: { ...newNegative([0.7, 0.4, 0.2], [0.1, 0.05, 0.02]), enabled: false },
});

beforeEach(() => {
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    selectedImage: { path: '/s/neg.NEF', isRaw: true, isReady: true } as SelectedImage,
  });
  setFilmBasePicking(true);
});

it('sets a picked film base without turning the tool on', () => {
  const prev = recipe();
  const next = withPickedFilmBase(prev, startFilmBasePick('/s/neg.NEF', prev), [0.8, 0.5, 0.3]);
  expect(next.negative).toMatchObject({ base: [0.8, 0.5, 0.3], enabled: false });
});

it('drops a pick after the picker was cancelled or re-armed, the photo changed or another film was set', () => {
  const prev = recipe();
  const pick = startFilmBasePick('/s/neg.NEF', prev);
  setFilmBasePicking(false);
  expect(withPickedFilmBase(prev, pick, [0.8, 0.5, 0.3])).toBe(prev);
  // Cancelled and armed again: the old sample is not this session's.
  setFilmBasePicking(true);
  expect(withPickedFilmBase(prev, pick, [0.8, 0.5, 0.3])).toBe(prev);
  const fresh = startFilmBasePick('/s/neg.NEF', prev);
  expect(withPickedFilmBase(prev, { ...fresh, path: '/s/other.NEF' }, [0.8, 0.5, 0.3])).toBe(prev);
  const without = { ...prev, negative: undefined };
  expect(withPickedFilmBase(without, fresh, [0.8, 0.5, 0.3])).toBe(without);
  // A preset set another film while sampling.
  const preset = { ...prev, negative: newNegative([0.9, 0.6, 0.3], [0.2, 0.1, 0.05]) };
  expect(withPickedFilmBase(preset, fresh, [0.8, 0.5, 0.3])).toBe(preset);
  // The lens vignetting changed while sampling.
  expect(withPickedFilmBase({ ...prev, lensVignetteAmount: 30 }, fresh, [0.8, 0.5, 0.3]).negative?.base)
    .toEqual(prev.negative?.base);
  // The colour editor's picker took over.
  useEditorStore.setState({ colorPickerTarget: 'new' });
  expect(withPickedFilmBase(prev, fresh, [0.8, 0.5, 0.3])).toBe(prev);
  useEditorStore.setState({ colorPickerTarget: null });
  expect(withPickedFilmBase(prev, fresh, [0.8, 0.5, 0.3]).negative?.base).toEqual([0.8, 0.5, 0.3]);
});

it('tells a reframed photo and another film apart', () => {
  const a = recipe();
  expect(sameAnalysisInputs(a, { ...a, exposure: 1 })).toBe(true);
  expect(sameAnalysisInputs(a, { ...a, crop: { x: 1, y: 2, width: 30, height: 40, unit: 'px' } })).toBe(false);
  expect(sameAnalysisInputs(a, { ...a, rotation: 2 })).toBe(false);
  expect(sameFilm(a.negative, { ...a.negative!, exposure: 0.5 })).toBe(true);
  expect(sameFilm(a.negative, { ...a.negative!, base: [0.6, 0.4, 0.2] })).toBe(false);
});
