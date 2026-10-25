import { invoke } from '@tauri-apps/api/core';
import { Invokes, type SelectedImage } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { isNegativeActive, type Adjustments } from './adjustments';

type Rgb = [number, number, number];

interface PreviewSample {
  image: SelectedImage;
  adjustments: Adjustments;
  normalizedX: number;
  normalizedY: number;
  useWgpu: boolean;
  finalPreviewUrl: string | null;
}

function sampleRenderedImage(url: string, x: number, y: number): Promise<Rgb | null> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.crossOrigin = 'Anonymous';
    image.onload = () => {
      try {
        const centerX = Math.floor(x * image.width);
        const centerY = Math.floor(y * image.height);
        const left = Math.max(0, centerX - 5);
        const top = Math.max(0, centerY - 5);
        const width = Math.min(image.width, centerX + 6) - left;
        const height = Math.min(image.height, centerY + 6) - top;
        if (width <= 0 || height <= 0) { resolve(null); return; }
        const canvas = document.createElement('canvas');
        canvas.width = 11;
        canvas.height = 11;
        const context = canvas.getContext('2d', { willReadFrequently: true });
        if (!context) { resolve(null); return; }
        context.drawImage(image, left, top, width, height, 0, 0, width, height);
        const data = context.getImageData(0, 0, width, height).data;
        const sum: Rgb = [0, 0, 0];
        for (let index = 0; index < data.length; index += 4) {
          sum[0] += data[index];
          sum[1] += data[index + 1];
          sum[2] += data[index + 2];
        }
        const count = data.length / 4;
        resolve(count ? sum.map((channel) => channel / count) as Rgb : null);
      } catch (error) { reject(error); }
    };
    image.onerror = () => reject(new Error('Could not read the developed preview.'));
    image.src = url;
  });
}

const clamp100 = (value: number) => Math.max(-100, Math.min(100, value));

/** Temperature and Tint that make linear `rgb` neutral under the shader's
 * white balance (`apply_white_balance`, temperature / 25 and tint / 100):
 * gains (1 + 0.2T, 1 + 0.05T, 1 − 0.2T) · (1 + 0.25t, 1 − 0.25t, 1 + 0.25t).
 * Temperature equalises red and blue, Tint then green with them. Solved on
 * the input before any adjustment, the result is absolute: it does not depend
 * on the current white balance, exposure, contrast or curves. */
export function neutralWhiteBalance([r, g, b]: Rgb) {
  if (!(r > 1e-6 && g > 1e-6 && b > 1e-6)) return null;
  const k = b / r;
  const t = Math.max(-4, Math.min(4, (5 * (k - 1)) / (k + 1)));
  const green = (1 + 0.05 * t) * g;
  const red = (1 + 0.2 * t) * r;
  const tint = (4 * (green - red)) / (green + red);
  return { temperature: clamp100(25 * t), tint: clamp100(100 * tint) };
}

/** [`neutralWhiteBalance`] in the sliders' whole steps (a picked 1.6009 is
 * noise, and does not fit the value field). */
export function neutralSliders(rgb: Rgb) {
  const exact = neutralWhiteBalance(rgb);
  return exact && { temperature: Math.round(exact.temperature), tint: Math.round(exact.tint) };
}

/** Auto white balance for `image`, as recipe values. RAWs: the Kelvin and
 * tint of the backend's Auto (fitted to Camera Raw's), relative sliders
 * cleared. Files without a camera calibration (JPEG, TIFF, some 4-colour
 * sensors) and film negatives (`adjustments.negative`): the relative sliders
 * that neutralise the preview's grey world. `null` while the photo's preview
 * is not ready. */
export async function autoWhiteBalance(
  image: SelectedImage,
  adjustments?: Partial<Adjustments>,
): Promise<Partial<Adjustments> | null> {
  // A film negative's camera white balance describes the light through the
  // film, not the scene: its positive is balanced on the preview's grey world.
  if (image.isRaw && !(adjustments && isNegativeActive(adjustments))) {
    const kelvin = await invoke<{ temperature: number; tint: number } | null>(Invokes.AutoWhiteBalance, {
      path: image.path,
    });
    if (kelvin) {
      return { whiteBalance: 'auto', wbTemperature: kelvin.temperature, wbTint: kelvin.tint, temperature: 0, tint: 0 };
    }
  }
  const grey = await invoke<Rgb | null>(Invokes.SamplePreviewGreyWorld, { path: image.path });
  return grey && neutralSliders(grey);
}

export async function whiteBalanceForCurrentPreview(request: PreviewSample) {
  const stillCurrent = () => {
    const current = useEditorStore.getState();
    return current.selectedImage === request.image && current.selectedImage?.isReady &&
      current.adjustments === request.adjustments;
  };
  if (!stillCurrent() || !Number.isFinite(request.normalizedX) || !Number.isFinite(request.normalizedY) ||
      request.normalizedX < 0 || request.normalizedX > 1 || request.normalizedY < 0 || request.normalizedY > 1) return null;

  // The native display: sample the preview's linear input before any
  // adjustment and solve exactly (sampling the developed pixels let contrast,
  // saturation and curves exaggerate the cast).
  if (request.useWgpu) {
    const linear = await invoke<Rgb | null>(Invokes.SamplePreviewWhiteBalance, {
      path: request.image.path, normalizedX: request.normalizedX, normalizedY: request.normalizedY,
    });
    if (!linear || !stillCurrent()) return null;
    // RAWs: the white balance in Kelvin and tint, from the camera's own
    // calibration (Lightroom's eyedropper); the relative sliders go to zero.
    // A film negative's positive takes the relative sliders, which act on
    // it exactly.
    if (request.image.isRaw && !isNegativeActive(request.adjustments)) {
      const kelvin = await invoke<{ temperature: number; tint: number } | null>(
        Invokes.WhiteBalanceFromSample,
        { imagePath: request.image.path, rgb: linear },
      ).catch(() => null);
      if (kelvin && Number.isFinite(kelvin.temperature) && Number.isFinite(kelvin.tint) && stillCurrent()) {
        return {
          whiteBalance: 'custom' as const,
          wbTemperature: kelvin.temperature,
          wbTint: kelvin.tint,
          temperature: 0,
          tint: 0,
        };
      }
    }
    return neutralSliders(linear);
  }
  // The CPU-rendered fallback only has the developed image: correct the
  // remaining cast there on top of the current values. A camera JPEG is only a
  // loading placeholder, so never sample its embedded thumbnail.
  const rgb = request.finalPreviewUrl
    ? await sampleRenderedImage(request.finalPreviewUrl, request.normalizedX, request.normalizedY)
    : null;
  if (!rgb || !stillCurrent()) return null;
  const correction = neutralWhiteBalance(rgb.map((channel) => Math.pow(channel / 255, 2.2)) as Rgb);
  if (!correction) return null;
  return {
    temperature: Math.round(clamp100((request.adjustments.temperature ?? 0) + correction.temperature)),
    tint: Math.round(clamp100((request.adjustments.tint ?? 0) + correction.tint)),
  };
}

/** Lightroom's white balance presets for RAWs (Kelvin, tint); kept in step
 * with `kelvin_white_balance` in image_processing.rs. */
export const WHITE_BALANCE_PRESETS = {
  daylight: { label: 'Daylight', temperature: 5500, tint: 10 },
  cloudy: { label: 'Cloudy', temperature: 6500, tint: 10 },
  shade: { label: 'Shade', temperature: 7500, tint: 10 },
  tungsten: { label: 'Tungsten', temperature: 2850, tint: 0 },
  fluorescent: { label: 'Fluorescent', temperature: 3800, tint: 21 },
  flash: { label: 'Flash', temperature: 5500, tint: 0 },
} as const;

/** The Kelvin slider runs in mireds, so warm light gets more travel, as in
 * Lightroom. */
export const KELVIN_SCALE = {
  toTrack: (kelvin: number) => -1e6 / Math.max(kelvin, 1),
  fromTrack: (position: number) => Math.round(-1e6 / Math.min(position, -1)),
};
