/** The photo's orientation steps, flips and straightening, as the masks that
 * are drawn on the shown photo record it with their points. */
export interface FrameGeometry {
  rotation: number;
  flipHorizontal: boolean;
  flipVertical: boolean;
  orientationSteps: number;
}

export interface Point {
  x: number;
  y: number;
}

/** The geometry of `adjustments` (missing values as the photo shown as it is). */
export function frameGeometry(source: Partial<Record<keyof FrameGeometry, unknown>> | null | undefined): FrameGeometry {
  return {
    rotation: typeof source?.rotation === 'number' ? source.rotation : 0,
    flipHorizontal: source?.flipHorizontal === true,
    flipVertical: source?.flipVertical === true,
    orientationSteps: ((Number(source?.orientationSteps) || 0) % 4 + 4) % 4,
  };
}

export function sameFrameGeometry(a: FrameGeometry, b: FrameGeometry): boolean {
  return (
    a.rotation === b.rotation &&
    a.flipHorizontal === b.flipHorizontal &&
    a.flipVertical === b.flipVertical &&
    a.orientationSteps === b.orientationSteps
  );
}

/** The photo's size as shown in `geometry`, from its unturned `width` × `height`. */
function shownSize(width: number, height: number, geometry: FrameGeometry) {
  return geometry.orientationSteps % 2 === 1 ? { w: height, h: width } : { w: width, h: height };
}

/** A point of the photo shown in `geometry` (full-resolution positions after
 * orientation, flips and straightening, before the crop) in the unturned
 * photo of `width` × `height`; the engine's `display_point_to_source`, for
 * colour samples and target points. */
export function shownToPhoto(point: Point, geometry: FrameGeometry, width: number, height: number): Point {
  const { w, h } = shownSize(width, height, geometry);
  const cx = w / 2;
  const cy = h / 2;
  const rad = (geometry.rotation * Math.PI) / 180;
  const cos = Math.cos(rad);
  const sin = Math.sin(rad);
  const dx = point.x - cx;
  const dy = point.y - cy;
  let x = dx * cos + dy * sin + cx;
  let y = -dx * sin + dy * cos + cy;
  if (geometry.flipHorizontal) x = w - x;
  if (geometry.flipVertical) y = h - y;
  switch (geometry.orientationSteps) {
    case 1:
      return { x: y, y: w - x };
    case 2:
      return { x: w - x, y: h - y };
    case 3:
      return { x: h - y, y: x };
    default:
      return { x, y };
  }
}

/** The inverse of `shownToPhoto`: where a point of the unturned photo is
 * shown in `geometry`. */
export function photoToShown(point: Point, geometry: FrameGeometry, width: number, height: number): Point {
  const { w, h } = shownSize(width, height, geometry);
  let x: number;
  let y: number;
  switch (geometry.orientationSteps) {
    case 1:
      x = w - point.y;
      y = point.x;
      break;
    case 2:
      x = w - point.x;
      y = h - point.y;
      break;
    case 3:
      x = point.y;
      y = h - point.x;
      break;
    default:
      x = point.x;
      y = point.y;
  }
  if (geometry.flipHorizontal) x = w - x;
  if (geometry.flipVertical) y = h - y;
  const cx = w / 2;
  const cy = h / 2;
  const rad = (geometry.rotation * Math.PI) / 180;
  const cos = Math.cos(rad);
  const sin = Math.sin(rad);
  const dx = x - cx;
  const dy = y - cy;
  return { x: cx + dx * cos - dy * sin, y: cy + dx * sin + dy * cos };
}

/** A point drawn on the photo shown in `from`, where it is shown in `to`
 * (the same spot of the photo). */
export function reframePoint(point: Point, from: FrameGeometry, to: FrameGeometry, width: number, height: number): Point {
  if (sameFrameGeometry(from, to)) return point;
  return photoToShown(shownToPhoto(point, from, width, height), to, width, height);
}

/** The frame a drawn mask (a radial or linear gradient, a brush or flow
 * stroke) was drawn in: the photo's orientation steps, flips and
 * straightening then, and the photo's own (unturned, full-resolution) size.
 * Shapes are stored on the photo as it was shown then; with their frame
 * they stay on that spot of the photo when it is turned, flipped or
 * straightened later, and on a photo of another size (copied, synced or in
 * a preset) they land on the same part of it, as Lightroom keeps its masks
 * relative to the photo (`frame` in docs/RECIPE_SCHEMA.md; the engine's
 * `ShapeFrame`). A frame stamped where the size was not known has none and
 * takes the size of the photo it is on. */
export interface ShapeFrame extends FrameGeometry {
  width?: number;
  height?: number;
}

/** A frame with the photo's size: the photo as it is shown now. */
export interface SizedFrame extends FrameGeometry {
  width: number;
  height: number;
}

/** The frame of the photo shown in `geometry`, with its unturned size; none
 * while the size is unknown. */
export function shapeFrame(
  geometry: Partial<Record<keyof FrameGeometry, unknown>> | null | undefined,
  width: number | undefined,
  height: number | undefined,
): SizedFrame | undefined {
  if (!(width && width > 0 && height && height > 0)) return undefined;
  return { ...frameGeometry(geometry), width, height };
}

/** A stored frame, if it is usable: with a size, or with none. */
function storedFrame(value: unknown): ShapeFrame | undefined {
  if (!value || typeof value !== 'object') return undefined;
  const frame = value as Partial<ShapeFrame>;
  const { width, height } = frame;
  if (width == null && height == null) return frameGeometry(frame);
  if (!(typeof width === 'number' && width > 0 && typeof height === 'number' && height > 0)) return undefined;
  return { ...frameGeometry(frame), width, height };
}

/** Where the photo's pixels are shown in `geometry`, in pixel indices as the
 * engine's image operations move them (index i of n lands on n − 1 − i; the
 * engine's `Placement`), so a moved shape covers the same pixels in the
 * viewer and in the render. */
function placement(geometry: FrameGeometry, width: number, height: number) {
  const { w, h } = shownSize(width, height, geometry);
  const rad = (geometry.rotation * Math.PI) / 180;
  const cos = Math.cos(rad);
  const sin = Math.sin(rad);
  return {
    /** The photo's pixel shown at `point`. */
    toPhoto(point: Point): Point {
      const dx = point.x - w / 2;
      const dy = point.y - h / 2;
      let x = dx * cos + dy * sin + w / 2;
      let y = -dx * sin + dy * cos + h / 2;
      if (geometry.flipHorizontal) x = w - 1 - x;
      if (geometry.flipVertical) y = h - 1 - y;
      switch (geometry.orientationSteps) {
        case 1:
          return { x: y, y: w - 1 - x };
        case 2:
          return { x: w - 1 - x, y: h - 1 - y };
        case 3:
          return { x: h - 1 - y, y: x };
        default:
          return { x, y };
      }
    },
    /** Where the photo's pixel `point` is shown. */
    toShown(point: Point): Point {
      let x: number;
      let y: number;
      switch (geometry.orientationSteps) {
        case 1:
          x = height - 1 - point.y;
          y = point.x;
          break;
        case 2:
          x = width - 1 - point.x;
          y = height - 1 - point.y;
          break;
        case 3:
          x = point.y;
          y = width - 1 - point.x;
          break;
        default:
          x = point.x;
          y = point.y;
      }
      if (geometry.flipHorizontal) x = w - 1 - x;
      if (geometry.flipVertical) y = h - 1 - y;
      const dx = x - w / 2;
      const dy = y - h / 2;
      return { x: w / 2 + dx * cos - dy * sin, y: h / 2 + dx * sin + dy * cos };
    },
  };
}

/** The photo's pixel shown at pixel `point` of the photo of `width` ×
 * `height` shown in `geometry` (pixel indices, as drawn masks are placed). */
export function shownPixelToPhoto(point: Point, geometry: FrameGeometry, width: number, height: number): Point {
  return placement(geometry, width, height).toPhoto(point);
}

/** The inverse of `shownPixelToPhoto`. */
export function photoPixelToShown(point: Point, geometry: FrameGeometry, width: number, height: number): Point {
  return placement(geometry, width, height).toShown(point);
}

/** From a shape's frame to the photo shown in `now`: none when the shape is
 * shown as drawn (same geometry and size). The engine's `ShapeFrame::map_to`. */
function shapeMap(frame: ShapeFrame | undefined, now: SizedFrame) {
  if (!frame) return null;
  const fromW = frame.width ?? now.width;
  const fromH = frame.height ?? now.height;
  // Sizes less than a pixel apart are the same photo.
  const resized = Math.abs(now.width - fromW) >= 1 || Math.abs(now.height - fromH) >= 1;
  if (!resized && sameFrameGeometry(frame, now)) return null;
  const toW = resized ? now.width : fromW;
  const toH = resized ? now.height : fromH;
  const from = placement(frame, fromW, fromH);
  const to = placement(now, toW, toH);
  const sx = toW / fromW;
  const sy = toH / fromH;
  const point = (p: Point): Point => {
    const q = from.toPhoto(p);
    // The same fraction of the other photo (index i of n covers (i + 0.5) / n).
    return to.toShown(resized ? { x: (q.x + 0.5) * sx - 0.5, y: (q.y + 0.5) * sy - 0.5 } : q);
  };
  const origin = point({ x: 0, y: 0 });
  const direction = (d: Point) => {
    const p = point(d);
    return { x: p.x - origin.x, y: p.y - origin.y };
  };
  const ex = direction({ x: 1, y: 0 });
  const ey = direction({ x: 0, y: 1 });
  // One flip more on one side than on the other mirrors the shape.
  const mirrors = ex.x * ey.y - ex.y * ey.x < 0;
  // How areas scale (1 unless the photo's size differs).
  const areaScale = resized ? sx * sy : 1;
  return { point, direction, mirrors, resized, areaScale };
}

const SHAPE_TYPES = new Set(['radial', 'linear']);
const STROKE_TYPES = new Set(['brush', 'flow', 'clone', 'heal', 'liquify', 'retouch']);

type Params = Record<string, unknown>;

/** A radial or linear gradient as drawn on the photo shown in `now`, with
 * `now` as its frame: one drawn in another frame moved to where that spot
 * of the photo is shown now (on a photo of another size: the same part of
 * it, its radii and ranges scaled along); one without a frame was drawn as
 * the photo is shown now. Brush and flow strokes drawn in another frame are
 * moved the same way (for drawing them: strokes are stored as drawn, each
 * with its own frame). The engine's `shape_as_shown`. */
export function shapeAsShown<T extends Params>(type: string, params: T, now: SizedFrame | undefined): T {
  if (!now || !params) return params;
  if (STROKE_TYPES.has(type)) {
    const lines = params.lines as Array<{ points?: Point[]; frame?: unknown; brushSize?: unknown }> | undefined;
    if (!Array.isArray(lines)) return params;
    let moved = false;
    const shown = lines.map((line) => {
      const map = shapeMap(storedFrame(line?.frame), now);
      if (!map || !Array.isArray(line.points)) return line;
      moved = true;
      const resized = { ...line, points: line.points.map(map.point), frame: now };
      if (map.resized && typeof line.brushSize === 'number') resized.brushSize = line.brushSize * Math.sqrt(map.areaScale);
      return resized;
    });
    return moved ? { ...params, lines: shown } : params;
  }
  if (!SHAPE_TYPES.has(type)) return params;
  const map = shapeMap(storedFrame(params.frame), now);
  if (!map) return { ...params, frame: now };
  const num = (key: string) => Number(params[key]) || 0;
  if (type === 'radial') {
    const centre = map.point({ x: num('centerX'), y: num('centerY') });
    const angle = (num('rotation') * Math.PI) / 180;
    const axis = map.direction({ x: Math.cos(angle), y: Math.sin(angle) });
    const shown: Params = {
      ...params,
      centerX: centre.x,
      centerY: centre.y,
      rotation: (Math.atan2(axis.y, axis.x) * 180) / Math.PI,
      frame: now,
    };
    if (map.resized) {
      // The first axis as it is carried along; the second keeps the
      // ellipse's area in proportion.
      const along = Math.hypot(axis.x, axis.y);
      shown.radiusX = num('radiusX') * along;
      shown.radiusY = (num('radiusY') * map.areaScale) / along;
    }
    return shown as T;
  }
  const start = map.point({ x: num('startX'), y: num('startY') });
  const end = map.point({ x: num('endX'), y: num('endY') });
  // Mirrored, the start (100%) side would fall on the other side of the
  // line through start and end: swapping them keeps every side in place.
  const [a, b] = map.mirrors ? [end, start] : [start, end];
  const shown: Params = { ...params, startX: a.x, startY: a.y, endX: b.x, endY: b.y, frame: now };
  if (map.resized) {
    // Distances from the central line scale as areas over lengths along it.
    const dx = num('endX') - num('startX');
    const dy = num('endY') - num('startY');
    const length = Math.hypot(dx, dy);
    const along = length > 0 ? map.direction({ x: dx / length, y: dy / length }) : null;
    const factor = along ? map.areaScale / Math.hypot(along.x, along.y) : Math.sqrt(map.areaScale);
    shown.range = (typeof params.range === 'number' ? params.range : 50) * factor;
    if (typeof params.rangeEnd === 'number') shown.rangeEnd = params.rangeEnd * factor;
    const band = params.band as { width: number; feather: number } | undefined;
    if (band) shown.band = { width: band.width * factor, feather: band.feather * factor };
  }
  return shown as T;
}

interface FramedContainer {
  subMasks: Array<{ type: string; parameters: Params }>;
}

/** `containers` with `frame` given to every drawn shape and stroke without
 * one (and, with `fillSizes`, `frame`'s size to frames without one), among
 * the sub-masks whose parameters `accept` takes. Unchanged containers,
 * sub-masks and parameters stay the same objects. */
function stampFrames<C extends FramedContainer>(
  containers: C[] | undefined,
  frame: ShapeFrame,
  fillSizes: boolean,
  accept: (params: Params) => boolean = () => true,
): C[] | undefined {
  if (!containers) return containers;
  const sized = fillSizes && frame.width !== undefined && frame.height !== undefined;
  const stamped = (value: unknown): ShapeFrame | null => {
    if (!value) return frame;
    if (!sized || typeof value !== 'object') return null;
    const stored = value as ShapeFrame;
    return stored.width == null && stored.height == null ? { ...stored, width: frame.width, height: frame.height } : null;
  };
  const framed = (params: Params, type: string): Params => {
    if (!params || !accept(params)) return params;
    if (SHAPE_TYPES.has(type)) {
      const next = stamped(params.frame);
      return next ? { ...params, frame: next } : params;
    }
    if (!STROKE_TYPES.has(type) || !Array.isArray(params.lines)) return params;
    let changed = false;
    const lines = (params.lines as Params[]).map((line) => {
      const next = stamped(line?.frame);
      if (!next) return line;
      changed = true;
      return { ...line, frame: next };
    });
    return changed ? { ...params, lines } : params;
  };
  let changed = false;
  const out = containers.map((container) => {
    let containerChanged = false;
    const subMasks = (container.subMasks ?? []).map((sub) => {
      const parameters = framed(sub.parameters, sub.type);
      if (parameters === sub.parameters) return sub;
      containerChanged = true;
      return { ...sub, parameters };
    });
    if (!containerChanged) return container;
    changed = true;
    return { ...container, subMasks };
  });
  return changed ? out : containers;
}

/** Drawn masks without a frame (made before frames were kept) were drawn as
 * the photo is shown. When an edit changes the photo's orientation, flips or
 * straightening, those the edit left as they were get the frame they were
 * shown in until then, so they stay on the photo's content (as Lightroom's
 * and Capture One's masks do) instead of on the screen; without the photo's
 * size the frame records none. Undo and redo restore recipes as they were
 * and do not come here. The backend does the same for pasted, synced and
 * preset settings (`keep_drawn_masks_on_photo` in the engine's
 * mask_generation.rs), where the masks left alone are those whose
 * parameters did not change; here they are the very same objects. */
export function keepDrawnMasksOnPhoto<
  T extends Partial<FrameGeometry> & { masks?: FramedContainer[]; aiPatches?: FramedContainer[] },
>(previous: T, next: T, width: number | undefined, height: number | undefined): T {
  const geometry = frameGeometry(previous);
  if (sameFrameGeometry(geometry, frameGeometry(next))) return next;
  const before: ShapeFrame = shapeFrame(previous, width, height) ?? geometry;
  const untouched = new Set<unknown>();
  for (const container of [...(previous.masks ?? []), ...(previous.aiPatches ?? [])]) {
    for (const sub of container.subMasks ?? []) untouched.add(sub.parameters);
  }
  const leftAlone = (params: Params) => untouched.has(params);
  const masks = stampFrames(next.masks, before, false, leftAlone);
  const aiPatches = stampFrames(next.aiPatches, before, false, leftAlone);
  if (masks === next.masks && aiPatches === next.aiPatches) return next;
  return { ...next, masks, aiPatches };
}

/** Drawn masks leaving their photo (copied, synced to other photos, saved in
 * a preset) with the frame of the photo they were drawn on: those without a
 * frame get the one of the photo shown in `geometry`, frames without a size
 * get the photo's `width` × `height` when known. Another photo then places
 * them on the same part of it, whatever its geometry and size. */
export function frameDrawnMasks<C extends FramedContainer>(
  containers: C[] | undefined,
  geometry: Partial<Record<keyof FrameGeometry, unknown>> | null | undefined,
  width: number | undefined,
  height: number | undefined,
): C[] | undefined {
  return stampFrames(containers, shapeFrame(geometry, width, height) ?? frameGeometry(geometry), true);
}
