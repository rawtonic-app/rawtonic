import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import CropFrame from './CropFrame';
import { createCropSelection } from './cropSelection';

afterEach(cleanup);

it('follows the crop selection by itself and shows the size while dragged', () => {
  const selection = createCropSelection();
  selection.setState({ crop: { unit: '%', x: 10, y: 10, width: 50, height: 50 } });
  const noop = () => {};
  const { container } = render(
    <CropFrame
      cropSelection={selection}
      aspect={undefined}
      onChange={noop}
      onComplete={noop}
      onDragStart={noop}
      onDragEnd={noop}
      shade={0.5}
      boxWidth={400}
      boxHeight={300}
      photoWidth={6000}
      photoHeight={4000}
      overlayMode="none"
      overlayRotation={0}
      denseGrid={false}
      dragging
    >
      <img alt="" />
    </CropFrame>
  );
  const frame = () => container.querySelector<HTMLElement>('.ReactCrop__crop-selection')!;
  expect(frame().style.left).toBe('10%');
  expect(container.querySelector('.crop-size-label')?.textContent).toBe('3000 × 2000 px');
  act(() => selection.setState({ crop: { unit: '%', x: 20, y: 5, width: 40, height: 50 } }));
  expect(frame().style.left).toBe('20%');
  expect(container.querySelector('.crop-size-label')?.textContent).toBe('2400 × 2000 px');
});
