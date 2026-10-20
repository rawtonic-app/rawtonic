import { describe, expect, it } from 'vitest';
import { Mask } from '../components/panel/right/Masks';
import { createSubMask } from './maskUtils';
import { SHAPE_DRAG_THRESHOLD, shapeFromDrag } from './shapeDraw';
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
