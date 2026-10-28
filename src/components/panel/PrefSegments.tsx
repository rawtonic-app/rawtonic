import type { KeyboardEvent } from 'react';

/** A two-way choice in a preference row, as the studio's segmented control
 * (Develop tools Left | Right): one Tab stop, arrow keys move and choose. */
export default function PrefSegments<T extends string>({ label, onChange, options, value }: {
  label: string;
  onChange(value: T): void;
  options: { label: string; value: T }[];
  value: T;
}) {
  const keys = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (!step) return;
    e.preventDefault();
    const index = options.findIndex((option) => option.value === value);
    const next = (index + step + options.length) % options.length;
    onChange(options[next].value);
    e.currentTarget.querySelectorAll<HTMLButtonElement>('[role="radio"]')[next]?.focus();
  };
  return (
    <div className="studio-segments pref-segments" role="radiogroup" aria-label={label} onKeyDown={keys}>
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={value === option.value}
          tabIndex={value === option.value ? 0 : -1}
          onClick={() => onChange(option.value)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}
