import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key: string) => key }) }));

import StudioSpotControls, { StudioSpotList } from './StudioSpotControls';
import { toggleRemoveObjects } from './useStudioTools';
import { useEditorStore } from '../../store/useEditorStore';
import { INITIAL_ADJUSTMENTS } from '../../utils/adjustments';
import { KEYBIND_DEFINITIONS } from '../../utils/keyboardUtils';
import { findAnotherSource, resetTurnedDown, resetTurnedDownForPhoto, sourcesTurnedDown } from '../../utils/spotSource';

beforeEach(() => {
  useEditorStore.setState({
    selectedImage: { path: '/s/a.NEF', isReady: true, isRaw: true } as never,
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    isSpotToolActive: true,
    activeSpotId: null,
    spotSettings: { mode: 'heal', size: 60, feather: 0, opacity: 100 },
    spotVisualize: { on: false, sensitivity: 50 }
  });
});
afterEach(cleanup);

describe('spot removal settings', () => {
  it('offers Detect objects in Remove only, with a brush (and its size) or a box', () => {
    render(<StudioSpotControls />);
    expect(screen.queryByLabelText('spotRemoval.detectObjects')).toBeNull();
    expect(screen.getByLabelText('spotRemoval.featherAria')).toBeTruthy();
    fireEvent.click(screen.getByRole('radio', { name: 'spotRemoval.modes.remove' }));
    // Feather and opacity do not apply to Remove.
    expect(screen.queryByLabelText('spotRemoval.featherAria')).toBeNull();
    expect(screen.getByLabelText('spotRemoval.sizeAria')).toBeTruthy();
    fireEvent.click(screen.getByLabelText('spotRemoval.detectObjects'));
    expect(useEditorStore.getState().spotSettings).toMatchObject({ mode: 'remove', detectObjects: true });
    // Brushing over the object is the default: its size stays.
    expect(
      screen.getByRole('radio', { name: 'spotRemoval.objectSelectModes.brush' }).getAttribute('aria-checked')
    ).toBe('true');
    expect(screen.getByLabelText('spotRemoval.sizeAria')).toBeTruthy();
    expect(screen.getByText(/spotRemoval\.detectObjectsHint/)).toBeTruthy();
    expect(screen.getByText(/spotRemoval\.detectObjectsRefineHint/)).toBeTruthy();
    fireEvent.click(screen.getByRole('radio', { name: 'spotRemoval.objectSelectModes.box' }));
    expect(useEditorStore.getState().spotSettings.objectSelect).toBe('box');
    expect(screen.queryByLabelText('spotRemoval.sizeAria')).toBeNull();
    expect(screen.getByText(/spotRemoval\.detectObjectsBoxHint/)).toBeTruthy();
  });

  it('offers Fill across edges in Heal only, on unless turned off', () => {
    render(<StudioSpotControls />);
    const box = screen.getByLabelText('spotRemoval.fillAcrossEdges') as HTMLInputElement;
    expect(box.checked).toBe(true);
    fireEvent.click(box);
    expect(useEditorStore.getState().spotSettings.fillAcrossEdges).toBe(false);
    fireEvent.click(screen.getByRole('radio', { name: 'spotRemoval.modes.clone' }));
    expect(screen.queryByLabelText('spotRemoval.fillAcrossEdges')).toBeNull();
    fireEvent.click(screen.getByRole('radio', { name: 'spotRemoval.modes.remove' }));
    expect(screen.queryByLabelText('spotRemoval.fillAcrossEdges')).toBeNull();
  });
});

describe('show removals', () => {
  it('hides and shows every removal, and only when there are some', () => {
    render(<StudioSpotControls />);
    expect(screen.queryByLabelText('spotRemoval.showRemovals')).toBeNull();
    cleanup();
    const patch = (id: string, name: string) => ({
      id,
      name,
      isLoading: false,
      invert: false,
      visible: true,
      prompt: '',
      patchData: null,
      subMasks: []
    });
    useEditorStore.setState({
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), aiPatches: [patch('a', 'Remove'), patch('b', 'Other')] }
    });
    render(<StudioSpotControls />);
    const box = screen.getByLabelText('spotRemoval.showRemovals') as HTMLInputElement;
    expect(box.checked).toBe(true);
    fireEvent.click(box);
    const visible = () => useEditorStore.getState().adjustments.aiPatches!.map((p) => p.visible);
    expect(visible()).toEqual([false, true]);
    fireEvent.click(screen.getByLabelText('spotRemoval.showRemovals'));
    expect(visible()).toEqual([true, true]);
  });
});

describe('Shift+Q (remove objects)', () => {
  it('is a tools keybind of its own', () => {
    const def = KEYBIND_DEFINITIONS.find((d) => d.action === 'remove_objects');
    expect(def).toMatchObject({ defaultCombo: ['shift', 'KeyQ'], section: 'tools' });
    const combos = KEYBIND_DEFINITIONS.flatMap((d) => [d.defaultCombo, ...(d.aliasCombos ?? [])])
      .filter((c) => c.length)
      .map((c) => c.join('+'));
    expect(combos.filter((c) => c === 'shift+KeyQ')).toHaveLength(1);
  });

  it('opens the spot tool in Remove with Detect objects, and ends it when pressed again', () => {
    const selectTool = vi.fn((tool: string) => {
      if (tool === 'spot') useEditorStore.setState({ isSpotToolActive: !useEditorStore.getState().isSpotToolActive });
    });
    useEditorStore.setState({ isSpotToolActive: false });
    toggleRemoveObjects(selectTool);
    expect(selectTool).toHaveBeenCalledWith('spot');
    expect(useEditorStore.getState()).toMatchObject({
      isSpotToolActive: true,
      spotSettings: { mode: 'remove', detectObjects: true, size: 60 }
    });
    toggleRemoveObjects(selectTool);
    expect(useEditorStore.getState().isSpotToolActive).toBe(false);
    // With the tool on in Heal, it switches the mode without ending the tool.
    useEditorStore.setState({
      isSpotToolActive: true,
      spotSettings: { mode: 'heal', size: 60, feather: 0, opacity: 100 }
    });
    selectTool.mockClear();
    toggleRemoveObjects(selectTool);
    expect(selectTool).not.toHaveBeenCalled();
    expect(useEditorStore.getState()).toMatchObject({
      isSpotToolActive: true,
      spotSettings: { mode: 'remove', detectObjects: true }
    });
  });
});

describe('Find another source', () => {
  const spot = (id: string, mode: 'heal' | 'clone') => ({
    id,
    mode,
    x: 10,
    y: 10,
    radius: 5,
    sourceX: 30,
    sourceY: 10,
    feather: 0,
    opacity: 100,
    visible: true
  });
  beforeEach(() => {
    invoke.mockReset();
    resetTurnedDownForPhoto('/s/a.NEF');
    useEditorStore.setState({
      selectedImage: { path: '/s/a.NEF', isReady: true, isRaw: true } as never,
      isSpotToolActive: true,
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), spots: [spot('s1', 'heal'), spot('s2', 'clone')] },
      activeSpotId: null
    });
  });

  it('is a tools keybind on / of its own (Lightroom), free in every other combo', () => {
    const def = KEYBIND_DEFINITIONS.find((d) => d.action === 'spot_new_source');
    expect(def).toMatchObject({ defaultCombo: ['Slash'], section: 'tools' });
    const combos = KEYBIND_DEFINITIONS.flatMap((d) => [d.defaultCombo, ...(d.aliasCombos ?? [])])
      .filter((c) => c.length)
      .map((c) => c.join('+'));
    expect(combos.filter((c) => c === 'Slash')).toHaveLength(1);
  });

  it("asks the backend from the spot's row, then from the settings' item, each turning the old source down", async () => {
    invoke.mockImplementation((_command: string, args: { excluded: unknown[] }) =>
      Promise.resolve(args.excluded.length ? { x: 50, y: 40 } : { x: 40, y: 10 })
    );
    render(<StudioSpotList />);
    // Heal and clone spots have one; a fill removal has none.
    const buttons = screen.getAllByRole('button', { name: 'spotRemoval.list.newSource' });
    expect(buttons).toHaveLength(2);
    await act(async () => fireEvent.click(buttons[0]));
    const ask = invoke.mock.calls.filter(([name]) => name === 'find_another_spot_source').at(-1)!;
    expect(ask[1]).toMatchObject({ spotId: 's1', excluded: [] });
    // The spot only gets a new source (one recipe change), the other keeps its own.
    expect(useEditorStore.getState().adjustments.spots.map((s) => [s.id, s.sourceX, s.sourceY])).toEqual([
      ['s1', 40, 10],
      ['s2', 30, 10]
    ]);
    expect(sourcesTurnedDown('s1')).toEqual([[30, 10]]);
    cleanup();

    // The settings' item next (the right-click popover on the photo shows
    // these settings), with the source the spot leaves turned down.
    useEditorStore.setState({ activeSpotId: 's1' });
    resetTurnedDownForPhoto('/s/a.NEF');
    render(<StudioSpotControls />);
    await act(async () => fireEvent.click(screen.getByTitle('spotRemoval.findAnotherSourceHint')));
    const last = invoke.mock.calls.filter(([name]) => name === 'find_another_spot_source').at(-1)!;
    expect(sourcesTurnedDown('s1')).toEqual([
      [30, 10],
      [40, 10]
    ]);
    expect(last[1]).toMatchObject({ spotId: 's1', excluded: [{ x: 30, y: 10 }] });
    expect(useEditorStore.getState().adjustments.spots[0]).toMatchObject({ sourceX: 50, sourceY: 40 });
    expect(useEditorStore.getState().adjustments.spots[1]).toMatchObject({ sourceX: 30, sourceY: 10 });
  });

  it('drops an answer for a spot that moved or changed its source meanwhile', async () => {
    resetTurnedDown('s1');
    let answer: (v: { x: number; y: number }) => void = () => {};
    invoke.mockImplementation(() => new Promise((resolve) => (answer = resolve)));
    const pending = findAnotherSource('s1');
    // A source drag lands while the search runs.
    useEditorStore.setState((st) => ({
      adjustments: {
        ...st.adjustments,
        spots: st.adjustments.spots.map((sp) => (sp.id === 's1' ? { ...sp, sourceX: 99, sourceY: 98 } : sp))
      }
    }));
    answer({ x: 40, y: 10 });
    expect(await pending).toBe(false);
    expect(useEditorStore.getState().adjustments.spots[0]).toMatchObject({ sourceX: 99, sourceY: 98 });
    expect(sourcesTurnedDown('s1')).toEqual([]);
  });

  it('makes each quick press its own undo step', async () => {
    useEditorStore.getState().resetHistory(useEditorStore.getState().adjustments);
    let n = 0;
    invoke.mockImplementation(() => Promise.resolve({ x: 40 + 10 * ++n, y: 10 }));
    // Both settle well inside the 500 ms history debounce.
    await act(async () => {
      await findAnotherSource('s1');
      await findAnotherSource('s1');
    });
    const { history, historyIndex } = useEditorStore.getState();
    expect(history).toHaveLength(3);
    expect(historyIndex).toBe(2);
    expect(history[1].spots[0]).toMatchObject({ sourceX: 50 });
    expect(history[2].spots[0]).toMatchObject({ sourceX: 60 });
  });

  it('keeps the spot as it was when no other source fits', async () => {
    invoke.mockResolvedValue(null);
    render(<StudioSpotList />);
    const before = useEditorStore.getState().adjustments;
    await act(async () => fireEvent.click(screen.getAllByRole('button', { name: 'spotRemoval.list.newSource' })[0]));
    expect(useEditorStore.getState().adjustments).toBe(before);
  });

  it('offers no item for a fill removal or a red eye correction', () => {
    useEditorStore.setState({ adjustments: structuredClone(INITIAL_ADJUSTMENTS), activeSpotId: null });
    render(<StudioSpotControls />);
    expect(screen.queryByRole('button', { name: /spotRemoval\.findAnotherSource/ })).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe('spot list and deleting', () => {
  const spot = (id: string, mode: 'heal' | 'clone', visible = true) => ({
    id,
    mode,
    x: 10,
    y: 10,
    radius: 5,
    sourceX: 30,
    sourceY: 10,
    feather: 0,
    opacity: 100,
    visible
  });
  const patch = (id: string, name: string) => ({
    id,
    name,
    isLoading: false,
    invert: false,
    visible: true,
    prompt: '',
    patchData: null,
    subMasks: []
  });
  const eye = (id: string) => ({
    id,
    version: 1,
    kind: 'red' as const,
    x: 0.5,
    y: 0.5,
    radiusX: 0.01,
    radiusY: 0.01,
    angle: 0,
    pupilSize: 50,
    darken: 50,
    catchlight: false,
    visible: true
  });
  beforeEach(() =>
    useEditorStore.setState({
      isSpotToolActive: false,
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        spots: [spot('s1', 'heal'), spot('s2', 'clone', false)],
        aiPatches: [patch('r1', 'Remove'), patch('o1', 'Other')] as never,
        redEyes: [eye('e1')] as never
      }
    })
  );
  const rows = () => screen.getAllByRole('listitem');

  it('lists the spots, then the removals; a click selects one and starts the tool', () => {
    render(<StudioSpotList />);
    expect(rows()).toHaveLength(3);
    // The hidden spot's eye is off.
    const eyes = screen.getAllByRole('button', { name: 'spotRemoval.list.toggle' });
    expect(eyes.map((b) => b.getAttribute('aria-pressed'))).toEqual(['true', 'false', 'true']);
    fireEvent.click(screen.getAllByRole('button', { name: 'spotRemoval.list.select' })[2]);
    expect(useEditorStore.getState()).toMatchObject({ isSpotToolActive: true, activeSpotId: 'r1' });
  });

  it('hides, shows and deletes one or all, leaving other AI patches alone', () => {
    render(<StudioSpotList />);
    fireEvent.click(screen.getAllByRole('button', { name: 'spotRemoval.list.toggle' })[1]);
    expect(useEditorStore.getState().adjustments.spots[1].visible).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'spotRemoval.list.hideAll' }));
    const hidden = useEditorStore.getState().adjustments;
    expect(hidden.spots.map((s) => s.visible)).toEqual([false, false]);
    expect(hidden.aiPatches.map((p) => p.visible)).toEqual([false, true]);
    fireEvent.click(screen.getByRole('button', { name: 'spotRemoval.list.showAll' }));
    expect(useEditorStore.getState().adjustments.spots.map((s) => s.visible)).toEqual([true, true]);
    useEditorStore.setState({ activeSpotId: 's1' });
    fireEvent.click(screen.getAllByRole('button', { name: 'spotRemoval.list.delete' })[0]);
    expect(useEditorStore.getState().adjustments.spots.map((s) => s.id)).toEqual(['s2']);
    expect(useEditorStore.getState().activeSpotId).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'spotRemoval.list.deleteAll' }));
    const after = useEditorStore.getState().adjustments;
    expect(after.spots).toHaveLength(0);
    expect(after.aiPatches.map((p) => p.id)).toEqual(['o1']);
    // Red eye corrections belong to Red eye mode's list.
    expect(after.redEyes).toHaveLength(1);
    expect(screen.queryByRole('list')).toBeNull();
  });

  it("turns a heal spot into a fill and the heal tool's fill back into a heal, showing it as busy meanwhile", async () => {
    const patchData = { color: 'c', mask: 'm', offsetX: 0, offsetY: 0, width: 4, height: 4, isSrgbEncoded: true };
    let finish: (json: string) => void = () => {};
    invoke.mockReset();
    invoke.mockImplementation((command: string) =>
      command === 'find_spot_source'
        ? Promise.resolve({ source: [40, 10], fill: false })
        : new Promise((resolve) => {
            finish = resolve;
          })
    );
    render(<StudioSpotList />);
    // Only heal spots and the heal tool's fills can be switched.
    expect(screen.getAllByRole('button', { name: 'spotRemoval.list.useFill' })).toHaveLength(1);
    expect(screen.queryByRole('button', { name: 'spotRemoval.list.useHeal' })).toBeNull();
    await act(async () => fireEvent.click(screen.getByRole('button', { name: 'spotRemoval.list.useFill' })));
    const call = invoke.mock.calls.find(([name]) => name === 'invoke_generative_replace_with_mask_def')!;
    const args = call[1] as {
      currentAdjustments: { spots: { id: string }[] };
      patchDefinition: { fromHeal: boolean; subMasks: { parameters: { lines: unknown[] } }[] };
      taskId: string;
    };
    // Filled from the photo without the heal, the spot's own area, under the spot's id.
    expect(args.currentAdjustments.spots.map((s) => s.id)).toEqual(['s2']);
    expect(args.patchDefinition.fromHeal).toBe(true);
    expect(args.patchDefinition.subMasks[0].parameters.lines).toEqual([
      expect.objectContaining({ points: [{ x: 10, y: 10 }], brushSize: 10 })
    ]);
    expect(args.taskId).toBe('s1');
    // The spot's opacity and feather carry over (feather never below Remove's).
    expect(args.patchDefinition.subMasks[0]).toMatchObject({ opacity: 100 });
    expect(args.patchDefinition.subMasks[0].parameters.lines[0]).toMatchObject({ feather: 0.2 });
    expect(screen.getByRole('status', { name: 'spotRemoval.list.switching' })).toBeTruthy();
    // Nothing changes in the recipe until the fill is ready.
    expect(useEditorStore.getState().adjustments.spots.map((s) => s.id)).toEqual(['s1', 's2']);
    await act(async () => finish(JSON.stringify(patchData)));
    const filled = useEditorStore.getState().adjustments;
    expect(filled.spots.map((s) => s.id)).toEqual(['s2']);
    const fill = filled.aiPatches.find((p) => p.fromHeal)!;
    expect(fill).toMatchObject({ name: 'Remove', patchData });
    expect(screen.queryByRole('status')).toBeNull();
    // Back to a heal with an automatic source.
    await act(async () => fireEvent.click(screen.getByRole('button', { name: 'spotRemoval.list.useHeal' })));
    const ask = invoke.mock.calls.find(([name]) => name === 'find_spot_source')!;
    expect(ask[1]).toMatchObject({ x: 10, y: 10, radius: 5, points: null, fillAcrossEdges: false });
    const healed = useEditorStore.getState().adjustments;
    expect(healed.aiPatches.some((p) => p.id === fill.id)).toBe(false);
    expect(healed.spots.at(-1)).toMatchObject({
      mode: 'heal',
      x: 10,
      y: 10,
      radius: 5,
      sourceX: 40,
      sourceY: 10,
      opacity: 100,
      feather: 20
    });
  });

  it('lists red eye corrections in Red eye mode', () => {
    useEditorStore.setState({ spotSettings: { mode: 'redEye', size: 60, feather: 0, opacity: 100 } });
    render(<StudioSpotList />);
    expect(rows()).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'spotRemoval.list.deleteAll' }));
    expect(useEditorStore.getState().adjustments.redEyes).toHaveLength(0);
    expect(useEditorStore.getState().adjustments.spots).toHaveLength(2);
  });

  it('deletes the selected spot or removal from the settings, also in the popover', () => {
    useEditorStore.setState({ isSpotToolActive: true });
    render(<StudioSpotControls />);
    expect(screen.queryByRole('button', { name: 'spotRemoval.deleteSelected' })).toBeNull();
    act(() => useEditorStore.setState({ activeSpotId: 'r1' }));
    fireEvent.click(screen.getByRole('button', { name: 'spotRemoval.deleteSelected' }));
    expect(useEditorStore.getState().adjustments.aiPatches.map((p) => p.id)).toEqual(['o1']);
    expect(useEditorStore.getState().activeSpotId).toBeNull();
    expect(screen.queryByRole('button', { name: 'spotRemoval.deleteSelected' })).toBeNull();
  });

  it('starts the tool when a mode or Detect objects is chosen, never ends it', () => {
    render(<StudioSpotControls />);
    fireEvent.click(screen.getByRole('radio', { name: 'spotRemoval.modes.clone' }));
    expect(useEditorStore.getState().isSpotToolActive).toBe(true);
    fireEvent.click(screen.getByRole('radio', { name: 'spotRemoval.modes.clone' }));
    expect(useEditorStore.getState().isSpotToolActive).toBe(true);
    act(() => useEditorStore.setState({ isSpotToolActive: false }));
    fireEvent.change(screen.getByLabelText('spotRemoval.sizeAria'), { target: { value: '90' } });
    expect(useEditorStore.getState().isSpotToolActive).toBe(false);
    fireEvent.click(screen.getByRole('radio', { name: 'spotRemoval.modes.remove' }));
    act(() => useEditorStore.setState({ isSpotToolActive: false }));
    fireEvent.click(screen.getByLabelText('spotRemoval.detectObjects'));
    expect(useEditorStore.getState().isSpotToolActive).toBe(true);
  });
});
