import i18n from "i18next";

const UNITS = ["byte", "kilobyte", "megabyte", "gigabyte", "terabyte", "petabyte"] as const;

/** Decimal byte units, with numbers and unit labels in the interface language. */
export function formatFileSize(bytes: number): string {
  let value = Number.isFinite(bytes) ? Math.max(0, bytes) : 0;
  let unit = 0;
  while (value >= 1000 && unit < UNITS.length - 1) {
    value /= 1000;
    unit++;
  }
  // Avoid showing 1,000 kB at the rounding boundary instead of 1 MB.
  if (unit < UNITS.length - 1 && Math.round(value * 10) >= 10000) {
    value /= 1000;
    unit++;
  }
  return new Intl.NumberFormat(i18n.resolvedLanguage ?? i18n.language, {
    style: "unit",
    unit: UNITS[unit],
    unitDisplay: "short",
    maximumFractionDigits: unit === 0 ? 0 : 1,
  }).format(value);
}
