/** sRGB (0–255, display encoded) to Oklab; the same matrices as the shader. */
export function srgbToOklab([r, g, b]: [number, number, number]): [number, number, number] {
  const lin = [r, g, b].map((v) => {
    const c = Math.min(1, Math.max(0, v / 255));
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  const l = Math.cbrt(0.4122214708 * lin[0] + 0.5363325363 * lin[1] + 0.0514459929 * lin[2]);
  const m = Math.cbrt(0.2119034982 * lin[0] + 0.6806995451 * lin[1] + 0.1073969566 * lin[2]);
  const s = Math.cbrt(0.0883024619 * lin[0] + 0.2817188376 * lin[1] + 0.6299787005 * lin[2]);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

/** Oklab hue (degrees), chroma and lightness of a display colour. */
export function srgbToOklch(rgb: [number, number, number]) {
  const [lightness, a, b] = srgbToOklab(rgb);
  return {
    lightness,
    chroma: Math.hypot(a, b),
    hue: ((Math.atan2(b, a) * 180) / Math.PI + 360) % 360,
  };
}

/** CSS colour for an Oklab LCh triple (for swatches). */
export function oklchCss(lightness: number, chroma: number, hue: number) {
  return `oklch(${(lightness * 100).toFixed(1)}% ${chroma.toFixed(3)} ${hue.toFixed(1)})`;
}
