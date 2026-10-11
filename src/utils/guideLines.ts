import type { GuideLine, GuideOrientation } from './adjustments';

/** Guided Upright: lines within 35° of an axis count for that axis. */
const MAX_TILT = Math.tan((35 * Math.PI) / 180);
/** Shortest guide on screen, in pixels. */
export const MIN_GUIDE_LENGTH = 15;
/** Up to two guides per axis feed the guided perspective. */
export const MAX_GUIDES_PER_AXIS = 2;

/** Vertical and horizontal guides are told apart by colour; a guide that
 * cannot be added is red. */
export const GUIDE_COLORS = { vertical: '#38bdf8', horizontal: '#f5a524', invalid: '#ef4444' } as const;

/** The axis of a guide drawn from (0, 0) to (dx, dy) on screen, or null when
 * it is too far from both. */
export function guideOrientation(dx: number, dy: number): GuideOrientation | null {
  if (Math.abs(dx) <= Math.abs(dy) * MAX_TILT) return 'vertical';
  if (Math.abs(dy) <= Math.abs(dx) * MAX_TILT) return 'horizontal';
  return null;
}

/** Why a guide of `type` cannot be added to `lines`, or null when it can. */
export function guideRejection(lines: GuideLine[], type: GuideOrientation | null, length: number) {
  if (length < MIN_GUIDE_LENGTH) return 'short' as const;
  if (!type) return 'angle' as const;
  if (lines.filter((l) => l.type === type).length >= MAX_GUIDES_PER_AXIS) return 'full' as const;
  return null;
}

/** "2 vertical · 1 horizontal" for the Upright panel. */
export function guideCount(lines: GuideLine[]) {
  const vertical = lines.filter((l) => l.type === 'vertical').length;
  const horizontal = lines.length - vertical;
  return `${vertical} vertical · ${horizontal} horizontal`;
}
