// SPDX-License-Identifier: AGPL-3.0-or-later
import './activity.css';

/** A ring filled to `fraction`, or turning when the progress is unknown.
 * Prefer a known fraction while the GPU works: a turning ring makes the
 * window server composite the window on every display frame, which slowed
 * a batch export by a fifth (notes: parity-gaps/G43, task AG). */
export function ProgressRing({ fraction, size = 16 }: { fraction: number | null; size?: number }) {
  const r = 6;
  const length = 2 * Math.PI * r;
  return (
    <svg className={`activity-ring${fraction === null ? ' is-indeterminate' : ''}`} width={size} height={size} viewBox="0 0 16 16" aria-hidden>
      <circle className="activity-ring-track" cx="8" cy="8" r={r} />
      <circle
        className="activity-ring-value"
        cx="8"
        cy="8"
        r={r}
        strokeDasharray={length}
        strokeDashoffset={fraction === null ? length * 0.7 : length * (1 - Math.min(1, Math.max(0, fraction)))}
      />
    </svg>
  );
}
