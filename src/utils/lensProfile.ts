import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import type { Adjustments } from './adjustments';

type LensFields = Pick<Adjustments, 'lensCorrectionMode' | 'lensMaker' | 'lensModel'>;

/** A recipe's lens choice: its mode and, chosen by hand, the lens (Auto's
 * lens is found, not chosen). */
export function lensChoice(adjustments: Partial<LensFields>): string {
  const manual = adjustments.lensCorrectionMode !== 'auto';
  return JSON.stringify([
    adjustments.lensCorrectionMode ?? null,
    manual ? adjustments.lensMaker ?? null : null,
    manual ? adjustments.lensModel ?? null : null,
  ]);
}

const exifNumber = (value: string | null | undefined) => {
  const parsed = Number.parseFloat(String(value ?? ''));
  return Number.isFinite(parsed) ? parsed : 0;
};

/** The lens fields for `adjustments`' lens choice on the photograph at
 * `path`, from the lens database as a save would look them up (with the
 * photo's EXIF): saves keep the parameters they get, so whatever changes the
 * lens choice looks them up here. Null for the camera's own data, which the
 * backend reads. */
export async function lookUpLensProfile(
  path: string,
  exif: Record<string, string | null | undefined>,
  adjustments: Partial<LensFields>,
): Promise<Partial<Adjustments> | null> {
  const mode = adjustments.lensCorrectionMode;
  if (mode === 'camera') return null;
  const params = (maker: string, model: string) =>
    invoke<Adjustments['lensDistortionParams']>(Invokes.GetLensDistortionParams, {
      maker,
      model,
      focalLength: exifNumber(exif.FocalLength),
      aperture: exifNumber(exif.FNumber),
      distance: exifNumber(exif.SubjectDistance),
      path,
    });
  if (mode === 'auto') {
    const found = exif.Make
      ? await invoke<[string, string] | null>(Invokes.AutodetectLens, {
          maker: exif.Make,
          model: exif.LensModel ?? '',
          cameraModel: exif.Model ?? '',
        })
      : null;
    if (!found) return { lensMaker: null, lensModel: null, lensDistortionParams: null };
    const [maker, model] = found;
    return { lensMaker: maker, lensModel: model, lensDistortionParams: await params(maker, model) };
  }
  if (adjustments.lensMaker && adjustments.lensModel)
    return { lensDistortionParams: await params(adjustments.lensMaker, adjustments.lensModel) };
  return { lensDistortionParams: null };
}
