import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it } from 'vitest';
import { useCompareSession } from './useCompareSession';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';

beforeEach(() => {
  localStorage.clear();
  useLibraryStore.setState({ sessionDirectory: '/one' });
});
afterEach(cleanup);
it('remembers the enabled mode and divider separately for each session across remounts', () => {
  const { unmount } = renderHook(useCompareSession);
  act(() => useEditorStore.getState().setEditor({ splitView: true, splitOrientation: 'top-bottom', splitPosition: 0.3, zoom: 3 }));
  act(() => useLibraryStore.setState({ sessionDirectory: '/two' }));
  expect(useEditorStore.getState()).toMatchObject({ splitView: false, splitOrientation: 'left-right', splitPosition: 0.5, zoom: 3 });
  act(() => useEditorStore.getState().setEditor({ splitView: true, splitPosition: 0.8 }));
  act(() => useLibraryStore.setState({ sessionDirectory: '/one' }));
  expect(useEditorStore.getState()).toMatchObject({ splitView: true, splitOrientation: 'top-bottom', splitPosition: 0.3, zoom: 3 });
  unmount();
  useEditorStore.setState({ splitView: false });
  renderHook(useCompareSession);
  expect(useEditorStore.getState()).toMatchObject({ splitView: true, splitOrientation: 'top-bottom', splitPosition: 0.3 });
});
it('validates stored preferences and ignores unrelated editor changes', () => {
  localStorage.setItem('rawtonic.compare:/one', JSON.stringify({ enabled: 'yes', orientation: 'unknown', position: 50 }));
  renderHook(useCompareSession);
  expect(useEditorStore.getState()).toMatchObject({ splitView: false, splitOrientation: 'left-right', splitPosition: 1 });
  act(() => useEditorStore.getState().setEditor({ zoom: 4 }));
  expect(JSON.parse(localStorage.getItem('rawtonic.compare:/one')!)).toEqual({ enabled: 'yes', orientation: 'unknown', position: 50 });
});
it('opens a session with corrupt storage using safe defaults', () => {
  localStorage.setItem('rawtonic.compare:/one', 'null');
  renderHook(useCompareSession);
  expect(useEditorStore.getState()).toMatchObject({ splitView: false, splitOrientation: 'left-right', splitPosition: 0.5 });
});
