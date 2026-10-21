import { MAX_CURVE_POINTS, type Coord } from "./adjustments";

/**
 * The renderer's point curve (shader `apply_curve`, point_curve.rs):
 * monotone cubic Hermite through the points, flat beyond the ends. `x` and
 * the result in 0–255; fewer than two points leave `x` alone.
 */
export function pointCurveValue(
  points: Coord[] | undefined,
  x: number,
): number {
  const p =
    points && points.length >= 2
      ? points
      : [
          { x: 0, y: 0 },
          { x: 255, y: 255 },
        ];
  const n = p.length;
  if (x <= p[0].x) return p[0].y;
  if (x >= p[n - 1].x) return p[n - 1].y;
  for (let i = 0; i < n - 1; i += 1) {
    const p1 = p[i];
    const p2 = p[i + 1];
    if (x > p2.x) continue;
    const p0 = p[Math.max(i - 1, 0)];
    const p3 = p[Math.min(i + 2, n - 1)];
    const slope = (a: Coord, b: Coord) =>
      (b.y - a.y) / Math.max(b.x - a.x, 0.001);
    const before = slope(p0, p1);
    const current = slope(p1, p2);
    const after = slope(p2, p3);
    let m1 =
      i === 0 ? current : before * current <= 0 ? 0 : (before + current) / 2;
    let m2 =
      i + 1 === n - 1
        ? current
        : current * after <= 0
          ? 0
          : (current + after) / 2;
    if (current !== 0) {
      const alpha = m1 / current;
      const beta = m2 / current;
      if (alpha * alpha + beta * beta > 9) {
        const tau = 3 / Math.sqrt(alpha * alpha + beta * beta);
        m1 *= tau;
        m2 *= tau;
      }
    }
    const dx = p2.x - p1.x;
    if (dx <= 0) return Math.min(255, Math.max(0, p1.y));
    const t = (x - p1.x) / dx;
    const t2 = t * t;
    const t3 = t2 * t;
    const y =
      (2 * t3 - 3 * t2 + 1) * p1.y +
      (t3 - 2 * t2 + t) * m1 * dx +
      (-2 * t3 + 3 * t2) * p2.y +
      (t3 - t2) * m2 * dx;
    return Math.min(255, Math.max(0, y));
  }
  return p[n - 1].y;
}

/**
 * The points the renderer uses: all of them with `whole` (the photo's
 * curves at `Curves.version` 2), else the first 16 (curves saved before, layers).
 */
export const renderedPoints = (
  points: Coord[] | undefined,
  whole: boolean,
): Coord[] | undefined => (whole ? points : points?.slice(0, MAX_CURVE_POINTS));
