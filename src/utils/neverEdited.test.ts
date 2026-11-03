import { expect, it } from 'vitest';
import {
  AUTO_CA_VERSION,
  CAMERA_GEOMETRY_VERSION,
  FRINGE_VERSION,
  INITIAL_ADJUSTMENTS,
  SOURCE_FRAME_VERSION,
  type Adjustments
} from './adjustments';
import { savedSourceFrame, withNeverEditedDefaults } from './neverEdited';

it('gives a never-edited RAW its defaults but keeps versions its recipe states', async () => {
  const camera = async () => ({ params: { k1: 0.1 } as unknown as Adjustments['lensDistortionParams'] });
  const fresh = await withNeverEditedDefaults({ ...INITIAL_ADJUSTMENTS }, null, true, camera);
  expect([fresh.fringeVersion, fresh.autoCaVersion, fresh.lensCorrectionMode]).toEqual([
    FRINGE_VERSION,
    AUTO_CA_VERSION,
    'camera'
  ]);
  expect(fresh.cameraGeometryVersion).toBe(CAMERA_GEOMETRY_VERSION);
  // Pasted with the processing versions (no lens mode): those stay.
  const saved = { exposure: 1, fringeVersion: 0, autoCaVersion: 1 };
  const pasted = await withNeverEditedDefaults(
    { ...INITIAL_ADJUSTMENTS, ...saved } as Adjustments,
    saved,
    true,
    camera
  );
  expect([pasted.fringeVersion, pasted.autoCaVersion]).toEqual([0, 1]);
  expect(pasted.lensCorrectionMode).toBe('camera');
});

it('asks for the camera lens data of the frame a saved recipe develops', () => {
  // Never edited, or a Reset of a never-edited photo: the newest frame.
  expect(savedSourceFrame(null)).toBe(SOURCE_FRAME_VERSION);
  expect(savedSourceFrame({})).toBe(SOURCE_FRAME_VERSION);
  expect(savedSourceFrame({ is_null: true })).toBe(SOURCE_FRAME_VERSION);
  // A Reset keeps the edited photo's frame.
  expect(savedSourceFrame({ sourceFrameVersion: 0 })).toBe(0);
  expect(savedSourceFrame({ sourceFrameVersion: 1, exposure: 1 })).toBe(1);
  // An edit from before the key (a batch paste without a lens mode, too):
  // the first frame, as source_frame::requested.
  expect(savedSourceFrame({ exposure: 1 })).toBe(0);
});
