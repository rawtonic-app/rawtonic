import { act } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';
import { adoptQueuedSelections } from './queuedSelections';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const PATH = '/library/sky.nef';
const SKY = 'data:image/png;base64,SKY';

const withSky = (recipe: Adjustments, parameters: Record<string, unknown>): Adjustments => ({
  ...recipe,
  masks: [
    {
      id: 'sky-layer',
      visible: true,
      subMasks: [{ id: 'sky-1', type: 'ai-sky', visible: true, parameters }]
    }
  ] as never
});
const pending = { maskDataBase64: null, detectPending: true, grow: 0, feather: 0 };
const computed = { maskDataBase64: SKY, grow: 0, feather: 0, rotation: 0 };
const skyOf = (recipe: Adjustments) => recipe.masks[0].subMasks[0].parameters as Record<string, unknown>;

/** The backend's answer: the waiting Sky component filled, with the
 * component's own settings as sent (`adopt_selections`). */
function answerWithSky() {
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command !== Invokes.AdoptSelections) return null;
    const { recipes } = args as { recipes: Adjustments[] };
    return recipes.map((recipe) =>
      skyOf(recipe).maskDataBase64 ? null : { 'sky-1': { ...skyOf(recipe), ...computed, detectPending: undefined } }
    );
  });
}

function open(adjustments: Adjustments, history: Adjustments[] = [adjustments]) {
  useEditorStore.setState({
    selectedImage: { path: PATH, isReady: true, isRaw: true, width: 4000, height: 3000, exif: {}, thumbnailUrl: '' },
    adjustments,
    history,
    historyIndex: history.length - 1,
    isSliderDragging: false
  } as never);
}

beforeEach(() => vi.mocked(invoke).mockReset());
afterEach(() => useEditorStore.setState({ selectedImage: null } as never));

describe('the open photo takes the selections the queue computed', () => {
  it('fills the waiting component and keeps an unsaved slider change', async () => {
    answerWithSky();
    const before = { ...INITIAL_ADJUSTMENTS, exposure: 0 } as Adjustments;
    const preset = withSky(before, pending);
    // Exposure moved after the preset, not saved yet.
    const edited = { ...preset, exposure: 1.4 };
    open(edited, [before, preset, edited]);
    await act(async () => {
      expect(await adoptQueuedSelections(PATH)).toBe(true);
    });
    const { adjustments, history, historyIndex } = useEditorStore.getState();
    expect(adjustments.exposure).toBe(1.4);
    expect(skyOf(adjustments).maskDataBase64).toBe(SKY);
    expect(skyOf(adjustments).detectPending).toBeUndefined();
    // Undo to the preset's step keeps the selection; no step of its own.
    expect(history).toHaveLength(3);
    expect(history[0]).toBe(before);
    expect(skyOf(history[1]).maskDataBase64).toBe(SKY);
    // The shown recipe stays its History entry.
    expect(history[historyIndex]).toBe(adjustments);
    // Bitmaps it holds are not sent through IPC again.
    const sent = vi.mocked(invoke).mock.calls[0][1] as { recipes: Adjustments[] };
    expect(sent.recipes).toHaveLength(2);
  });

  it('asks again when the photographer edits while the backend matches', async () => {
    const preset = withSky({ ...INITIAL_ADJUSTMENTS } as Adjustments, pending);
    open(preset);
    let calls = 0;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command !== Invokes.AdoptSelections) return null;
      calls += 1;
      // A slider changed during the first answer.
      if (calls === 1) {
        const now = useEditorStore.getState();
        const moved = { ...now.adjustments, contrast: 25 };
        useEditorStore.setState({ adjustments: moved, history: [...now.history, moved], historyIndex: 1 });
      }
      const { recipes } = args as { recipes: Adjustments[] };
      return recipes.map((recipe) => ({ 'sky-1': { ...skyOf(recipe), ...computed } }));
    });
    await act(async () => {
      expect(await adoptQueuedSelections(PATH)).toBe(true);
    });
    expect(calls).toBe(2);
    const { adjustments } = useEditorStore.getState();
    expect(adjustments.contrast).toBe(25);
    expect(skyOf(adjustments).maskDataBase64).toBe(SKY);
  });

  it('waits for a slider drag to end', async () => {
    answerWithSky();
    open(withSky({ ...INITIAL_ADJUSTMENTS } as Adjustments, pending));
    useEditorStore.setState({ isSliderDragging: true });
    let done = false;
    const adopting = adoptQueuedSelections(PATH).then(() => (done = true));
    await act(async () => {});
    expect(done).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
    await act(async () => {
      useEditorStore.setState({ isSliderDragging: false });
      await adopting;
    });
    expect(skyOf(useEditorStore.getState().adjustments).maskDataBase64).toBe(SKY);
  });

  it('leaves another photo, and a component that holds its bitmap, alone', async () => {
    answerWithSky();
    open(withSky({ ...INITIAL_ADJUSTMENTS } as Adjustments, pending));
    expect(await adoptQueuedSelections('/library/other.nef')).toBe(false);
    const own = withSky({ ...INITIAL_ADJUSTMENTS } as Adjustments, { maskDataBase64: 'data:own' });
    open(own);
    expect(await adoptQueuedSelections(PATH)).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
    expect(useEditorStore.getState().adjustments).toBe(own);
  });
});
