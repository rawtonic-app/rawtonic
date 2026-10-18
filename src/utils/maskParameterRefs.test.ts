import { beforeEach, describe, expect, it } from 'vitest';
import type { Adjustments } from './adjustments';
import {
  acknowledgeParameters,
  forgetParameterReferences,
  withParameterReferences,
} from './maskParameterRefs';

const stroke = (n: number, x0 = 0) => ({
  tool: 'brush',
  brushSize: 50,
  points: Array.from({ length: n }, (_, i) => ({ x: x0 + i, y: i })),
});

type Line = ReturnType<typeof stroke>;
// Loosely typed recipes: the tests read fields the strict types make optional.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Loose = any;

const recipe = (lines: Line[], extra: object = {}): Loose =>
  ({
    exposure: 0.5,
    masks: [
      {
        id: 'layer',
        adjustments: { exposure: 1 },
        subMasks: [
          { id: 'brush-1', type: 'brush', parameters: { lines } },
          { id: 'radial-1', type: 'radial', parameters: { centerX: 1 } },
        ],
        ...extra,
      },
    ],
  }) as unknown as Adjustments;

describe('withParameterReferences', () => {
  beforeEach(() => forgetParameterReferences());

  it('sends strokes with a token first and as a reference once acknowledged', () => {
    const adjustments = recipe([stroke(80)]);
    const first = withParameterReferences(adjustments);
    expect(first.sent).toHaveLength(1);
    const token = first.sent[0];
    const sentBrush = first.adjustments.masks[0].subMasks[0] as Loose;
    expect(sentBrush.paramsToken).toBe(token);
    expect(sentBrush.parameters).toBe(adjustments.masks[0].subMasks[0].parameters);
    // The recipe itself is untouched.
    expect((adjustments.masks[0].subMasks[0] as Loose).paramsToken).toBeUndefined();

    acknowledgeParameters(first.sent);
    // A slider move: the layer's adjustments change, its strokes do not.
    const moved = { ...adjustments, masks: [{ ...adjustments.masks[0], adjustments: { exposure: 2 } }] };
    const second = withParameterReferences(moved);
    expect(second.sent).toEqual([]);
    expect(second.adjustments.masks[0].subMasks[0].parameters).toEqual({ $paramsRef: token });
    expect(second.adjustments.masks[0].adjustments).toEqual({ exposure: 2 });
    // Other components are passed through as they are.
    expect(second.adjustments.masks[0].subMasks[1]).toBe(adjustments.masks[0].subMasks[1]);
  });

  it('gives changed strokes a new token', () => {
    const adjustments = recipe([stroke(80)]);
    const first = withParameterReferences(adjustments);
    acknowledgeParameters(first.sent);
    const edited = recipe([stroke(80), stroke(10, 500)]);
    const second = withParameterReferences(edited);
    expect(second.sent).toHaveLength(1);
    expect(second.sent[0]).not.toBe(first.sent[0]);
    expect(second.adjustments.masks[0].subMasks[0].parameters).toBe(edited.masks[0].subMasks[0].parameters);
  });

  it('notices a stroke grown in place', () => {
    const adjustments = recipe([stroke(80)]);
    const first = withParameterReferences(adjustments);
    acknowledgeParameters(first.sent);
    adjustments.masks[0].subMasks[0].parameters.lines[0].points.push({ x: 999, y: 1 });
    const second = withParameterReferences(adjustments);
    expect(second.sent).toHaveLength(1);
    expect((second.adjustments.masks[0].subMasks[0] as Loose).paramsToken).toBe(second.sent[0]);
  });

  it('leaves small strokes and recipes without brush layers alone', () => {
    const small = recipe([stroke(5)]);
    expect(withParameterReferences(small)).toEqual({ adjustments: small, sent: [] });
    const none = { exposure: 1 } as Loose;
    expect(withParameterReferences(none).adjustments).toBe(none);
  });

  it('sends everything again after the references are forgotten', () => {
    const adjustments = recipe([stroke(80)]);
    acknowledgeParameters(withParameterReferences(adjustments).sent);
    forgetParameterReferences();
    const again = withParameterReferences(adjustments);
    expect(again.sent).toHaveLength(1);
    expect(again.adjustments.masks[0].subMasks[0].parameters.lines).toHaveLength(1);
  });
});
