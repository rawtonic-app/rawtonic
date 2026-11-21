import '../../i18n';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import { newColorRange, type Adjustments, type ColorRange } from '../../utils/adjustments';
import { useEditorStore } from '../../store/useEditorStore';
import { srgbToOklch } from '../../utils/oklab';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../ui/AppProperties';
import { geometryKeyOf } from '../../utils/colorRanges';
import {
  deltaE00,
  labToLinear,
  linearToLab,
  linearToSrgb8,
  oklabToLinear,
  rangeCentre,
  renderedLab,
  srgb8ToLinear,
  type Vec3
} from '../../utils/colorTarget';
import StudioColorEditor from './StudioColorEditor';
import StudioColorRangeWheel, { chromaOf, radiusOf, rangeBand, wheelPoint } from './StudioColorRangeWheel';

/** The wheel is drawn 224 px wide: one view-box unit (±1.12) is 100 px. */
const WHEEL_PX = 224;
const at = (hue: number, chroma: number) => {
  const p = wheelPoint(hue, chroma);
  return { clientX: (p.x + 1.12) * 100, clientY: (p.y + 1.12) * 100 };
};

let changes: Partial<Adjustments>[] = [];
const interaction = vi.fn();

/** The panel's ranges as the editor's recipe holds them (the app's `change`
 * writes the recipe the renderer measures). */
const storeRanges = (ranges: ColorRange[], layerId?: string) =>
  useEditorStore.setState((s) => ({
    adjustments: layerId
      ? ({ ...s.adjustments, masks: [{ id: layerId, adjustments: { colorRanges: ranges } }] } as never)
      : { ...s.adjustments, colorRanges: ranges }
  }));

function Harness({ initial, layerId, store }: { initial: ColorRange[]; layerId?: string; store?: boolean }) {
  const [values, setValues] = useState<Partial<Adjustments>>({
    colorRanges: initial
  });
  return (
    <StudioColorEditor
      values={values}
      layerId={layerId}
      disabled={false}
      onInteraction={interaction}
      change={(patch) => {
        changes.push(patch);
        setValues((v) => ({ ...v, ...patch }));
        if (store && patch.colorRanges) storeRanges(patch.colorRanges, layerId);
      }}
    />
  );
}

const lastRanges = () => changes.at(-1)!.colorRanges as ColorRange[];

const wheelSvg = () => {
  const svg = screen.getByRole('group', { name: 'Color range wheel' }).querySelector('svg')!;
  vi.spyOn(svg, 'getBoundingClientRect').mockReturnValue({
    left: 0,
    top: 0,
    width: WHEEL_PX,
    height: WHEEL_PX
  } as DOMRect);
  return svg;
};

const openTool = () => {
  if (!screen.queryByRole('tablist')) {
    fireEvent.click(screen.getByRole('button', { name: /Color editor/ }));
  }
};

beforeEach(() => {
  changes = [];
  interaction.mockReset();
  useEditorStore.setState({
    colorPickerTarget: null,
    colorPickerLayer: null,
    activeColorRangeId: null,
    colorRangeShow: null
  });
});
afterEach(cleanup);

const skinish = () => ({
  ...newColorRange(50, 0.06, 0.65),
  hueRange: 25,
  chromaRange: 50,
  smoothness: 50
});

describe('range wheel geometry', () => {
  it('maps chroma to the radius and back', () => {
    for (const c of [0, 0.02, 0.06, 0.15, 0.32]) {
      expect(chromaOf(radiusOf(c))).toBeCloseTo(c, 6);
    }
    // Hue 0 at the right, 90 at the top (screen y down).
    expect(wheelPoint(90, 0.32).y).toBeCloseTo(-1, 6);
  });

  it('builds the band the renderer selects', () => {
    const band = rangeBand(skinish());
    expect(band.hueLo).toBe(25);
    expect(band.hueHi).toBe(75);
    expect(band.chromaLo).toBeCloseTo(0.03, 6);
    expect(band.chromaHi).toBeCloseTo(0.06 * 2 + 0.01, 6);
    // Smoothness 50 → softness 0.55: the hue weight is full within
    // ±25 × 0.45 and zero beyond ±25 × 1.55; the chroma feather is
    // 0.5 × 0.55 × 0.1 + 0.004 on both sides of each chroma edge.
    expect(band.core!.hueLo).toBeCloseTo(50 - 11.25, 6);
    expect(band.core!.hueHi).toBeCloseTo(50 + 11.25, 6);
    expect(band.outer.hueHi).toBeCloseTo(50 + 38.75, 6);
    expect(band.core!.chromaLo).toBeCloseTo(0.03 + 0.0315, 6);
    expect(band.core!.chromaHi).toBeCloseTo(0.13 - 0.0315, 6);
    expect(band.outer.chromaHi).toBeCloseTo(0.13 + 0.0315, 6);
    // A band narrower than its feathers has no full-weight core.
    expect(rangeBand({ ...skinish(), chromaRange: 0 }).core).toBeNull();
    // A grey sample: the band of chroma 0 (0 … 0.01, feather 0.00675),
    // faded by the grey gate, so no full-weight core.
    const grey = rangeBand({ ...skinish(), chroma: 0 });
    expect(grey.outer.chromaHi).toBeCloseTo(0.01675, 6);
    expect(grey.outer.chromaLo).toBe(0.004);
    expect(grey.core).toBeNull();
    // The core starts where the grey gate is full.
    const pale = rangeBand({
      ...skinish(),
      chroma: 0.02,
      chromaRange: 100,
      smoothness: 0
    });
    expect(pale.core!.chromaLo).toBe(0.015);
  });
});

describe('range wheel <-> sliders', () => {
  it('dragging a hue edge sets the Range hue slider', () => {
    const range = skinish();
    render(<Harness initial={[range]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole('slider', {
      name: 'Range hue edge, counter-clockwise'
    });
    expect(edge.getAttribute('aria-valuenow')).toBe('25');
    fireEvent.pointerDown(edge, { button: 0, ...at(75, 0.06) });
    expect(interaction).toHaveBeenLastCalledWith(true);
    fireEvent.pointerMove(edge, at(90, 0.06));
    fireEvent.pointerUp(edge);
    expect(interaction).toHaveBeenLastCalledWith(false);
    expect(lastRanges()[0].hueRange).toBe(40);
    expect(lastRanges()[0].hue).toBe(50);
    // The slider shows the dragged width.
    const slider = screen.getByRole('slider', {
      name: 'Color range hue range'
    });
    expect((slider as HTMLInputElement).value).toBe('40');
  });

  it('dragging the outer saturation edge sets the saturation range', () => {
    render(<Harness initial={[skinish()]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole('slider', {
      name: 'Range outer saturation edge'
    });
    // Outer edge at c(1 + 2r) + 0.01: 0.1 → r = (0.1 − 0.01 − 0.06) / 0.12.
    fireEvent.pointerDown(edge, { button: 0, ...at(50, 0.13) });
    fireEvent.pointerMove(edge, at(50, 0.1));
    fireEvent.pointerUp(edge);
    expect(lastRanges()[0].chromaRange).toBe(25);
    expect(
      (
        screen.getByRole('slider', {
          name: 'Color range saturation range'
        }) as HTMLInputElement
      ).value
    ).toBe('25');
  });

  it("keeps a saturated range's out-of-wheel width until the edge is dragged", () => {
    // Outer edge 0.2 × 2.4 + 0.01 = 0.49: beyond the rim, so the handle sits
    // on the rim; pressing and moving it there must not change the range.
    const range = { ...newColorRange(30, 0.2, 0.6), chromaRange: 70 };
    render(<Harness initial={[range]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole('slider', {
      name: 'Range outer saturation edge'
    });
    fireEvent.pointerDown(edge, { button: 0, ...at(30, 0.32) });
    fireEvent.pointerMove(edge, at(30, 0.32));
    fireEvent.pointerUp(edge);
    expect(changes).toHaveLength(0);
    // Dragged inwards it narrows from where it was, by the pointer's way.
    fireEvent.pointerDown(edge, { button: 0, ...at(30, 0.32) });
    fireEvent.pointerMove(edge, at(30, 0.26));
    fireEvent.pointerUp(edge);
    const width = lastRanges()[0].chromaRange;
    // Edge 0.49 − 0.06 = 0.43 → (0.43 − 0.01 − 0.2) / 0.4 = 55.
    expect(width).toBe(55);
  });

  it('draws the edge handles over the colour handle, so they stay reachable', () => {
    // Saturated blue: the outer edge clips to the rim right beside the
    // colour; SVG paints later elements on top, so the edges come last.
    render(<Harness initial={[newColorRange(264, 0.313, 0.45)]} />);
    openTool();
    const colour = screen.getByRole('slider', { name: 'Range color' });
    for (const name of ['Range outer saturation edge', 'Range inner saturation edge', 'Range hue edge, clockwise']) {
      const edge = screen.getByRole('slider', { name });
      expect(colour.compareDocumentPosition(edge) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    }
  });

  it('selecting another range mid-drag ends the drag without editing either', () => {
    const a = skinish();
    const b = { ...newColorRange(220, 0.08, 0.5), hueRange: 25 };
    render(<Harness initial={[a, b]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole('slider', {
      name: 'Range hue edge, counter-clockwise'
    });
    fireEvent.pointerDown(edge, { button: 0, ...at(75, 0.06) });
    fireEvent.pointerMove(edge, at(90, 0.06));
    act(() => useEditorStore.setState({ activeColorRangeId: b.id }));
    expect(interaction).toHaveBeenLastCalledWith(false);
    fireEvent.pointerMove(edge, at(100, 0.06));
    fireEvent.pointerUp(edge);
    // Nothing was written: the pending frame of range A was dropped.
    expect(changes).toHaveLength(0);
    expect(
      screen.getByRole('slider', { name: 'Range hue edge, counter-clockwise' }).getAttribute('aria-valuenow')
    ).toBe('25');
  });

  it('another photograph mid-drag ends the drag (copied ranges share ids)', () => {
    useEditorStore.setState({
      selectedImage: { path: '/a.cr3' } as never
    });
    render(<Harness initial={[skinish()]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole('slider', {
      name: 'Range hue edge, counter-clockwise'
    });
    fireEvent.pointerDown(edge, { button: 0, ...at(75, 0.06) });
    fireEvent.pointerMove(edge, at(90, 0.06));
    act(() => useEditorStore.setState({ selectedImage: { path: '/b.cr3' } as never }));
    fireEvent.pointerMove(edge, at(100, 0.06));
    fireEvent.pointerUp(edge);
    expect(changes).toHaveLength(0);
    useEditorStore.setState({ selectedImage: null });
  });

  it('a near-grey range keeps its width on a press without movement', () => {
    const range = { ...newColorRange(50, 0.004, 0.6), chromaRange: 50 };
    render(<Harness initial={[range]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole('slider', {
      name: 'Range outer saturation edge'
    });
    fireEvent.pointerDown(edge, { button: 0, ...at(50, 0.018) });
    fireEvent.pointerMove(edge, at(50, 0.018));
    fireEvent.pointerUp(edge);
    expect(changes).toHaveLength(0);
  });

  it("disabled mid-drag shows the recipe's range again", () => {
    const range = skinish();
    const ranges = [range];
    const props = {
      ranges,
      active: range,
      onChange: vi.fn(),
      onInteraction: vi.fn()
    };
    const { rerender } = render(<StudioColorRangeWheel {...props} />);
    wheelSvg();
    const edge = () => screen.getByRole('slider', { name: 'Range hue edge, counter-clockwise' });
    fireEvent.pointerDown(edge(), { button: 0, ...at(75, 0.06) });
    fireEvent.pointerMove(edge(), at(90, 0.06));
    expect(edge().getAttribute('aria-valuenow')).toBe('40');
    rerender(<StudioColorRangeWheel {...props} disabled />);
    expect(props.onInteraction).toHaveBeenLastCalledWith(false);
    rerender(<StudioColorRangeWheel {...props} />);
    expect(edge().getAttribute('aria-valuenow')).toBe('25');
    expect(props.onChange).not.toHaveBeenCalled();
  });

  it('follows the sliders: a new hue range moves the edge handles', () => {
    const range = skinish();
    const { rerender } = render(<Harness initial={[range]} />);
    openTool();
    const edge = () => screen.getByRole('slider', { name: 'Range hue edge, clockwise' });
    const before = Number(edge().getAttribute('cy'));
    const slider = screen.getByRole('slider', {
      name: 'Color range hue range'
    });
    fireEvent.keyDown(slider, { key: 'ArrowRight' });
    rerender(<Harness initial={[range]} />);
    expect(lastRanges()[0].hueRange).toBe(30);
    expect(edge().getAttribute('aria-valuenow')).toBe('30');
    // The clockwise edge (hue 50 − 30) sits lower on the wheel than before.
    expect(Number(edge().getAttribute('cy'))).toBeGreaterThan(before);
  });

  it('steps a handle with the arrow keys', () => {
    render(<Harness initial={[skinish()]} />);
    openTool();
    const edge = screen.getByRole('slider', {
      name: 'Range inner saturation edge'
    });
    fireEvent.keyDown(edge, { key: 'ArrowUp', shiftKey: true });
    expect(lastRanges()[0].chromaRange).toBe(60);
    const colour = screen.getByRole('slider', { name: 'Range color' });
    fireEvent.keyDown(colour, { key: 'ArrowLeft' });
    expect(lastRanges()[0].hue).toBe(49);
  });

  it('a press beside the handles moves the range to that colour', () => {
    render(<Harness initial={[skinish()]} />);
    openTool();
    const svg = wheelSvg();
    fireEvent.pointerDown(svg, { button: 0, ...at(200, 0.1) });
    fireEvent.pointerUp(svg);
    const moved = lastRanges()[0];
    expect(moved.hue).toBeCloseTo(200, 0);
    expect(moved.chroma).toBeCloseTo(0.1, 2);
    // Lightness and shape stay.
    expect(moved.lightness).toBe(0.65);
    expect(moved.hueRange).toBe(25);
  });

  it('a pick armed for a range ends with a press on the wheel', () => {
    render(<Harness initial={[skinish()]} />);
    openTool();
    fireEvent.click(screen.getByRole('button', { name: 'Pick the range color again' }));
    const svg = wheelSvg();
    fireEvent.pointerDown(svg, { button: 0, ...at(120, 0.1) });
    fireEvent.pointerUp(svg);
    expect(lastRanges()).toHaveLength(1);
    expect(lastRanges()[0].hue).toBeCloseTo(120, 0);
    expect(lastRanges()[0].lightness).toBe(0.65);
    expect(useEditorStore.getState().colorPickerTarget).toBeNull();
  });

  it('re-picks a range on the wheel at the range limit', () => {
    const full = Array.from({ length: 8 }, (_, i) => ({
      ...newColorRange(i * 45, 0.08, 0.6),
      hueRange: 10
    }));
    render(<Harness initial={full} />);
    openTool();
    fireEvent.click(screen.getByRole('button', { name: 'Pick the range color again' }));
    const group = screen.getByRole('group', { name: 'Color range wheel' });
    expect(group.className).not.toContain('is-disabled');
    const svg = wheelSvg();
    fireEvent.pointerDown(svg, { button: 0, ...at(20, 0.1) });
    expect(lastRanges()).toHaveLength(8);
    expect(lastRanges()[0].hue).toBeCloseTo(20, 0);
    expect(useEditorStore.getState().colorPickerTarget).toBeNull();
    // Adding a ninth stays impossible.
    expect(
      (
        screen.getByRole('button', {
          name: 'Pick a color range'
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true);
  });

  it('with the picker armed, a press on the wheel adds a range', () => {
    render(<Harness initial={[skinish()]} />);
    openTool();
    fireEvent.click(screen.getByRole('button', { name: 'Pick a color range' }));
    expect(useEditorStore.getState().colorPickerTarget).toBe('new');
    const svg = wheelSvg();
    fireEvent.pointerDown(svg, { button: 0, ...at(140, 0.12) });
    expect(lastRanges()).toHaveLength(2);
    expect(lastRanges()[1].hue).toBeCloseTo(140, 0);
    expect(useEditorStore.getState().colorPickerTarget).toBeNull();
    expect(useEditorStore.getState().activeColorRangeId).toBe(lastRanges()[1].id);
  });
});

describe('skin tone mode', () => {
  it('picks skin on the wheel into a range of its own', () => {
    const other = newColorRange(240, 0.1, 0.5);
    render(<Harness initial={[other]} />);
    openTool();
    fireEvent.click(screen.getByRole('tab', { name: 'Skin tone' }));
    expect(screen.getByText(/Pick skin in the photo/)).toBeTruthy();
    // No skin range yet: the wheel shows none and a press picks skin.
    expect(screen.queryByRole('slider', { name: 'Range color' })).toBeNull();
    const svg = wheelSvg();
    act(() => {
      fireEvent.pointerDown(svg, { button: 0, ...at(55, 0.07) });
    });
    const ranges = lastRanges();
    expect(ranges).toHaveLength(2);
    const skin = ranges.find((r) => r.kind === 'skin')!;
    expect(skin.hue).toBeCloseTo(55, 0);
    expect(useEditorStore.getState().activeColorRangeId).toBe(skin.id);
    // Uniformity comes first, the shifts under Amount after it.
    const names = screen.getAllByRole('slider').map((s) => s.getAttribute('aria-label'));
    expect(names.indexOf('Color range hue uniformity')).toBeLessThan(names.indexOf('Color range hue'));
    expect(screen.getByText('Amount')).toBeTruthy();
    fireEvent.keyDown(screen.getByRole('slider', { name: 'Color range saturation uniformity' }), { key: 'ArrowRight' });
    expect(lastRanges().find((r) => r.kind === 'skin')!.saturationUniformity).toBe(5);
    expect(lastRanges().find((r) => r.id === other.id)).toEqual(other);
  });

  it('arms the photo picker for skin and keeps skin out of the range list', () => {
    const skin = { ...newColorRange(50, 0.06, 0.6), kind: 'skin' as const };
    const other = newColorRange(240, 0.1, 0.5);
    render(<Harness initial={[skin, other]} />);
    openTool();
    // Ranges mode lists the other range only.
    expect(screen.getAllByRole('button', { name: /^Color range \d+°$/ })).toHaveLength(1);
    fireEvent.click(screen.getByRole('tab', { name: 'Skin tone' }));
    expect(useEditorStore.getState().activeColorRangeId).toBe(skin.id);
    fireEvent.click(screen.getByRole('button', { name: 'Pick the range color again' }));
    expect(useEditorStore.getState().colorPickerTarget).toBe(skin.id);
    fireEvent.click(screen.getByRole('tab', { name: 'Ranges' }));
    expect(useEditorStore.getState().colorPickerTarget).toBeNull();
  });

  it('offers the skin picker when there is no skin range', () => {
    render(<Harness initial={[]} />);
    openTool();
    fireEvent.click(screen.getByRole('tab', { name: 'Skin tone' }));
    fireEvent.click(screen.getByRole('button', { name: 'Pick the skin tone' }));
    expect(useEditorStore.getState().colorPickerTarget).toBe('skin');
    expect(screen.getByText('Click skin in the photo.')).toBeTruthy();
  });
});

describe('target colour', () => {
  const photo = { path: '/a.cr3', isReady: true } as never;
  const red = () => {
    const { hue, chroma, lightness } = srgbToOklch([200, 60, 50]);
    return newColorRange(hue, chroma, lightness);
  };
  beforeEach(() => useEditorStore.setState({ selectedImage: photo, colorTarget: null, colorPicksThroughRanges: [] }));

  const typeInto = (name: string, text: string) => {
    const field = screen.getByRole('textbox', { name });
    fireEvent.focus(field);
    fireEvent.change(field, { target: { value: text } });
    fireEvent.keyDown(field, { key: 'Enter' });
  };

  it('a typed hex target moves the range in one edit and, without a pick, says it is predicted', () => {
    const range = red();
    render(<Harness initial={[range]} />);
    openTool();
    typeInto('Target color as sRGB hex', '#c81e2d');
    expect(changes).toHaveLength(1);
    const [moved] = lastRanges();
    expect(moved.hueShift).not.toBe(0);
    expect({ ...moved, hueShift: 0, saturation: 0, lightnessShift: 0 }).toEqual(range);
    // Not picked in the photo: nothing to measure, so never "reached".
    expect(screen.getByRole('status').textContent).toMatch(
      /^Predicted ΔE00 0\.\d from the target for the range's color, not measured/
    );
    // The fields now show the target.
    expect((screen.getByRole('textbox', { name: 'Target color as sRGB hex' }) as HTMLInputElement).value).toBe(
      '#C81E2D'
    );
    // An invalid entry changes nothing and the field shows the target again.
    typeInto('Target color as sRGB hex', '#12');
    expect(changes).toHaveLength(1);
  });

  it('says which shift ran out for a target beyond reach', () => {
    render(<Harness initial={[red()]} />);
    openTool();
    typeInto('Target hue h° (CIELAB D50)', '250');
    expect(changes).toHaveLength(1);
    const [moved] = lastRanges();
    expect(moved.hueShift).toBe(-100);
    // Every slider the result leaves at its end is named.
    expect(Math.abs(moved.saturation)).toBe(100);
    expect(screen.getByRole('status').textContent).toMatch(
      /^Predicted out of reach: ΔE00 [\d.]+ left; hue \(±50°\), saturation \(0–200 %\) at its limit/
    );
  });

  /** A pick of `range` in the photo (or layer L1) and a renderer for it: the
   * input probe returns `input`, the result probe renders the recipe's ranges
   * by the model plus `offset` in Lab (what the model leaves out: texture,
   * later tools), as sRGB display values. */
  const pickedRenderer = (range: ColorRange, influence: number, offset: Vec3, layer?: string) => {
    const centre = rangeCentre(range);
    const input = { pixels: [centre], influence: [influence] };
    useEditorStore.setState({
      colorRangeInputs: {
        [`/a.cr3|${layer ?? ''}|${range.id}`]: {
          point: [0.3, 0.7],
          centre,
          geometry: geometryKeyOf(useEditorStore.getState().adjustments),
          input
        }
      }
    });
    storeRanges([range], layer);
    const results: Vec3[] = [];
    const render = (recipe: Adjustments) => {
      const ranges = (layer ? recipe.masks[0].adjustments.colorRanges : recipe.colorRanges) as ColorRange[];
      const lab = renderedLab(ranges, input).map((v, k) => v + offset[k]) as Vec3;
      results.push(lab);
      // An sRGB output: display values clip at 0 and 255.
      return [linearToSrgb8(labToLinear(lab)).map((v) => Math.min(255, Math.max(0, v)))];
    };
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === Invokes.SampleColorRangeInput) return { pixels: [oklabToLinear(centre)], influence: [influence] };
      if (command === Invokes.SampleColorRangeResult) return render((args as { adjustments: Adjustments }).adjustments);
      return null;
    });
    return { input, results };
  };
  const cleanStore = () =>
    useEditorStore.setState({ colorRangeInputs: {}, colorTargetMeasure: null, isSliderDragging: false });

  it('corrects a typed target on the measured result at the pick and states that measurement', async () => {
    const range = red();
    // Picked on a feathered layer (half strength); the render lands 3 ΔE off the model.
    const { input, results } = pickedRenderer(range, 0.5, [2.5, -1.5, 1], 'L1');
    const dragging: boolean[] = [];
    const unsubscribe = useEditorStore.subscribe((s, p) => {
      if (s.isSliderDragging !== p.isSliderDragging) dragging.push(s.isSliderDragging);
    });
    render(<Harness initial={[range]} layerId="L1" store />);
    openTool();
    typeInto('Target color as sRGB hex', '#c81e2d');
    await waitFor(() => expect(screen.getByRole('status').textContent).toMatch(/measured at the picked point/));
    unsubscribe();
    expect(invoke).toHaveBeenCalledWith(
      Invokes.SampleColorRangeInput,
      expect.objectContaining({ path: '/a.cr3', normalizedX: 0.3, normalizedY: 0.7, layer: 'L1' })
    );
    expect(invoke).toHaveBeenCalledWith(
      Invokes.SampleColorRangeResult,
      expect.objectContaining({ path: '/a.cr3', normalizedX: 0.3, normalizedY: 0.7 })
    );
    const lab = linearToLab(srgb8ToLinear([0xc8, 0x1e, 0x2d]));
    const [moved] = lastRanges();
    // The model alone would leave the offset; the corrected sliders measure close.
    const measured = renderedLab([moved], input).map((v, k) => v + [2.5, -1.5, 1][k]) as Vec3;
    expect(deltaE00(lab, measured)).toBeLessThan(1);
    expect(deltaE00(lab, results[0])).toBeGreaterThan(2);
    // The readout is the measurement (8-bit display values), not the model.
    const value = Number(screen.getByRole('status').textContent!.match(/ΔE00 ([\d.]+)/)![1]);
    expect(Math.abs(value - deltaE00(lab, measured))).toBeLessThan(0.15);
    expect(screen.getByRole('status').textContent).toMatch(
      /^Target reached: ΔE00 0\.\d, measured at the picked point\./
    );
    // The model's solve and the correction are one interaction: one undo step.
    expect(dragging).toEqual([true, false]);
    cleanStore();
  });

  it('says the target is out of the sRGB output with the measured distance', async () => {
    const range = red();
    // The output clips the colour into sRGB.
    pickedRenderer(range, 1, [0, 0, 0]);
    render(<Harness initial={[range]} store />);
    openTool();
    typeInto('Target chroma C* (CIELAB D50)', '130');
    await waitFor(() => expect(screen.getByRole('status').textContent).toMatch(/measured/));
    expect(screen.getByRole('status').textContent).toMatch(
      /^The target is outside sRGB: the sRGB result at the picked point stays ΔE00 [\d.]+ from it \(measured\)\./
    );
    cleanStore();
  });

  it('re-measures after a slider move and never shows the model meanwhile', async () => {
    const range = red();
    pickedRenderer(range, 1, [3, 0, 0]);
    render(<Harness initial={[range]} store />);
    openTool();
    typeInto('Target color as sRGB hex', '#c81e2d');
    await waitFor(() => expect(screen.getByRole('status').textContent).toMatch(/^Target reached: .*measured/));
    // Another edit of the recipe: until it is measured the readout says so.
    act(() => storeRanges([{ ...lastRanges()[0], lightnessShift: lastRanges()[0].lightnessShift + 30 }]));
    expect(screen.getByRole('status').textContent).toBe('Measuring the result at the picked point…');
    await waitFor(() =>
      expect(screen.getByRole('status').textContent).toMatch(/^ΔE00 [\d.]+ from the target, measured/)
    );
    cleanStore();
  });

  it('applies only the latest typed target when samples return in reverse order', async () => {
    const range = red();
    pickedRenderer(range, 1, [0, 0, 0]);
    const render0 = vi.mocked(invoke).getMockImplementation()!;
    const answers: (() => void)[] = [];
    vi.mocked(invoke).mockImplementation((command, args) =>
      command === Invokes.SampleColorRangeInput
        ? new Promise((resolve) => answers.push(() => resolve(render0(command, args))))
        : render0(command, args)
    );
    render(<Harness initial={[range]} store />);
    openTool();
    typeInto('Target color as sRGB hex', '#c81e2d');
    typeInto('Target color as sRGB hex', '#b4283c');
    await waitFor(() => expect(answers.length).toBeGreaterThanOrEqual(2));
    // The later request's sample first, then the earlier one's.
    answers[1]();
    await waitFor(() => expect(screen.getByRole('status').textContent).toMatch(/measured/));
    const settled = changes.length;
    answers[0]();
    await new Promise((r) => setTimeout(r, 20));
    expect(changes).toHaveLength(settled);
    const lab = linearToLab(srgb8ToLinear([0xb4, 0x28, 0x3c]));
    expect(deltaE00(lab, renderedLab(lastRanges(), { pixels: [rangeCentre(range)], influence: [1] }))).toBeLessThan(1);
    expect(useEditorStore.getState().isSliderDragging).toBe(false);
    cleanStore();
  });

  it('redraws each valid typed value after a pause, as one interaction committed on Return', () => {
    vi.useFakeTimers();
    try {
      render(<Harness initial={[red()]} />);
      openTool();
      const field = screen.getByRole('textbox', { name: 'Target red (sRGB 0–255)' }) as HTMLInputElement;
      field.focus();
      // Typed digit by digit: each value redraws once typing pauses 50 ms.
      fireEvent.change(field, { target: { value: '1' } });
      act(() => vi.advanceTimersByTime(20));
      fireEvent.change(field, { target: { value: '16' } });
      act(() => vi.advanceTimersByTime(49));
      expect(changes).toHaveLength(0);
      act(() => vi.advanceTimersByTime(1));
      expect(changes).toHaveLength(1);
      expect(useEditorStore.getState().isSliderDragging).toBe(true);
      fireEvent.change(field, { target: { value: '161' } });
      act(() => vi.advanceTimersByTime(50));
      expect(changes).toHaveLength(2);
      // An invalid value redraws nothing.
      fireEvent.change(field, { target: { value: '1610' } });
      act(() => vi.advanceTimersByTime(50));
      expect(changes).toHaveLength(2);
      fireEvent.change(field, { target: { value: '161' } });
      fireEvent.keyDown(field, { key: 'Enter' });
      // Return commits (without a pick, the model's solve) and ends the interaction.
      expect(changes).toHaveLength(3);
      expect(useEditorStore.getState().isSliderDragging).toBe(false);
      // A hex redraws only once all six digits are typed.
      const hex = screen.getByRole('textbox', { name: 'Target color as sRGB hex' });
      hex.focus();
      fireEvent.change(hex, { target: { value: '#70' } });
      act(() => vi.advanceTimersByTime(50));
      fireEvent.change(hex, { target: { value: '#709' } });
      act(() => vi.advanceTimersByTime(50));
      expect(changes).toHaveLength(3);
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps Return and Escape from the app shortcuts once the field lets go of the focus', () => {
    render(<Harness initial={[red()]} />);
    openTool();
    const seen: boolean[] = [];
    const listen = (e: KeyboardEvent) => seen.push(e.defaultPrevented);
    window.addEventListener('keydown', listen);
    try {
      const field = screen.getByRole('textbox', { name: 'Target color as sRGB hex' });
      for (const key of ['Escape', 'Enter']) {
        field.focus();
        fireEvent.change(field, { target: { value: '#c81e2d' } });
        fireEvent.keyDown(field, { key });
      }
    } finally {
      window.removeEventListener('keydown', listen);
    }
    expect(seen).toEqual([true, true]);
  });

  it('Escape after typed redraws restores the recipe and the target as they were', () => {
    vi.useFakeTimers();
    try {
      const range = red();
      storeRanges([range]);
      const before = useEditorStore.getState().adjustments;
      render(<Harness initial={[range]} />);
      openTool();
      const field = screen.getByRole('textbox', { name: 'Target color as sRGB hex' }) as HTMLInputElement;
      field.focus();
      fireEvent.change(field, { target: { value: '#c81e2d' } });
      act(() => vi.advanceTimersByTime(50));
      expect(changes).toHaveLength(1);
      expect(useEditorStore.getState().colorTarget).not.toBeNull();
      fireEvent.keyDown(field, { key: 'Escape' });
      expect(useEditorStore.getState()).toMatchObject({
        adjustments: before,
        colorTarget: null,
        isSliderDragging: false
      });
      expect(useEditorStore.getState().historyPending).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('flags a typed target outside sRGB', () => {
    render(<Harness initial={[red()]} />);
    openTool();
    typeInto('Target chroma C* (CIELAB D50)', '130');
    expect(screen.getByRole('status').textContent).toContain(
      'The target is outside sRGB; an sRGB export maps the color into sRGB.'
    );
  });

  it('Escape restores the field without applying; Return applies and leaves the field', () => {
    render(<Harness initial={[red()]} />);
    openTool();
    const field = screen.getByRole('textbox', { name: 'Target color as sRGB hex' }) as HTMLInputElement;
    const shown = field.value;
    field.focus();
    fireEvent.change(field, { target: { value: '#c81e2d' } });
    fireEvent.keyDown(field, { key: 'Escape' });
    expect(changes).toHaveLength(0);
    expect(field.value).toBe(shown);
    expect(document.activeElement).not.toBe(field);
    field.focus();
    fireEvent.change(field, { target: { value: '#c81e2d' } });
    fireEvent.keyDown(field, { key: 'Enter' });
    expect(changes).toHaveLength(1);
    expect(document.activeElement).not.toBe(field);
  });

  it('selecting another range ends a pick armed for the first', () => {
    const a = red();
    const b = newColorRange(240, 0.1, 0.5);
    render(<Harness initial={[a, b]} />);
    openTool();
    fireEvent.click(screen.getByRole('button', { name: 'Pick the target color' }));
    expect(useEditorStore.getState().colorPickerTarget).toBe(`target:${a.id}`);
    fireEvent.click(screen.getAllByRole('button', { name: /^Color range \d+°$/ })[1]);
    expect(useEditorStore.getState().activeColorRangeId).toBe(b.id);
    expect(useEditorStore.getState().colorPickerTarget).toBeNull();
    // A pick that adds a range stays armed.
    fireEvent.click(screen.getByRole('button', { name: 'Pick a color range' }));
    fireEvent.click(screen.getAllByRole('button', { name: /^Color range \d+°$/ })[0]);
    expect(useEditorStore.getState().colorPickerTarget).toBe('new');
  });

  it('a layer range is predicted at full layer strength, never claimed as reached', () => {
    render(<Harness initial={[red()]} layerId="L1" />);
    openTool();
    typeInto('Target color as sRGB hex', '#c81e2d');
    const text = screen.getByRole('status').textContent!;
    expect(text).toMatch(/^At full layer strength the range lands ΔE00 0\.\d from the target/);
    expect(text).not.toContain('Target reached');
  });

  it('does not claim a target reached for a range picked through its own strong change', () => {
    const range = red();
    useEditorStore.setState({ colorPicksThroughRanges: [`/a.cr3||${range.id}`] });
    render(<Harness initial={[range]} />);
    openTool();
    typeInto('Target color as sRGB hex', '#c81e2d');
    expect(screen.getByRole('status').textContent).toMatch(
      /^Predicted ΔE00 0\.\d from the target, but this color was picked through/
    );
  });

  it('a pick armed for a layer ends when the tool turns to another layer', () => {
    const range = red();
    const { rerender } = render(<Harness initial={[range]} layerId="L1" />);
    openTool();
    fireEvent.click(screen.getByRole('button', { name: 'Pick the target color' }));
    expect(useEditorStore.getState()).toMatchObject({
      colorPickerTarget: `target:${range.id}`,
      colorPickerLayer: 'L1'
    });
    rerender(<Harness initial={[range]} layerId="L2" />);
    expect(useEditorStore.getState()).toMatchObject({ colorPickerTarget: null, colorPickerLayer: null });
    // Sampling a target edits nothing and stays armed.
    fireEvent.click(screen.getByRole('button', { name: 'Sample a target color' }));
    rerender(<Harness initial={[range]} layerId="L1" />);
    expect(useEditorStore.getState().colorPickerTarget).toBe('target');
  });

  it('arms the target pickers', () => {
    const range = red();
    render(<Harness initial={[range]} />);
    openTool();
    fireEvent.click(screen.getByRole('button', { name: 'Pick the target color' }));
    expect(useEditorStore.getState().colorPickerTarget).toBe(`target:${range.id}`);
    fireEvent.click(screen.getByRole('button', { name: 'Sample a target color' }));
    expect(useEditorStore.getState().colorPickerTarget).toBe('target');
    expect(changes).toHaveLength(0);
  });

  it('uses a target sampled in another photo on a click', () => {
    const range = red();
    const lab = [45, 62, 38] as [number, number, number];
    useEditorStore.setState({ colorTarget: { lab, sampledIn: '/b/ref.nef', appliedTo: null } });
    render(<Harness initial={[range]} />);
    openTool();
    expect(screen.queryByRole('status')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: /Use target from ref\.nef/ }));
    expect(changes).toHaveLength(1);
    expect(useEditorStore.getState().colorTarget).toEqual({
      lab,
      sampledIn: '/b/ref.nef',
      appliedTo: { photo: '/a.cr3', layer: null, rangeId: range.id }
    });
    expect(screen.queryByRole('button', { name: /Use target from/ })).toBeNull();
    expect(screen.getByRole('status')).toBeTruthy();
  });
});
