/** Crop frame resizing from the middle of a side while the aspect ratio is
 * fixed, as in Capture One: the dragged side moves, the other dimension
 * follows the ratio and stays centred. react-image-crop hides these handles
 * with a fixed ratio (it resizes them as corners and collapses the frame). */

export type CropEdge = 'n' | 'e' | 's' | 'w';

export interface CropRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export function isCropEdge(ord: string | null | undefined): ord is CropEdge {
  return ord === 'n' || ord === 'e' || ord === 's' || ord === 'w';
}

/** The frame after dragging `edge` by `dx`, `dy` (same units as the rects)
 * inside a box of `boxWidth` × `boxHeight`, keeping `aspect` (width/height). */
export function resizeCropFromEdge(
  start: CropRect,
  edge: CropEdge,
  dx: number,
  dy: number,
  aspect: number,
  boxWidth: number,
  boxHeight: number,
  minSize = 10,
): CropRect {
  const vertical = edge === 'n' || edge === 's';
  const centreX = start.x + start.width / 2;
  const centreY = start.y + start.height / 2;
  if (vertical) {
    // The dragged side may reach the box edge; the centred width must fit.
    const maxHeight = Math.min(
      edge === 'n' ? start.y + start.height : boxHeight - start.y,
      (2 * Math.min(centreX, boxWidth - centreX)) / aspect,
    );
    const height = Math.max(minSize, Math.min(maxHeight, start.height + (edge === 'n' ? -dy : dy)));
    const width = height * aspect;
    return {
      x: centreX - width / 2,
      y: edge === 'n' ? start.y + start.height - height : start.y,
      width,
      height,
    };
  }
  const maxWidth = Math.min(
    edge === 'w' ? start.x + start.width : boxWidth - start.x,
    2 * Math.min(centreY, boxHeight - centreY) * aspect,
  );
  const width = Math.max(minSize, Math.min(maxWidth, start.width + (edge === 'w' ? -dx : dx)));
  const height = width / aspect;
  return {
    x: edge === 'w' ? start.x + start.width - width : start.x,
    y: centreY - height / 2,
    width,
    height,
  };
}
