import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import FaceRetouchOverlay from './FaceRetouchOverlay';
import { useEditorStore } from '../../../store/useEditorStore';
import { INITIAL_ADJUSTMENTS, type Adjustments } from '../../../utils/adjustments';
import { faceRetouchGeometry } from '../../studio/StudioFaceRetouch';

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
