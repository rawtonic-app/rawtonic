import { describe, expect, it } from 'vitest';
import { pixelExactRect } from './SecondViewFrames';

const frame = (width: number, height: number) => ({
  id: 'f',
  width,
  height,
  shown: [6000, 4000] as [number, number],
  region: [1000, 800, width, height] as [number, number, number, number]
});

describe('pixelExactRect', () => {
  it('puts a 100 % region on whole device pixels at its own size', () => {
    expect(pixelExactRect([-412.37, 18.6, 1920.2, 1080], frame(1920, 1080))).toEqual([-412, 19, 1920, 1080]);
  });

  it('leaves a frame shown enlarged or reduced where it is', () => {
    expect(pixelExactRect([10.5, 3.2, 3840, 2160], frame(1920, 1080))).toBeNull();
    expect(pixelExactRect([10.5, 3.2, 960, 540], frame(1920, 1080))).toBeNull();
  });
});
