import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import FaceRetouchOverlay from './FaceRetouchOverlay';
import { useEditorStore } from '../../../store/useEditorStore';
import { INITIAL_ADJUSTMENTS, type Adjustments } from '../../../utils/adjustments';
import { faceRetouchGeometry, reframeProtection } from '../../studio/StudioFaceRetouch';

const face = {
  id: 'face-1',
  crop: [1, 0, 0, 0, 1, 0],
  size: 768,
  eyeDistance: 120,
  skin: '',
  underEyes: null,
  blemishes: [
    { id: 1, x: 100, y: 100, r: 5, score: 0.9 },
    { id: 2, x: 200, y: 100, r: 4, score: 0.03 },
  ],
  kept: [] as number[],
  added: [] as number[],
  settings: {},
  enabled: true,
  box: [50, 50, 300, 300],
};

beforeEach(() => {
  const adjustments: Adjustments = structuredClone(INITIAL_ADJUSTMENTS);
  adjustments.faceRetouch = {
    settings: { blemishes: 30 },
    faces: [face],
    frame: '{}',
    geometry: JSON.stringify(faceRetouchGeometry(adjustments)),
  };
  useEditorStore.setState({
    adjustments,
    selectedImage: { path: '/qa/p.dng', width: 400, height: 300, isRaw: true, isReady: true } as never,
    showFaceBlemishes: true,
    showFaceProtection: false,
    faceProtect: { painting: false, erase: false, size: 30, feather: 50 },
  });
});
afterEach(cleanup);

const mount = () =>
  render(<FaceRetouchOverlay rect={{ left: 0, top: 0, width: 400, height: 300 }} cropX={0} cropY={0} scale={1} zoom={1} />);
const retouched = () => useEditorStore.getState().adjustments.faceRetouch!.faces[0];

describe('face retouch blemishes on the photograph', () => {
  it('keeps a healed blemish and heals a candidate the slider does not reach', () => {
    mount();
    const healed = screen.getByLabelText('Keep blemish 1 of face-1');
    expect(healed.getAttribute('data-state')).toBe('healed');
    expect(screen.getByLabelText('Heal blemish 2 of face-1').getAttribute('data-state')).toBe('candidate');
    fireEvent.click(healed);
    expect(retouched().kept).toEqual([1]);
    fireEvent.click(screen.getByLabelText('Heal blemish 2 of face-1'));
    expect(retouched().added).toEqual([2]);
    // A kept one is healed again.
    fireEvent.click(screen.getByLabelText('Heal blemish 1 of face-1'));
    expect(retouched().kept).toEqual([]);
    expect(retouched().added).toEqual([2]);
  });

  it('shows nothing for faces of another geometry', () => {
    useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, rotation: 3 } }));
    const { container } = mount();
    expect(container.querySelectorAll('circle')).toHaveLength(0);
  });
});

describe('face retouch protection brush', () => {
  const overlay = () => {
    const el = screen.getByLabelText('Face retouch');
    el.getBoundingClientRect = () => ({ left: 0, top: 0, width: 400, height: 300 }) as DOMRect;
    return el;
  };
  const protect = () => useEditorStore.getState().adjustments.faceRetouch!.protect;
  const paint = (el: HTMLElement, from: number, to: number, altKey = false) => {
    fireEvent.pointerDown(el, { button: 0, clientX: from, clientY: 100, pointerId: 1, altKey });
    for (let x = from + 5; x <= to; x += 5) fireEvent.pointerMove(el, { clientX: x, clientY: 100, pointerId: 1, altKey });
    fireEvent.pointerUp(el, { pointerId: 1 });
  };

  it('paints strokes with the brush settings; Alt erases', () => {
    useEditorStore.setState({ faceProtect: { painting: true, erase: false, size: 20, feather: 40 } });
    mount();
    const el = overlay();
    paint(el, 80, 140);
    expect(protect()).toHaveLength(1);
    const [stroke] = protect()!;
    expect(stroke).toMatchObject({ radius: 20, feather: 40 });
    expect(stroke.erase).toBeUndefined();
    expect(stroke.points[0]).toEqual({ x: 80, y: 100 });
    // A point every fifth of the radius.
    expect(stroke.points.at(-1)).toEqual({ x: 140, y: 100 });
    paint(el, 100, 120, true);
    expect(protect()![1].erase).toBe(true);
    // The protected area is tinted while painting.
    expect(screen.getByLabelText('Protected areas')).toBeTruthy();
  });

  it('resizes the brush with [ and ] instead of rotating the photo', () => {
    useEditorStore.setState({ faceProtect: { painting: true, erase: false, size: 20, feather: 40 } });
    mount();
    const rotate = vi.fn();
    window.addEventListener('keydown', rotate);
    fireEvent.keyDown(window, { key: ']' });
    expect(useEditorStore.getState().faceProtect.size).toBe(23);
    fireEvent.keyDown(window, { key: '[' });
    expect(useEditorStore.getState().faceProtect.size).toBe(20);
    expect(rotate).not.toHaveBeenCalled();
    window.removeEventListener('keydown', rotate);
  });

  it('leaves the blemishes to the brush while painting', () => {
    useEditorStore.setState({ faceProtect: { painting: true, erase: false, size: 20, feather: 40 } });
    mount();
    expect((screen.getByLabelText('Keep blemish 1 of face-1') as unknown as SVGElement).style.pointerEvents).toBe('none');
  });

  it('shows the protected areas only when asked, without painting', () => {
    useEditorStore.setState((s) => ({
      showFaceBlemishes: false,
      adjustments: {
        ...s.adjustments,
        faceRetouch: { ...s.adjustments.faceRetouch!, protect: [{ points: [{ x: 10, y: 10 }], radius: 5, feather: 0 }] },
      },
    }));
    mount();
    expect(screen.queryByLabelText('Protected areas')).toBeNull();
    cleanup();
    useEditorStore.setState({ showFaceProtection: true });
    mount();
    expect(screen.getByLabelText('Protected areas')).toBeTruthy();
    expect(document.querySelectorAll('circle')).toHaveLength(0);
  });

  it('keeps strokes on the photo when the faces are found again after a turn', () => {
    const strokes = [{ points: [{ x: 10, y: 20 }], radius: 5, feather: 50 }];
    const before = JSON.stringify({ orientationSteps: 0 });
    const turned = { ...INITIAL_ADJUSTMENTS, orientationSteps: 1 } as Adjustments;
    // 400 × 300 turned a quarter clockwise: (x, y) is shown at (300 − y, x).
    expect(reframeProtection(strokes, before, turned, 400, 300)![0].points[0]).toEqual({ x: 280, y: 10 });
    expect(reframeProtection(strokes, before, { ...INITIAL_ADJUSTMENTS } as Adjustments, 400, 300)).toEqual(strokes);
  });
});
