import { act, renderHook } from '@testing-library/react';
import { vi } from 'vitest';
import { useSettingsStore } from '../../store/useSettingsStore';
import type { AppSettings } from '../ui/AppProperties';
import { useIngestDialogSize } from './useIngestDialogSize';

import { describe, expect, it } from 'vitest';
import { clampIngestSize } from './useIngestDialogSize';

describe('import dialog dimensions', () => {
  it('defaults to ninety percent up to 1600 by 1000', () => {
    expect(clampIngestSize(undefined, { width: 2000, height: 1400 })).toEqual({
      width: 1600,
      height: 1000,
      maximized: false
    });
  });
  it('clamps to minimum and available viewport, including small windows', () => {
    expect(clampIngestSize({ width: 20, height: 9999, maximized: false }, { width: 1200, height: 800 })).toEqual({
      width: 760,
      height: 776,
      maximized: false
    });
    expect(clampIngestSize(undefined, { width: 600, height: 400 })).toEqual({
      width: 576,
      height: 376,
      maximized: false
    });
  });
  it('retains restored dimensions and maximized state from serialized preferences', () => {
    const saved = { width: 1100, height: 700, maximized: true };
    expect(clampIngestSize(JSON.parse(JSON.stringify(saved)), { width: 1400, height: 1000 })).toEqual(saved);
  });
});

it('maximizes, restores and reads persisted dimensions on reopening', () => {
  const original = useSettingsStore.getState();
  const save = vi.fn(async (appSettings: AppSettings) => {
    useSettingsStore.setState({ appSettings });
  });
  useSettingsStore.setState({
    appSettings: { ingestDialogSize: { width: 800, height: 550, maximized: false } } as AppSettings,
    handleSettingsChange: save
  });
  const first = renderHook(useIngestDialogSize);
  act(() => first.result.current.toggle());
  expect(first.result.current.size.maximized).toBe(true);
  expect(save).toHaveBeenCalledOnce();
  first.unmount();
  const reopened = renderHook(useIngestDialogSize);
  expect(reopened.result.current.size.maximized).toBe(true);
  act(() => reopened.result.current.toggle());
  expect(reopened.result.current.size).toEqual({ width: 800, height: 550, maximized: false });
  reopened.unmount();
  useSettingsStore.setState(original);
});

it('resizes symmetrically, clamps and saves once on release', () => {
  const original = useSettingsStore.getState();
  const save = vi.fn(async () => {});
  useSettingsStore.setState({
    appSettings: { ingestDialogSize: { width: 800, height: 550, maximized: false } } as AppSettings,
    handleSettingsChange: save
  });
  const hook = renderHook(useIngestDialogSize);
  const element = document.createElement('div');
  element.setPointerCapture = vi.fn();
  act(() =>
    hook.result.current.start(
      {
        button: 0,
        clientX: 0,
        clientY: 0,
        pointerId: 1,
        preventDefault() {},
        currentTarget: element
      } as unknown as import('react').PointerEvent<HTMLElement>,
      'se'
    )
  );
  const move = new Event('pointermove');
  Object.assign(move, { clientX: 10, clientY: 10 });
  act(() => element.dispatchEvent(move));
  expect(hook.result.current.size.width).toBe(820);
  expect(hook.result.current.size.height).toBe(570);
  expect(save).not.toHaveBeenCalled();
  act(() => element.dispatchEvent(new Event('pointerup')));
  expect(save).toHaveBeenCalledOnce();
  hook.unmount();
  useSettingsStore.setState(original);
});
