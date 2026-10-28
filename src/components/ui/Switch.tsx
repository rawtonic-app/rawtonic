import React from 'react';
import clsx from 'clsx';
import { motion } from 'framer-motion';
import Text from './Text';
import { TextVariants } from '../../types/typography';
import { useSettingField } from './settingField';

interface SwitchProps {
  checked: boolean;
  className?: string;
  disabled?: boolean;
  id?: string;
  label: string;
  onChange(val: boolean): void;
  tooltip?: string;
  trackClassName?: string;
}

/**
 * A beautiful, reusable, and accessible toggle switch component.
 *
 * @param {string} label - The text label for the switch.
 * @param {boolean} checked - The current state of the switch.
 * @param {function(boolean): void} onChange - Callback function that receives the new boolean state.
 * @param {boolean} [disabled=false] - Whether the switch is interactive.
 * @param {string} [className=''] - Additional classes for the container.
 * @param {string} [trackClassName] - Custom classes for the switch's background track.
 */
const Switch = ({
  checked,
  className = '',
  disabled = false,
  label,
  onChange,
  tooltip,
  trackClassName,
}: SwitchProps) => {
  const uniqueId = `switch-${label.replace(/\s+/g, '-').toLowerCase()}`;
  // In a preference row the switch keeps its own label and is described by
  // the setting's name and description.
  const field = useSettingField();
  const describedBy = field ? [field.labelId, field.descriptionId].filter(Boolean).join(' ') : undefined;

  const spring = {
    type: 'spring',
    stiffness: 700,
    damping: 30,
  } as const;

  return (
    <label
      className={clsx(
        'flex items-center justify-between',
        disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer',
        className,
      )}
      htmlFor={uniqueId}
      data-tooltip={tooltip}
    >
      <Text variant={TextVariants.label} className="select-none switch-label">
        {label}
      </Text>
      <div className="relative w-10 h-5">
        <input
          checked={checked}
          className="sr-only peer"
          disabled={disabled}
          id={uniqueId}
          aria-describedby={describedBy}
          onChange={(e: React.ChangeEvent<HTMLInputElement>) => !disabled && onChange(e.target.checked)}
          type="checkbox"
        />
        <div
          className={clsx(
            'w-full h-full rounded-full border transition-colors',
            // The input is visually hidden; its keyboard focus shows on the track.
            'peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-[#a9c9e1]',
            checked ? 'bg-[#7d95a9] border-[#7d95a9]' : 'bg-[#3a3c40] border-[#4b4e53]',
            trackClassName,
          )}
        ></div>
        <motion.div
          className={clsx('absolute top-0.5 left-0.5 w-4 h-4 rounded-full shadow transition-colors', {
            'bg-white': checked,
            'bg-[#c9ccd1]': !checked,
          })}
          transition={spring}
          initial={false}
          animate={{ x: checked ? 20 : 0 }}
        />
      </div>
    </label>
  );
};

export default Switch;
