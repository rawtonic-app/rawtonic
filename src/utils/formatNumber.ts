/** Format a display value, choosing its sign only after rounding. */
export function formatNumber(value: number, digits = 0, options: { trim?: boolean; signed?: boolean } = {}) {
  const fixed = value.toFixed(digits);
  const rounded = Number(fixed);
  const text = rounded === 0 ? (options.trim ? '0' : (0).toFixed(digits)) : options.trim ? String(rounded) : fixed;
  if (!options.signed || !Number.isFinite(rounded)) return text;
  return rounded < 0 ? `−${text.slice(1)}` : `+${text}`;
}
