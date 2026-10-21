import { expect, it } from 'vitest';
import { AUTO_CA_VERSION, FRINGE_VERSION, INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';
import { withNeverEditedDefaults } from './neverEdited';

it('gives a never-edited RAW its defaults but keeps versions its recipe states', async () => {
  const camera = async () => ({ params: { k1: 0.1 } as unknown as Adjustments['lensDistortionParams'] });
  const fresh = await withNeverEditedDefaults({ ...INITIAL_ADJUSTMENTS }, null, true, camera);
  expect([fresh.fringeVersion, fresh.autoCaVersion, fresh.lensCorrectionMode]).toEqual([FRINGE_VERSION, AUTO_CA_VERSION, 'camera']);
  // Pasted with the processing versions (no lens mode): those stay.
  const saved = { exposure: 1, fringeVersion: 1, autoCaVersion: 1 };
  const pasted = await withNeverEditedDefaults({ ...INITIAL_ADJUSTMENTS, ...saved } as Adjustments, saved, true, camera);
  expect([pasted.fringeVersion, pasted.autoCaVersion]).toEqual([1, 1]);
  expect(pasted.lensCorrectionMode).toBe('camera');
});
