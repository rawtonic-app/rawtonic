/** Shared mask/retouch cursor outlines, with widths in screen pixels. */
export function brushCursorOutlines(outer: number, inner?: number) {
  return [outer, inner].flatMap((radius, ring) =>
    radius === undefined || (ring === 0 && inner !== undefined && Math.abs(outer - inner) < 0.5)
      ? []
      : [
          { radius, stroke: 'rgba(0, 0, 0, 0.45)', strokeWidth: 3 },
          {
            radius,
            stroke: ring === 0 ? 'rgba(255, 255, 255, 0.6)' : 'rgba(255, 255, 255, 0.95)',
            strokeWidth: ring === 0 ? 1 : 1.5
          }
        ]
  );
}
