import i18n from 'i18next';

/** The backend's error when a photo exceeds the GPU's texture limit
 * (`rawtonic_gpu::gpu_processing::texture_limit_error`). */
const TEXTURE_LIMIT =
  /^The image \((\d+) × (\d+) px\) is larger than this graphics card can render \((\d+) px per side\)\.$/;

/** The backend's error for Light flare on such a photo
 * (`rawtonic_gpu::gpu_processing::flare_texture_limit_error`). */
const FLARE_TEXTURE_LIMIT =
  /^Light flare cannot be rendered on an image of (\d+) × (\d+) px, larger than this graphics card's limit \((\d+) px per side\)\. Turn Light flare off to export it\.$/;

/** A render error in the interface language; other errors keep their text. */
export function renderErrorMessage(error: unknown): string {
  const message = String(error).replace(/^Error: /, '');
  const limit = TEXTURE_LIMIT.exec(message);
  if (limit) {
    const [, width, height, max] = limit;
    return i18n.t('studio.export.errors.textureLimit', { width, height, limit: max });
  }
  const flare = FLARE_TEXTURE_LIMIT.exec(message);
  if (flare) {
    const [, width, height, max] = flare;
    return i18n.t('studio.export.errors.flareTextureLimit', { width, height, limit: max });
  }
  return message;
}
