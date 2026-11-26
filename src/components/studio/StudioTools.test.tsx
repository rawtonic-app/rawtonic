import { useSettingsStore } from '../../store/useSettingsStore';
import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, renderHook, screen, within } from '@testing-library/react';
import type { MouseEvent as ReactMouseEvent } from 'react';
import StudioWorkspace from './StudioWorkspace';
import StudioToolBar from './StudioToolBar';
import { useQuickAdjust } from './useQuickAdjust';
import { stepBrushSize, useStudioTools } from './useStudioTools';
import { useEditorStore } from '../../store/useEditorStore';
import { useUIStore } from '../../store/useUIStore';
import { debouncedSetHistory } from '../../hooks/useEditorActions';
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER } from '../../utils/adjustments';
import { createSubMask } from '../../utils/maskUtils';
import { registerSliderDraftRenderer } from '../../utils/sliderDraft';
import { kindOf } from '../../utils/maskKinds';
import { Mask, SubMaskMode, ToolType } from '../panel/right/Masks';
import { Panel, type SelectedImage } from '../ui/AppProperties';

// The viewer stands in for the canvas: a surface that reports right-clicks.
vi.mock('../panel/Editor', () => ({
  default: ({ onContextMenu }: { onContextMenu(e: ReactMouseEvent): void }) => (
    <div data-testid="photo" onContextMenu={onContextMenu} />
  )
}));
vi.mock('./StudioDevelop', () => ({
  default: () => null,
  ToolGroup: () => null
}));
vi.mock('./StudioNavigation', () => ({ default: () => null }));

const image = { width: 6000, height: 4000 } as SelectedImage;
beforeEach(() => {
  HTMLDialogElement.prototype.close = vi.fn();
  HTMLDialogElement.prototype.showModal = vi.fn();
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    }
  );
  useUIStore.setState({ activeView: 'editor', activePanel: Panel.Adjustments });
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    selectedImage: {
      path: '/qa/photo.dng',
      width: 6000,
      height: 4000,
      isRaw: true,
      isReady: true
    } as SelectedImage
  });
});
afterEach(() => {
  cleanup();
  debouncedSetHistory.cancel();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const editor = () => useEditorStore.getState();
/** A key press as the browser sends it: down and up on the focused element. */
const press = (key: string, code: string, init: KeyboardEventInit = {}) => {
  fireEvent.keyDown(document.body, { key, code, ...init });
  fireEvent.keyUp(document.body, { key, code, ...init });
};
const pressed = (name: string) => screen.getByRole('button', { name }).getAttribute('aria-pressed');
const withLinearLayer = () => {
  const linear = createSubMask(Mask.Linear, image);
  const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
  adjustments.masks = [
    {
      ...structuredClone(INITIAL_MASK_CONTAINER),
      id: 'layer',
      name: 'Sky',
      subMasks: [linear]
    }
  ];
  useEditorStore.setState({
    adjustments,
    activeMaskContainerId: 'layer',
    activeMaskId: linear.id
  });
  useUIStore.setState({ activePanel: Panel.Masks });
  return linear;
};
const workspace = (onEditorContextMenu = vi.fn()) => {
  const noop = vi.fn();
  render(
    <div className="studio-app">
      <StudioWorkspace
        files={[]}
        flagCounts={{ all: 0, picked: 0, rejected: 0, unflagged: 0 }}
        transformWrapperRef={{ current: null }}
        onSelect={noop}
        onFilmSelect={noop}
        onOpen={noop}
        onNavigate={noop}
        onBack={noop}
        onRate={noop}
        onContextMenu={noop}
        onEditorContextMenu={onEditorContextMenu}
        onCopy={noop}
        onPaste={noop}
        onReset={noop}
        onZoom={noop}
        requestThumbnails={noop}
        settings={null}
        onSettings={noop}
      />
    </div>
  );
  return onEditorContextMenu;
};

describe('brush size steps', () => {
  it('change by about 15%, at least one pixel, within 1–6000', () => {
    expect(stepBrushSize(50, 1)).toBe(58);
    expect(stepBrushSize(50, -1)).toBe(42);
    expect(stepBrushSize(1, 1)).toBe(2);
    expect(stepBrushSize(2, -1)).toBe(1);
    expect(stepBrushSize(1, -1)).toBe(1);
    expect(stepBrushSize(5900, 1)).toBe(6000);
  });
});

describe('tool state', () => {
  it("re-renders its users for layer edits, not for the photograph's values", () => {
    withLinearLayer();
    let renders = 0;
    const { result } = renderHook(() => {
      renders++;
      return useStudioTools();
    });
    const before = renders;
    // A slider drag's commits change the photograph's values only.
    act(() =>
      useEditorStore.setState((state) => ({
        adjustments: { ...state.adjustments, exposure: 1 }
      }))
    );
    expect(renders).toBe(before);
    act(() =>
      useEditorStore.setState((state) => ({
        adjustments: {
          ...state.adjustments,
          masks: state.adjustments.masks.map((m) => ({ ...m, invert: true }))
        }
      }))
    );
    expect(renders).toBe(before + 1);
    expect(result.current.layer?.invert).toBe(true);
    expect(result.current.activeTool).toBe('linear');
  });
});

describe('cursor tool bar', () => {
  it('starts a brush layer when no layer is selected and offers erasing only with a layer', () => {
    render(<StudioToolBar />);
    expect(pressed('Select')).toBe('true');
    expect((screen.getByRole('button', { name: 'Eraser' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Brush' }));
    const layer = editor().adjustments.masks[0];
    expect(layer.subMasks[0].type).toBe(Mask.Brush);
    expect(editor().activeMaskId).toBe(layer.subMasks[0].id);
    expect(useUIStore.getState().activePanel).toBe(Panel.Masks);
    expect(pressed('Brush')).toBe('true');
  });

  it('draws and erases on the selected layer, keeping its gradient', () => {
    const linear = withLinearLayer();
    render(<StudioToolBar />);
    expect(pressed('Linear gradient')).toBe('true');
    fireEvent.click(screen.getByRole('button', { name: 'Eraser' }));
    const layer = editor().adjustments.masks[0];
    expect(layer.subMasks[0]).toEqual(linear);
    expect(layer.subMasks.find((m) => m.id === editor().activeMaskId)?.mode).toBe(SubMaskMode.Subtractive);
    expect(pressed('Eraser')).toBe('true');
    fireEvent.click(screen.getByRole('button', { name: 'Select' }));
    expect(editor().activeMaskContainerId).toBe('layer');
    expect(editor().activeMaskId).toBeNull();
    expect(pressed('Select')).toBe('true');
  });

  it('keeps the pen pressure settings when switching to the eraser', () => {
    withLinearLayer();
    useEditorStore.setState({
      brushSettings: { size: 80, feather: 50, tool: ToolType.Brush, pressureSize: false, pressureFlow: true }
    });
    render(<StudioToolBar />);
    fireEvent.click(screen.getByRole('button', { name: 'Eraser' }));
    expect(editor().brushSettings).toMatchObject({ pressureSize: false, pressureFlow: true });
  });

  it('switches to crop and back, and the picker works on the photograph', () => {
    withLinearLayer();
    render(<StudioToolBar />);
    fireEvent.click(screen.getByRole('button', { name: 'Crop' }));
    expect(useUIStore.getState().activePanel).toBe(Panel.Crop);
    expect(editor().activeMaskContainerId).toBeNull();
    expect(pressed('Crop')).toBe('true');
    fireEvent.click(screen.getByRole('button', { name: 'Crop' }));
    expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
    fireEvent.click(screen.getByRole('button', { name: 'White balance picker' }));
    expect(editor().isWbPickerActive).toBe(true);
    expect(pressed('White balance picker')).toBe('true');
  });

  it('offers the loupe option while the white balance picker is active', () => {
    const previous = useSettingsStore.getState();
    const save = vi.fn(async (appSettings: NonNullable<typeof previous.appSettings>) =>
      useSettingsStore.setState({ appSettings })
    );
    useSettingsStore.setState({
      appSettings: {} as NonNullable<typeof previous.appSettings>,
      handleSettingsChange: save
    });
    try {
      render(<StudioToolBar />);
      expect(screen.queryByRole('checkbox', { name: 'Show Loupe' })).toBeNull();
      const picker = screen.getByRole('button', { name: 'White balance picker' });
      fireEvent.click(picker);
      const checkbox = screen.getByRole<HTMLInputElement>('checkbox', { name: 'Show Loupe' });
      expect(checkbox.checked).toBe(true);
      fireEvent.click(checkbox);
      expect(save).toHaveBeenCalledWith({ showWbLoupe: false });
      expect(checkbox.checked).toBe(false);
      expect(editor().isWbPickerActive).toBe(true);
      fireEvent.click(picker);
      expect(screen.queryByRole('checkbox', { name: 'Show Loupe' })).toBeNull();
      fireEvent.click(picker);
      expect(screen.getByRole<HTMLInputElement>('checkbox', { name: 'Show Loupe' }).checked).toBe(false);
    } finally {
      cleanup();
      useSettingsStore.setState(previous);
    }
  });

  it('does nothing before the photograph is ready', () => {
    useEditorStore.setState((s) => ({
      selectedImage: { ...s.selectedImage!, isReady: false }
    }));
    render(<StudioToolBar />);
    expect((screen.getByRole('button', { name: 'Brush' }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe('right-click on the photograph', () => {
  it("opens the photograph's menu without a selected layer", () => {
    const menu = workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), {
      clientX: 400,
      clientY: 300
    });
    expect(menu).toHaveBeenCalledOnce();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('opens the brush settings at the pointer while painting, and Escape closes only the panel', () => {
    withLinearLayer();
    const menu = workspace();
    fireEvent.click(screen.getByRole('button', { name: 'Brush' }));
    fireEvent.contextMenu(screen.getByTestId('photo'), {
      clientX: 400,
      clientY: 300
    });
    expect(menu).not.toHaveBeenCalled();
    const panel = screen.getByRole('dialog', { name: 'Brush settings' });
    expect(panel.textContent).toContain('Sky');
    expect(screen.queryByRole('textbox', { name: 'Brush size value (px)' })).toBeNull();
    const size = screen.getByRole('textbox', { name: 'Size value (px)' });
    fireEvent.focus(size);
    fireEvent.change(size, { target: { value: '120' } });
    fireEvent.blur(size);
    expect(editor().brushSettings?.size).toBe(120);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(editor().activeMaskContainerId).toBe('layer');
  });

  it('closes on a press outside and when the layer is deselected', () => {
    withLinearLayer();
    workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), {
      clientX: 10,
      clientY: 10
    });
    expect(screen.getByRole('dialog', { name: 'Linear gradient settings' })).toBeTruthy();
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole('dialog')).toBeNull();
    fireEvent.contextMenu(screen.getByTestId('photo'), {
      clientX: 10,
      clientY: 10
    });
    act(() =>
      useEditorStore.setState({
        activeMaskContainerId: null,
        activeMaskId: null
      })
    );
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});

describe('brush settings at the pointer', () => {
  const brushLayer = (mode = SubMaskMode.Additive) => {
    withLinearLayer();
    const brush = { ...createSubMask(Mask.Brush, image), mode };
    useEditorStore.setState((state) => ({
      adjustments: {
        ...state.adjustments,
        masks: state.adjustments.masks.map((mask) => ({ ...mask, subMasks: [brush] }))
      },
      activeMaskId: brush.id
    }));
  };
  const field = (name: string) => screen.getByRole('textbox', { name: `${name} value (px)` });

  it('keeps the ring at the right-click while open, and Enter applies a typed size and closes', () => {
    brushLayer();
    workspace();
    expect(editor().toolPopoverOpen).toBe(false);
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    expect(screen.getByRole('dialog', { name: 'Brush settings' })).toBeTruthy();
    expect(editor().toolPopoverOpen).toBe(true);
    const size = field('Size');
    act(() => size.focus());
    fireEvent.change(size, { target: { value: '140' } });
    fireEvent.keyDown(size, { key: 'Enter' });
    // The same state the tool panel's Size writes.
    expect(editor().brushSettings?.size).toBe(140);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(editor().toolPopoverOpen).toBe(false);
  });

  it('opens for the eraser and the spot tool, writing the spot settings the panel uses', () => {
    brushLayer(SubMaskMode.Subtractive);
    workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    expect(screen.getByRole('dialog', { name: 'Eraser settings' })).toBeTruthy();
    fireEvent.keyDown(window, { key: 'Escape' });
    act(() => useEditorStore.setState({ isSpotToolActive: true, activeMaskContainerId: null, activeMaskId: null }));
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    expect(screen.getByRole('dialog', { name: 'Spot removal settings' })).toBeTruthy();
    const size = field('Spot size');
    fireEvent.focus(size);
    fireEvent.change(size, { target: { value: '90' } });
    fireEvent.blur(size);
    expect(editor().spotSettings.size).toBe(90);
  });

  it('Escape keeps applied values; typed digits are dropped first, then the panel closes', () => {
    brushLayer();
    workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    const size = field('Size');
    fireEvent.focus(size);
    fireEvent.change(size, { target: { value: '150' } });
    fireEvent.blur(size);
    expect(editor().brushSettings?.size).toBe(150);
    fireEvent.focus(size);
    fireEvent.input(size, { target: { value: '99' } });
    fireEvent.keyDown(size, { key: 'Escape' });
    expect(screen.getByRole('dialog', { name: 'Brush settings' })).toBeTruthy();
    expect(editor().brushSettings?.size).toBe(150);
    fireEvent.keyDown(document.body, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(editor().brushSettings?.size).toBe(150);
  });

  it('closes on Escape after a checkbox changed, and Enter on a checkbox keeps the panel open', () => {
    brushLayer();
    workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    const autoMask = screen.getByRole('checkbox', { name: 'Auto mask' });
    act(() => autoMask.focus());
    fireEvent.click(autoMask);
    fireEvent.input(autoMask);
    expect(editor().brushSettings?.autoMask).toBe(true);
    fireEvent.keyDown(autoMask, { key: 'Enter' });
    expect(screen.getByRole('dialog', { name: 'Brush settings' })).toBeTruthy();
    fireEvent.keyDown(autoMask, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(editor().brushSettings?.autoMask).toBe(true);
  });

  it("closes on Enter from a slider's track", () => {
    brushLayer();
    workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    const track = screen.getByRole('slider', { name: 'Size' });
    act(() => track.focus());
    fireEvent.keyDown(track, { key: 'Enter' });
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('reaches every value field with Tab', () => {
    brushLayer();
    workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    const panel = screen.getByRole('dialog', { name: 'Brush settings' });
    const fields = within(panel)
      .getAllByRole('textbox')
      .filter((el) => el.classList.contains('adjustment-value'));
    expect(fields.length).toBeGreaterThanOrEqual(3);
    expect(panel.contains(document.activeElement)).toBe(true);
    act(() => fields[0].focus());
    for (let i = 1; i < fields.length; i++) {
      fireEvent.keyDown(document.activeElement!, { key: 'Tab' });
      expect(document.activeElement).toBe(fields[i]);
    }
  });

  it("opens the photograph's menu from its last row, where the right-click was", () => {
    brushLayer();
    const menu = workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    expect(menu).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Photo menu…' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(menu).toHaveBeenCalledOnce();
    expect(menu.mock.calls[0][0]).toMatchObject({ clientX: 400, clientY: 300 });
  });
});

describe('select tool and closing clicks', () => {
  it("keeps the photograph's menu with the select tool, even with a layer selected", () => {
    withLinearLayer();
    const menu = workspace();
    fireEvent.click(screen.getByRole('button', { name: 'Select' }));
    fireEvent.contextMenu(screen.getByTestId('photo'), {
      clientX: 400,
      clientY: 300
    });
    expect(menu).toHaveBeenCalledOnce();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('lets the press that closes the panel act on the photograph', () => {
    withLinearLayer();
    workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 10, clientY: 10 });
    const clicked = vi.fn();
    screen.getByTestId('photo').addEventListener('click', clicked);
    fireEvent.pointerDown(screen.getByTestId('photo'));
    fireEvent.click(screen.getByTestId('photo'));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(clicked).toHaveBeenCalledOnce();
  });

  it("closes the photograph's menu with the select tool without the click acting", () => {
    withLinearLayer();
    workspace();
    fireEvent.click(screen.getByRole('button', { name: 'Select' }));
    const menu = document.createElement('div');
    menu.setAttribute('role', 'menu');
    document.body.append(menu);
    const clicked = vi.fn();
    screen.getByTestId('photo').addEventListener('click', clicked);
    fireEvent.pointerDown(screen.getByTestId('photo'));
    menu.remove();
    fireEvent.mouseDown(screen.getByTestId('photo'));
    fireEvent.click(screen.getByTestId('photo'));
    expect(clicked).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('photo'));
    expect(clicked).toHaveBeenCalledOnce();
  });
});

describe('[ and ]', () => {
  it('resize the active brush and are left to rotation otherwise', () => {
    withLinearLayer();
    workspace();
    const rotate = vi.fn();
    window.addEventListener('keydown', rotate);
    fireEvent.keyDown(document.body, { key: ']' });
    expect(rotate).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button', { name: 'Brush' }));
    const before = editor().brushSettings?.size ?? 50;
    fireEvent.keyDown(document.body, { key: ']' });
    expect(editor().brushSettings?.size).toBe(stepBrushSize(before, 1));
    fireEvent.keyDown(document.body, { key: '[' });
    fireEvent.keyDown(document.body, { key: '[' });
    expect(editor().brushSettings?.size).toBeLessThan(before);
    expect(rotate).toHaveBeenCalledOnce();
    window.removeEventListener('keydown', rotate);
  });

  it('resize the brush while the slider just dragged keeps the focus, never rotating', () => {
    withLinearLayer();
    workspace();
    fireEvent.click(screen.getByRole('button', { name: 'Brush' }));
    const rotate = vi.fn();
    window.addEventListener('keydown', rotate);
    const slider = document.body.appendChild(document.createElement('input'));
    slider.type = 'range';
    slider.focus();
    const before = editor().brushSettings?.size ?? 50;
    fireEvent.keyDown(slider, { key: ']', code: 'BracketRight' });
    expect(editor().brushSettings?.size).toBe(stepBrushSize(before, 1));
    expect(rotate).not.toHaveBeenCalled();
    // The slider's own keys stay with it.
    fireEvent.keyDown(slider, { key: 'ArrowRight', code: 'ArrowRight' });
    expect(editor().brushSettings?.size).toBe(stepBrushSize(before, 1));
    window.removeEventListener('keydown', rotate);
    slider.remove();
  });
});

describe('Capture One tool keys', () => {
  it('pick the cursor tools in Develop', () => {
    workspace();
    press('b', 'KeyB');
    expect(pressed('Brush')).toBe('true');
    const layer = editor().activeMaskContainerId;
    expect(layer).toBeTruthy();
    press('e', 'KeyE');
    expect(pressed('Eraser')).toBe('true');
    expect(editor().activeMaskContainerId).toBe(layer);
    press('v', 'KeyV');
    expect(pressed('Select')).toBe('true');
    press('l', 'KeyL');
    expect(pressed('Linear gradient')).toBe('true');
    press('t', 'KeyT');
    expect(pressed('Radial gradient')).toBe('true');
    expect(editor().adjustments.masks).toHaveLength(3);
    press('c', 'KeyC');
    expect(useUIStore.getState().activePanel).toBe(Panel.Crop);
    // R is Capture One's straighten tool (in the crop view); again, crop.
    press('r', 'KeyR');
    expect(pressed('Straighten')).toBe('true');
    expect(editor().isStraightenActive).toBe(true);
    press('r', 'KeyR');
    expect(pressed('Crop')).toBe('true');
    press('c', 'KeyC');
    expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
    press('w', 'KeyW');
    expect(editor().isWbPickerActive).toBe(true);
  });

  it('leave the Library, modified keys and text fields alone', () => {
    workspace();
    press('b', 'KeyB', { metaKey: true });
    expect(editor().adjustments.masks).toHaveLength(0);
    const field = document.createElement('input');
    document.body.appendChild(field);
    fireEvent.keyDown(field, { key: 'b', code: 'KeyB' });
    fireEvent.keyUp(field, { key: 'b', code: 'KeyB' });
    expect(editor().adjustments.masks).toHaveLength(0);
    field.remove();
    act(() => useUIStore.setState({ activeView: 'library' }));
    press('b', 'KeyB');
    expect(editor().adjustments.masks).toHaveLength(0);
  });

  it("follow the user's own keybinds", async () => {
    const { useSettingsStore } = await import('../../store/useSettingsStore');
    useSettingsStore.setState({
      appSettings: { keybinds: { tool_brush: ['KeyN'] } } as never
    });
    try {
      workspace();
      press('b', 'KeyB');
      press('k', 'KeyK');
      expect(editor().adjustments.masks).toHaveLength(0);
      press('n', 'KeyN');
      expect(pressed('Brush')).toBe('true');
    } finally {
      useSettingsStore.setState({ appSettings: null });
    }
  });

  it('start a Subject or Background layer with Shift+, and Shift+.', () => {
    workspace();
    // German Mac layouts type ; and : there: the key's code decides.
    press(';', 'Comma', { shiftKey: true });
    press(':', 'Period', { shiftKey: true });
    const masks = editor().adjustments.masks;
    expect(masks).toHaveLength(2);
    expect(masks.map((m) => kindOf(m.subMasks[0]))).toEqual(['subject', 'background']);
    expect(editor().activeMaskContainerId).toBe(masks[1].id);
    expect(useUIStore.getState().activePanel).toBe(Panel.Masks);
  });

  it('change brush hardness with Shift and accept Option-typed brackets', () => {
    workspace();
    fireEvent.click(screen.getByRole('button', { name: 'Brush' }));
    const { size, hardness = 50 } = editor().brushSettings!;
    fireEvent.keyDown(document.body, { key: '}', shiftKey: true });
    expect(editor().brushSettings?.hardness).toBe(hardness + 10);
    expect(editor().brushSettings?.size).toBe(size);
    // German Mac layouts type ] as Option+6.
    fireEvent.keyDown(document.body, { key: ']', altKey: true });
    expect(editor().brushSettings?.size).toBe(stepBrushSize(size, 1));
  });

  it('size the brush with the keys bound to Rotate on a Swiss or German layout, never turning the photograph', () => {
    // MSMOKE1 (Swiss German Mac): the physical [ ] keys type ü and ¨; they
    // carry Rotate left/right, which turned the photo (a History step) while
    // the brush was active.
    withLinearLayer();
    workspace();
    const rotate = vi.fn();
    window.addEventListener('keydown', rotate);
    fireEvent.click(screen.getByRole('button', { name: 'Brush' }));
    const size = editor().brushSettings?.size ?? 50;
    const recipe = editor().adjustments;
    fireEvent.keyDown(document.body, { key: '¨', code: 'BracketRight' });
    expect(editor().brushSettings?.size).toBe(stepBrushSize(size, 1));
    fireEvent.keyDown(document.body, { key: 'ü', code: 'BracketLeft' });
    fireEvent.keyDown(document.body, { key: 'ü', code: 'BracketLeft' });
    expect(editor().brushSettings?.size).toBe(stepBrushSize(stepBrushSize(stepBrushSize(size, 1), -1), -1));
    // With Shift (è on that layout) the hardness, as Shift+[ elsewhere.
    const hardness = editor().brushSettings?.hardness ?? 50;
    fireEvent.keyDown(document.body, { key: 'è', code: 'BracketLeft', shiftKey: true });
    expect(editor().brushSettings?.hardness).toBe(Math.max(0, hardness - 10));
    expect(rotate).not.toHaveBeenCalled();
    expect(editor().adjustments).toBe(recipe);
    window.removeEventListener('keydown', rotate);
  });
});

describe('layer selector in the viewer bar', () => {
  it('picks the layer the tools act on, and Background returns to the photograph', () => {
    withLinearLayer();
    useEditorStore.setState({ activeMaskContainerId: null, activeMaskId: null });
    useUIStore.setState({ activePanel: Panel.Adjustments });
    workspace();
    const select = screen.getByRole('combobox', { name: 'Layer' }) as HTMLSelectElement;
    expect(select.value).toBe('');
    fireEvent.change(select, { target: { value: 'layer' } });
    expect(editor().activeMaskContainerId).toBe('layer');
    expect(editor().activeMaskId).toBeNull();
    expect(useUIStore.getState().activePanel).toBe(Panel.Masks);
    expect(select.value).toBe('layer');
    fireEvent.change(select, { target: { value: '' } });
    expect(editor().activeMaskContainerId).toBeNull();
    expect(select.value).toBe('');
  });
});

describe('crop tool panel', () => {
  it('opens on right-click with the crop tool, swaps orientation and resets', () => {
    const menu = workspace();
    fireEvent.click(screen.getByRole('button', { name: 'Crop' }));
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    expect(menu).not.toHaveBeenCalled();
    const panel = screen.getByRole('dialog', { name: 'Crop settings' });
    expect(panel.textContent).toContain('6000 × 4000 px');
    const ratio = screen.getByRole('combobox', { name: 'Crop aspect ratio' });
    fireEvent.change(ratio, { target: { value: '1.5' } });
    expect(editor().adjustments.aspectRatio).toBe(1.5);
    fireEvent.click(screen.getByRole('button', { name: 'Portrait' }));
    expect(editor().adjustments.aspectRatio).toBeCloseTo(2 / 3);
    // A new ratio keeps the portrait orientation.
    fireEvent.change(ratio, { target: { value: String(16 / 9) } });
    expect(editor().adjustments.aspectRatio).toBeCloseTo(9 / 16);
    fireEvent.click(within(panel).getByRole('button', { name: 'Reset crop and straighten' }));
    expect(editor().adjustments.aspectRatio).toBeNull();
    expect(editor().adjustments.crop).toBeNull();
  });

  it('adds a custom ratio with Enter in its fields, without closing the panel first', () => {
    useUIStore.setState({ activePanel: Panel.Crop });
    useSettingsStore.setState({ appSettings: { customAspectRatios: [] } as never });
    workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    fireEvent.click(screen.getByRole('button', { name: 'Add a crop ratio' }));
    fireEvent.change(screen.getByRole('textbox', { name: 'Ratio width' }), { target: { value: '7' } });
    const height = screen.getByRole('textbox', { name: 'Ratio height' });
    fireEvent.change(height, { target: { value: '5' } });
    const event = fireEvent.keyDown(height, { key: 'Enter' });
    // Not taken by the panel: the browser goes on to submit the form.
    expect(event).toBe(true);
    expect(screen.getByRole('dialog', { name: 'Crop settings' })).toBeTruthy();
  });

  it('opens with the pointer in its title bar', () => {
    workspace();
    fireEvent.click(screen.getByRole('button', { name: 'Crop' }));
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    const panel = screen.getByRole('dialog', { name: 'Crop settings' });
    // jsdom has no layout: the title is 0 px high, so the pointer is at its top.
    expect(panel.style.left).toBe('368px');
    expect(panel.style.top).toBe('300px');
  });
});

describe('new gradients', () => {
  it('wait for a drag on the photo to draw them, outside the recipe', () => {
    workspace();
    useUIStore.setState({ activePanel: Panel.Masks });
    press('l', 'KeyL');
    const linear = editor().adjustments.masks[0].subMasks[0];
    expect(linear.type).toBe(Mask.Linear);
    // The next drag draws it (notes: reference/wp7-use-test-c1-lr.md finding 2).
    expect(editor().shapeDrawTarget).toBe(linear.id);
    expect(Object.keys(linear.parameters)).not.toContain('isInitialDraw');
    press('t', 'KeyT');
    const radial = editor().adjustments.masks[1].subMasks[0];
    expect(editor().shapeDrawTarget).toBe(radial.id);
    // Picking another component ends it; a brush never waits for a drag.
    useEditorStore.setState({ activeMaskId: linear.id });
    expect(editor().shapeDrawTarget).toBeNull();
    press('b', 'KeyB');
    expect(editor().shapeDrawTarget).toBeNull();
  });
});

describe('radial panel', () => {
  it('draws the mask inside or outside', () => {
    const radial = createSubMask(Mask.Radial, image);
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.masks = [{ ...structuredClone(INITIAL_MASK_CONTAINER), id: 'r', name: 'Spot', subMasks: [radial] }];
    useEditorStore.setState({ adjustments, activeMaskContainerId: 'r', activeMaskId: radial.id });
    useUIStore.setState({ activePanel: Panel.Masks });
    workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    const inside = screen.getByRole('checkbox', { name: 'Draw mask inside' }) as HTMLInputElement;
    expect(inside.checked).toBe(!radial.invert);
    fireEvent.click(inside);
    expect(editor().adjustments.masks[0].subMasks[0].invert).toBe(!radial.invert ? true : false);
  });
});

describe('Quick Adjust (Capture One)', () => {
  it('changes a control while its key is held and the wheel turns over the photo', () => {
    workspace();
    const photo = screen.getByTestId('photo');
    fireEvent.keyDown(document.body, { key: 'q', code: 'KeyQ' });
    fireEvent.wheel(photo, { deltaY: -100, clientX: 300, clientY: 200 });
    fireEvent.wheel(photo, { deltaY: -100, clientX: 300, clientY: 200 });
    expect(editor().adjustments.exposure).toBeCloseTo(0.1);
    expect(screen.getByRole('status').textContent).toBe('Exposure+0.1');
    expect(editor().isSliderDragging).toBe(true);
    fireEvent.keyUp(document.body, { key: 'q', code: 'KeyQ' });
    expect(screen.queryByRole('status')).toBeNull();
    expect(editor().isSliderDragging).toBe(false);
  });

  it("adds up wheel steps that come faster than the drag's store commits", () => {
    // Both events fall in the same commit interval even on a busy host.
    vi.useFakeTimers();
    const stop = registerSliderDraftRenderer(() => true);
    try {
      const { result } = renderHook(() => useQuickAdjust(true));
      const { container } = render(<div className="studio-viewer" />);
      const photo = container.firstElementChild!;
      fireEvent.keyDown(document.body, { key: 'q', code: 'KeyQ' });
      fireEvent.wheel(photo, { deltaY: -100, clientX: 300, clientY: 200 });
      fireEvent.wheel(photo, { deltaY: -100, clientX: 300, clientY: 200 });
      expect(result.current?.value).toBeCloseTo(0.1);
      expect(editor().adjustments.exposure).toBe(0);
      fireEvent.keyUp(document.body, { key: 'q', code: 'KeyQ' });
      expect(editor().adjustments.exposure).toBeCloseTo(0.1);
    } finally {
      stop();
    }
  });

  it("keeps a tapped key's own action and drags change the value", () => {
    workspace();
    const photo = screen.getByTestId('photo');
    // A tap on W still picks the white balance tool.
    press('w', 'KeyW');
    expect(editor().isWbPickerActive).toBe(true);
    press('w', 'KeyW');
    expect(editor().isWbPickerActive).toBe(false);
    // Held W with a drag changes Contrast instead.
    fireEvent.keyDown(document.body, { key: 'w', code: 'KeyW' });
    fireEvent.pointerDown(photo, { button: 0, clientX: 100, clientY: 100 });
    fireEvent.pointerMove(photo, { clientX: 140, clientY: 100 });
    fireEvent.pointerUp(photo, { clientX: 140, clientY: 100 });
    fireEvent.keyUp(document.body, { key: 'w', code: 'KeyW' });
    expect(editor().adjustments.contrast).toBe(10);
    expect(editor().isWbPickerActive).toBe(false);
  });

  it('acts on the selected layer', () => {
    withLinearLayer();
    workspace();
    const photo = screen.getByTestId('photo');
    fireEvent.keyDown(document.body, { key: 'r', code: 'KeyR' });
    fireEvent.wheel(photo, { deltaY: -100 });
    fireEvent.keyUp(document.body, { key: 'r', code: 'KeyR' });
    expect(editor().adjustments.masks[0].adjustments.saturation).toBe(1);
    expect(editor().adjustments.saturation).toBe(0);
  });
});

it('puts the cursor into the Library search field on its shortcut', () => {
  useUIStore.setState({ activeView: 'library' });
  workspace();
  const search = screen.getByRole('searchbox');
  act(() => useUIStore.getState().requestSearchFocus());
  expect(document.activeElement).toBe(search);
});

describe('workspace keyboard accessibility', () => {
  it('anchors keyboard tool settings at the viewer and returns focus on Escape', () => {
    workspace();
    act(() => useUIStore.setState({ activePanel: Panel.Crop }));
    const photo = screen.getByTestId('photo');
    photo.tabIndex = 0;
    photo.focus();
    vi.spyOn(photo, 'getBoundingClientRect').mockReturnValue({ left: 400, bottom: 300 } as DOMRect);
    expect(pressed('Crop')).toBe('true');
    fireEvent.contextMenu(photo, { clientX: 0, clientY: 0 });
    const panel = screen.getByRole('dialog');
    expect(parseFloat(panel.style.left)).toBeGreaterThan(300);
    expect(panel.contains(document.activeElement)).toBe(true);
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(photo);
  });

  it("starts the tool column at 340 px, so sliders are as fine as the reference editors'", () => {
    // UXR1: a 300 px column gave Exposure a 112 px track (0.097 EV/px)
    // against Lightroom's 0.070 (notes: reference/ux-develop-core-2026-10-31).
    localStorage.removeItem('rawtonic.studio.width');
    localStorage.removeItem('retouch.studio.width');
    workspace();
    const separator = screen.getByRole('separator', { name: 'Tool panel width' });
    expect(separator.getAttribute('aria-valuenow')).toBe('340');
  });

  it('moves the old 300 px default to 340 px once, but keeps a width the user chose', () => {
    // The column stored its width from the first start on, so the old
    // default reached everyone as a "stored" 300.
    localStorage.removeItem('rawtonic.studio.widthChosen');
    localStorage.setItem('rawtonic.studio.width', '300');
    workspace();
    const separator = screen.getByRole('separator', { name: 'Tool panel width' });
    expect(separator.getAttribute('aria-valuenow')).toBe('340');
    // Moved back to 300 with the separator, it stays there.
    for (let i = 0; i < 4; i++) fireEvent.keyDown(separator, { key: 'ArrowLeft' });
    expect(separator.getAttribute('aria-valuenow')).toBe('300');
    cleanup();
    workspace();
    expect(screen.getByRole('separator', { name: 'Tool panel width' }).getAttribute('aria-valuenow')).toBe('300');
    // Any other stored width is the user's.
    localStorage.removeItem('rawtonic.studio.widthChosen');
    localStorage.setItem('rawtonic.studio.width', '310');
    cleanup();
    workspace();
    expect(screen.getByRole('separator', { name: 'Tool panel width' }).getAttribute('aria-valuenow')).toBe('310');
    localStorage.removeItem('rawtonic.studio.widthChosen');
    localStorage.removeItem('rawtonic.studio.width');
  });

  it.each(['split-tools', 'tools-left'] as const)('moves the separator in the arrow direction in %s', (layout) => {
    useUIStore.getState().setStudioToolLayout(layout);
    workspace();
    const separator = screen.getByRole('separator', { name: 'Tool panel width' });
    const before = Number(separator.getAttribute('aria-valuenow'));
    fireEvent.keyDown(separator, { key: 'ArrowRight' });
    expect(Number(separator.getAttribute('aria-valuenow'))).toBe(before + (layout === 'split-tools' ? -10 : 10));
    fireEvent.keyDown(separator, { key: 'ArrowLeft' });
    expect(Number(separator.getAttribute('aria-valuenow'))).toBe(before);
  });

  it('opens tool settings without focusing Reset', () => {
    withLinearLayer();
    const brush = createSubMask(Mask.Brush, image);
    useEditorStore.setState((state) => ({
      adjustments: {
        ...state.adjustments,
        masks: state.adjustments.masks.map((mask) => ({ ...mask, subMasks: [brush] }))
      },
      activeMaskId: brush.id
    }));
    workspace();
    fireEvent.contextMenu(screen.getByTestId('photo'), { clientX: 400, clientY: 300 });
    const panel = screen.getByRole('dialog', { name: 'Brush settings' });
    expect(panel.contains(document.activeElement)).toBe(true);
    expect(document.activeElement?.getAttribute('aria-label')).not.toBe('Reset brush');
    expect(document.activeElement?.classList.contains('studio-tool-popover-reset')).toBe(false);
  });

  it.each(['ContextMenu', 'F10'])('opens brush settings from the existing viewer with %s', (key) => {
    withLinearLayer();
    const brush = createSubMask(Mask.Brush, image);
    useEditorStore.setState((state) => ({
      adjustments: {
        ...state.adjustments,
        masks: state.adjustments.masks.map((mask) => ({ ...mask, subMasks: [brush] }))
      },
      activeMaskId: brush.id
    }));
    workspace();
    const viewer = screen.getByRole('group', { name: 'Photo viewer' });
    viewer.focus();
    vi.spyOn(viewer, 'getBoundingClientRect').mockReturnValue({ left: 400, bottom: 300 } as DOMRect);
    fireEvent.keyDown(viewer, { key, shiftKey: key === 'F10' });
    const panel = screen.getByRole('dialog', { name: 'Brush settings' });
    expect(parseFloat(panel.style.left)).toBeGreaterThan(300);
    expect(panel.contains(document.activeElement)).toBe(true);
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(document.activeElement).toBe(viewer);
  });
});

it.each(['windows', 'linux'])('renders platform tooltips on %s', (osPlatform) => {
  useSettingsStore.setState({ osPlatform });
  workspace();
  const titles = Array.from(document.querySelectorAll('[title]'), (node) => node.getAttribute('title'));
  expect(titles.join(' ')).toContain('Ctrl+Z');
  expect(titles.join(' ')).not.toMatch(/[⌘⇧⌥]/);
});
