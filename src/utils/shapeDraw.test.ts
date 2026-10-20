import { describe, expect, it } from 'vitest';
import { Mask } from '../components/panel/right/Masks';
import { createSubMask } from './maskUtils';
import { SHAPE_DRAG_THRESHOLD, committedShapeLayer, draftToCommit, shapeFromDrag } from './shapeDraw';
import { linearFrame, linearLineOffsets, type LinearParams } from './linearMask';

const image = { width: 6000, height: 4000 };
const opts = { scale: 0.25, handle: 800 };

describe('drawing a new gradient by a drag', () => {
  it('is a click below the threshold in screen pixels', () => {
    const { parameters } = createSubMask(Mask.Radial, image);
    const short = (SHAPE_DRAG_THRESHOLD - 1) / opts.scale;
    expect(shapeFromDrag(Mask.Radial, parameters, { x: 100, y: 100 }, { x: 100 + short, y: 100 }, opts)).toBeNull();
  });

  it('draws a radial from its centre out to the pointer', () => {
    const { parameters } = createSubMask(Mask.Radial, image);
    const drawn = shapeFromDrag(Mask.Radial, { ...parameters, rotation: 30 }, { x: 1000, y: 800 }, { x: 1600, y: 500 }, opts);
    expect(drawn).toMatchObject({ centerX: 1000, centerY: 800, radiusX: 600, radiusY: 300, rotation: 0, feather: 0.5 });
  });

  it('draws a linear from its 100% point at the press to its 0% point at the pointer', () => {
    const { parameters } = createSubMask(Mask.Linear, image);
    const drawn = shapeFromDrag(Mask.Linear, parameters, { x: 3000, y: 500 }, { x: 3000, y: 1500 }, opts) as LinearParams;
    const { normal } = linearFrame(drawn);
    const offsets = linearLineOffsets(drawn);
    const middle = { x: (drawn.startX + drawn.endX) / 2, y: (drawn.startY + drawn.endY) / 2 };
    // The central line lies halfway, across the drag; the transition spans it.
    expect(middle.x).toBeCloseTo(3000);
    expect(middle.y).toBeCloseTo(1000);
    expect(middle.y + normal.y * offsets.start).toBeCloseTo(500);
    expect(middle.y + normal.y * offsets.end).toBeCloseTo(1500);
  });
});

describe('releasing a gradient drag', () => {
  it('commits the draft only to the component the drag started on', () => {
    const radial = createSubMask(Mask.Radial, image);
    const linear = createSubMask(Mask.Linear, image);
    const draft = { id: radial.id, params: { ...radial.parameters, centerX: 10 } };
    expect(draftToCommit(draft, radial.id)).toEqual(draft.params);
    // L pressed mid-drag made a new linear active: its shape stays its own.
    expect(draftToCommit(draft, linear.id)).toBeNull();
    expect(draftToCommit(null, radial.id)).toBeNull();
  });

  it('measures the click threshold in screen pixels, zoom included', () => {
    const { parameters } = createSubMask(Mask.Radial, image);
    // `scale` is screen pixels per image pixel (fit 0.25 at 4x zoom = 1).
    expect(shapeFromDrag(Mask.Radial, parameters, { x: 0, y: 0 }, { x: 10, y: 0 }, { scale: 1, handle: 1 })).toBeNull();
    expect(shapeFromDrag(Mask.Radial, parameters, { x: 0, y: 0 }, { x: 16, y: 0 }, { scale: 1, handle: 1 })).not.toBeNull();
  });
});

describe('a cancelled gradient drag', () => {
  it('shows the layer of the drawn component as the recipe has it', () => {
    const radial = createSubMask(Mask.Radial, image);
    const brush = createSubMask(Mask.Brush, image);
    const layers = [
      { id: 'a', subMasks: [brush] },
      { id: 'b', subMasks: [brush, radial] },
    ];
    expect(committedShapeLayer(layers, radial.id)).toBe(layers[1]);
    expect(committedShapeLayer(layers, 'gone')).toBeNull();
  });
});

describe('a gradient drag back towards its press', () => {
  it('keeps following the pointer once it has drawn', () => {
    const { parameters } = createSubMask(Mask.Radial, image);
    const from = { x: 0, y: 0 };
    const opts = { scale: 1, handle: 1 };
    // Out 100 px: drawing starts; back to 5 px: the shape shrinks with it
    // instead of keeping the 100 px draft (the release commits what is shown).
    const out = shapeFromDrag(Mask.Radial, parameters, from, { x: 100, y: 0 }, opts);
    expect(out).toMatchObject({ radiusX: 100 });
    const back = shapeFromDrag(Mask.Radial, parameters, from, { x: 5, y: 0 }, { ...opts, drawing: true });
    expect(back).toMatchObject({ radiusX: 5 });
  });
});
