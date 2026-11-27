import '../../i18n';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import AdjustmentControl, { PEEK_HOLD_MS } from './AdjustmentControl';
import { updateAdjustments } from '../../hooks/useEditorActions';
import { useEditorStore } from '../../store/useEditorStore';
import { INITIAL_ADJUSTMENTS } from '../../utils/adjustments';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => new Promise(() => {})) }));

const setClarity = (clarity: number) => updateAdjustments((prev) => ({ ...prev, clarity }));

beforeEach(() => {
  vi.useFakeTimers();
  const adjustments = { ...INITIAL_ADJUSTMENTS, clarity: 25 };
  useEditorStore.setState({
    selectedImage: { path: '/a.dng', isReady: true, width: 10, height: 10 } as never,
    adjustments,
    history: [adjustments],
    historyIndex: 0,
    showOriginal: false,
    toolBefore: null,
    previewOverride: null,
    isSliderDragging: false
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const renderClarity = (props: Partial<Parameters<typeof AdjustmentControl>[0]> = {}) => {
  const onChange = vi.fn(setClarity);
  render(
    <AdjustmentControl
      label="Clarity"
      value={useEditorStore.getState().adjustments.clarity as number}
      onChange={onChange}
      {...props}
    />
  );
  const label = screen.getByText('Clarity');
  // jsdom has no pointer capture.
  label.setPointerCapture = vi.fn();
  return { label, onChange };
};

describe('holding a slider label (UI-PEEK)', () => {
  it('shows the photo at the default while held and restores it exactly on release, without a history step', () => {
    const before = useEditorStore.getState().adjustments;
    const { label } = renderClarity();
    fireEvent.pointerDown(label, { button: 0, clientX: 10, pointerId: 1 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS - 1));
    expect(useEditorStore.getState().previewOverride).toBeNull();
    act(() => vi.advanceTimersByTime(1));
    const held = useEditorStore.getState();
    expect(held.previewOverride).toMatchObject({ clarity: 0 });
    expect(held.adjustments).toBe(before);
    expect(label.closest('.adjustment-control')!.classList.contains('is-peeking')).toBe(true);
    // A small wobble while held does not start a scrub.
    fireEvent.pointerMove(label, { clientX: 40, pointerId: 1 });
    expect(useEditorStore.getState().previewOverride).toMatchObject({ clarity: 0 });
    fireEvent.pointerUp(label, { pointerId: 1 });
    act(() => vi.advanceTimersByTime(2000));
    const after = useEditorStore.getState();
    expect(after.previewOverride).toBeNull();
    expect(after.toolBefore).toBeNull();
    expect(after.adjustments).toBe(before);
    expect(after.history).toHaveLength(1);
    expect(after.historyIndex).toBe(0);
  });

  it('a short press shows nothing; a double-click still resets', () => {
    const { label, onChange } = renderClarity();
    fireEvent.pointerDown(label, { button: 0, clientX: 10, pointerId: 1 });
    fireEvent.pointerUp(label, { pointerId: 1 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS * 2));
    expect(useEditorStore.getState().previewOverride).toBeNull();
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.doubleClick(label);
    expect(onChange).toHaveBeenLastCalledWith(0);
    expect(useEditorStore.getState().adjustments.clarity).toBe(0);
  });

  it('moving before the hold scrubs instead', () => {
    const { label, onChange } = renderClarity();
    fireEvent.pointerDown(label, { button: 0, clientX: 10, pointerId: 1 });
    fireEvent.pointerMove(label, { clientX: 15, pointerId: 1 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS * 2));
    expect(useEditorStore.getState().previewOverride).toBeNull();
    expect(onChange).toHaveBeenLastCalledWith(30);
    fireEvent.pointerUp(label, { pointerId: 1 });
  });

  it('a slider at its default, or one that sets a tool, shows no peek', () => {
    useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, clarity: 0 } });
    const { label } = renderClarity({ value: 0 });
    fireEvent.pointerDown(label, { button: 0, clientX: 10, pointerId: 1 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS));
    expect(useEditorStore.getState().previewOverride).toBeNull();
    fireEvent.pointerUp(label, { pointerId: 1 });
    cleanup();
    const brush = vi.fn();
    render(<AdjustmentControl label="Size" value={40} min={1} max={100} peek={false} onChange={brush} />);
    const size = screen.getByText('Size');
    size.setPointerCapture = vi.fn();
    fireEvent.pointerDown(size, { button: 0, clientX: 10, pointerId: 1 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS));
    expect(brush).not.toHaveBeenCalled();
    expect(useEditorStore.getState().previewOverride).toBeNull();
  });

  it('a focus loss ends the peek', () => {
    const { label } = renderClarity();
    fireEvent.pointerDown(label, { button: 0, clientX: 10, pointerId: 1 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS));
    expect(useEditorStore.getState().previewOverride).not.toBeNull();
    act(() => void window.dispatchEvent(new Event('blur')));
    expect(useEditorStore.getState()).toMatchObject({ previewOverride: null, toolBefore: null });
  });

  it('marks the default on the track', () => {
    renderClarity({ min: -100, max: 100 });
    const track = screen.getByRole('slider', { name: 'Clarity' });
    expect(track.classList.contains('has-default-tick')).toBe(true);
    expect(track.style.getPropertyValue('--default-at')).toBe('0.5');
    cleanup();
    render(<AdjustmentControl label="Amount" value={10} min={0} max={100} defaultValue={25} onChange={vi.fn()} />);
    expect(screen.getByRole('slider', { name: 'Amount' }).style.getPropertyValue('--default-at')).toBe('0.25');
  });
});
