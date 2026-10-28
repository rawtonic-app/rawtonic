/** Map client coordinates through the SVG's inverse screen transform. */
export function curvePointerPosition(
  clientX: number,
  clientY: number,
  inverse: Pick<DOMMatrix, "a" | "b" | "c" | "d" | "e" | "f">,
) {
  const clamp = (value: number) => Math.max(0, Math.min(255, value));
  return {
    x: clamp(inverse.a * clientX + inverse.c * clientY + inverse.e),
    y: clamp(255 - (inverse.b * clientX + inverse.d * clientY + inverse.f)),
  };
}
