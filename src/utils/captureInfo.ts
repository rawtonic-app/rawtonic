/**
 * Capture settings of a photograph as the viewer shows them under it
 * (Capture One): "ISO 100   1/850 s   f/2.8   35 mm". The EXIF values come as
 * the backend formats them (exif_processing): "1/850 s", "2 s", "f/2.8", "35".
 */

type Exif = Record<string, string | number | null | undefined> | null | undefined;

const firstNumber = (value: unknown): number | null => {
  if (value === null || value === undefined) return null;
  const match = String(value).match(/\d+(?:\.\d+)?/);
  if (!match) return null;
  const number = parseFloat(match[0]);
  return Number.isFinite(number) && number > 0 ? number : null;
};

/** At most one decimal, none when it would be ".0". */
const oneDecimal = (value: number) => String(Math.round(value * 10) / 10);

/** Exposure time in seconds from "1/850 s", "1/850", "0.5 s" or "30". */
export function parseExposureTime(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  const text = String(value).trim();
  const fraction = text.match(/^(\d+(?:\.\d+)?)\s*\/\s*(\d+(?:\.\d+)?)/);
  if (fraction) {
    const numerator = parseFloat(fraction[1]);
    const denominator = parseFloat(fraction[2]);
    return denominator > 0 && numerator > 0 ? numerator / denominator : null;
  }
  return firstNumber(text);
}

/** 1/850 s, 1/3 s, then decimals as cameras mark them: 0.4 s, 0.5 s, 1.3 s, 2 s, 30 s. */
export function formatShutter(seconds: number | null): string | null {
  if (seconds === null || !(seconds > 0) || !Number.isFinite(seconds)) return null;
  if (seconds < 0.38) return `1/${Math.round(1 / seconds)} s`;
  return `${oneDecimal(seconds)} s`;
}

export function formatAperture(value: unknown): string | null {
  const number = firstNumber(value);
  return number === null ? null : `f/${oneDecimal(number)}`;
}

export function formatFocalLength(value: unknown): string | null {
  const number = firstNumber(value);
  return number === null ? null : `${oneDecimal(number)} mm`;
}

export function formatIso(value: unknown): string | null {
  const number = firstNumber(value);
  return number === null ? null : `ISO ${Math.round(number)}`;
}

/** The capture settings present in the EXIF, in Capture One's order; missing ones are left out. */
export function captureInfoParts(exif: Exif): string[] {
  if (!exif) return [];
  const parts = [
    formatIso(exif.PhotographicSensitivity ?? exif.ISOSpeedRatings ?? exif.ISOSpeed ?? exif.ISO),
    formatShutter(parseExposureTime(exif.ExposureTime)),
    formatAperture(exif.FNumber),
    formatFocalLength(exif.FocalLength),
  ];
  return parts.filter((part): part is string => part !== null);
}
