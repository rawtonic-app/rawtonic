import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { CAMERA_CA_VERSION, type Adjustments } from './adjustments';

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
 * `path`: the camera's own data, or the lens database as a save would look
 * them up (with the photo's EXIF). Saves keep the parameters they get, so
 * whatever changes the lens choice looks them up here; rejects when the
 * lookup fails. */
export async function lookUpLensProfile(
  path: string,
  exif: Record<string, string | null | undefined>,
  adjustments: Partial<LensFields>,
): Promise<Partial<Adjustments>> {
  const mode = adjustments.lensCorrectionMode;
  if (mode === 'camera') {
    const camera = await invoke<{ params: Adjustments['lensDistortionParams'] } | null>(
      Invokes.GetCameraLensProfile,
      { path },
    );
    return {
      lensMaker: null,
      lensModel: null,
      lensDistortionParams: camera?.params ?? null,
      cameraCaVersion: CAMERA_CA_VERSION,
    };
  }
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

/** Lens parameters still to be looked up: the editor commits a new lens
 * choice at once with these, the save looks them up (edits.rs, LensLookup),
 * and `refreshLensProfile` fills them in for the preview meanwhile. */
export const PENDING_LENS = { pending: true } as unknown as Adjustments['lensDistortionParams'];

export function isPendingLens(params: unknown): boolean {
  return Boolean(params && typeof params === 'object' && (params as { pending?: unknown }).pending === true);
}

/** The lens fields to commit with a new lens choice: parameters pending, and
 * for Auto no lens found yet. */
export function pendingLens(adjustments: Partial<LensFields>): Partial<Adjustments> {
  return adjustments.lensCorrectionMode === 'auto'
    ? { lensMaker: null, lensModel: null, lensDistortionParams: PENDING_LENS }
    : { lensDistortionParams: PENDING_LENS };
}

/** Looks the pending lens up for the preview and fills it in only while the
 * photograph, the lens choice and the pending mark are still those it was
 * started for: a later choice, an undo or switching correction off wins. */
export function refreshLensProfile(
  path: string,
  exif: Record<string, string | null | undefined>,
  choice: Partial<LensFields>,
  isCurrent: (path: string) => boolean,
  update: (change: (current: Adjustments) => Adjustments) => void,
): void {
  void lookUpLensProfile(path, exif, choice)
    .then((lens) => {
      if (!isCurrent(path)) return;
      update((current) =>
        isPendingLens(current.lensDistortionParams) && lensChoice(current) === lensChoice(choice)
          ? { ...current, ...lens }
          : current,
      );
    })
    .catch(() => {});
}
