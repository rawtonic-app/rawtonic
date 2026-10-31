// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * The Loupe face strip's box on the photograph (G35; notes:
 * reference/ux-eyes-culling-2026-10-31/README.md §4). The backend carries a
 * face's box (fractions of the upright, uncropped photograph, as read)
 * through the recipe's geometry into mask-frame pixels (`face_boxes_shown`,
 * the mapping red eye corrections use: turns, flips, straightening,
 * transforms, lens distortion); here it goes the rest of the way, as every
 * overlay of the viewer does: minus the crop's origin, times the displayed
 * photograph's scale (CSS pixels of its fitted box, `fitInViewer`). The
 * viewer's zoom and pan transform the overlay with the photograph, so the
 * box follows them without being asked again.
 */

export type Point = { x: number; y: number };

/** `outline` (mask-frame pixels) in the displayed photograph's box. */
export function outlineOnPhoto(
  outline: ReadonlyArray<readonly [number, number]>,
  cropX: number,
  cropY: number,
  scale: number,
): Point[] {
  return outline.map(([x, y]) => ({ x: (x - cropX) * scale, y: (y - cropY) * scale }));
}

/** SVG `points` of an outline. */
export const svgPoints = (points: Point[]) => points.map((p) => `${p.x},${p.y}`).join(' ');

/**
 * Where the box's label goes: at its top left corner as shown (the point
 * nearest the outline's bounding box's top left), above the box; below
 * that corner's edge when the box reaches the photograph's top, so the
 * label stays on the photograph.
 */
export function labelPlace(points: Point[], labelHeight: number): Point & { below: boolean } {
  const minX = Math.min(...points.map((p) => p.x));
  const minY = Math.min(...points.map((p) => p.y));
  const corner = points.reduce((best, p) =>
    Math.hypot(p.x - minX, p.y - minY) < Math.hypot(best.x - minX, best.y - minY) ? p : best,
  );
  const below = corner.y < labelHeight;
  return { x: corner.x, y: corner.y, below };
}
