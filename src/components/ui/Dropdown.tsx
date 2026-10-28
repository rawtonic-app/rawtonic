import { useState, useEffect, useRef, useMemo, useId } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import { Check, ChevronDown } from 'lucide-react';
import Input from './Input';
import Text from './Text';
import { useSettingField } from './settingField';
import { TEXT_COLOR_KEYS, TextColors, TextVariants, TextWeights } from '../../types/typography';
import clsx from 'clsx';

export interface OptionItem<T extends React.Key> {
  disabled?: boolean;
  label: string;
  value: T;
}

interface DropdownProps<T extends React.Key> {
  className?: string;
  onChange: (value: T) => void;
  options: Array<OptionItem<T>>;
  placeholder?: string;
  searchPlaceholder?: string;
  value: T | null;
  disabled?: boolean;
  triggerClassName?: string;
}

/**
 * A select-only combobox (WAI-ARIA APG): focus stays on the trigger (or the
 * filter field while typing), the arrow keys move the active option, Enter or
 * Space chooses it and Escape closes the list without leaving the dialog it
 * sits in.
 */
const Dropdown = <T extends React.Key>({
  className = '',
  onChange,
  options,
  placeholder = 'Select an option',
  searchPlaceholder = 'Filter options...',
  value,
  disabled = false,
  triggerClassName = '',
}: DropdownProps<T>) => {
  const [isOpen, setIsOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const [searchTerm, setSearchTerm] = useState('');
  const [showSearch, setShowSearch] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const baseId = useId();
  const listboxId = `${baseId}-listbox`;
  const optionId = (index: number) => `${baseId}-option-${index}`;
  const selectedOption = options.find((opt) => opt.value === value) || null;
  // In a preference row the setting names the combobox; the chosen value is
  // its value, not its name.
  const field = useSettingField();

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        setIsOpen(false);
      }
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
    };
  }, []);

  useEffect(() => {
    if (!isOpen) {
      setSearchTerm('');
      setShowSearch(false);
      setActiveIndex(-1);
    }
  }, [isOpen]);

  const filteredOptions = useMemo(() => {
    if (!searchTerm) return options;
    return options.filter((opt) => opt.label.toLowerCase().includes(searchTerm.toLowerCase()));
  }, [options, searchTerm]);

  // The next enabled option from `from` in direction `step`, or -1.
  const enabledFrom = (from: number, step: 1 | -1): number => {
    for (let i = from; i >= 0 && i < filteredOptions.length; i += step) {
      if (!filteredOptions[i].disabled) return i;
    }
    return -1;
  };
  const firstEnabled = () => enabledFrom(0, 1);
  const lastEnabled = () => enabledFrom(filteredOptions.length - 1, -1);

  // Typing filters the list; the first match becomes the active option.
  useEffect(() => {
    if (isOpen && searchTerm) setActiveIndex(enabledFrom(0, 1));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchTerm]);

  useEffect(() => {
    if (!isOpen || activeIndex < 0) return;
    document.getElementById(optionId(activeIndex))?.scrollIntoView?.({ block: 'nearest' });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOpen, activeIndex]);

  const open = (edge?: 'first' | 'last') => {
    const selected = options.findIndex((opt) => opt.value === value && !opt.disabled);
    setActiveIndex(
      selected >= 0
        ? selected
        : edge === 'last'
          ? enabledFrom(options.length - 1, -1)
          : enabledFrom(0, 1),
    );
    setIsOpen(true);
  };

  const close = () => {
    setIsOpen(false);
    triggerRef.current?.focus();
  };

  const handleSelect = (option: OptionItem<T>) => {
    if (option.disabled) return;
    onChange(option.value);
    close();
  };

  const isPrintableKey = (e: React.KeyboardEvent<Element>): boolean => {
    if (e.metaKey || e.ctrlKey || e.altKey) return false;
    return e.key.length === 1;
  };

  const consume = (e: React.KeyboardEvent) => {
    e.stopPropagation();
    e.preventDefault();
  };

  const handleContainerKeyDown = (e: React.KeyboardEvent) => {
    if (disabled) return;
    const inSearch = e.target === searchInputRef.current;

    if (e.key === 'Escape') {
      // A closed list leaves Escape to the dialog or view around it.
      if (!isOpen) return;
      consume(e);
      close();
      return;
    }

    if (e.key === 'Tab') {
      if (isOpen) setIsOpen(false);
      return;
    }

    if (!isOpen) {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp' || e.key === 'Enter' || e.key === ' ') {
        consume(e);
        open(e.key === 'ArrowUp' ? 'last' : 'first');
        return;
      }
      if (e.key === 'Home' || e.key === 'End') {
        consume(e);
        open(e.key === 'End' ? 'last' : 'first');
        return;
      }
    } else {
      switch (e.key) {
        case 'ArrowDown': {
          consume(e);
          if (e.altKey) return;
          const next = activeIndex < 0 ? firstEnabled() : enabledFrom(activeIndex + 1, 1);
          if (next >= 0) setActiveIndex(next);
          return;
        }
        case 'ArrowUp': {
          consume(e);
          if (e.altKey) {
            close();
            return;
          }
          const previous = activeIndex < 0 ? lastEnabled() : enabledFrom(activeIndex - 1, -1);
          if (previous >= 0) setActiveIndex(previous);
          return;
        }
        case 'Home':
        case 'End': {
          // In the filter field they move the caret.
          if (inSearch) return;
          consume(e);
          const edge = e.key === 'Home' ? firstEnabled() : lastEnabled();
          if (edge >= 0) setActiveIndex(edge);
          return;
        }
        case 'PageDown':
        case 'PageUp': {
          consume(e);
          const step = e.key === 'PageDown' ? 1 : -1;
          let target = activeIndex;
          for (let moved = 0, i = activeIndex + step; moved < 10 && i >= 0 && i < filteredOptions.length; i += step) {
            if (!filteredOptions[i].disabled) {
              target = i;
              moved++;
            }
          }
          if (target >= 0) setActiveIndex(target);
          return;
        }
        case 'Enter': {
          consume(e);
          const option = filteredOptions[activeIndex];
          if (option) handleSelect(option);
          return;
        }
        case ' ': {
          // A space in the filter field is part of the search text.
          if (inSearch) return;
          consume(e);
          const option = filteredOptions[activeIndex];
          if (option) handleSelect(option);
          return;
        }
      }
    }

    if (inSearch) return;

    if (isPrintableKey(e)) {
      consume(e);
      if (!isOpen) open();
      setShowSearch(true);
      setSearchTerm((prev) => prev + e.key);
    }
  };

  const activeDescendant = isOpen && filteredOptions[activeIndex] ? optionId(activeIndex) : undefined;

  return (
    <div className={`relative ${className}`} ref={dropdownRef} onKeyDown={handleContainerKeyDown}>
      <button
        ref={triggerRef}
        role="combobox"
        aria-activedescendant={showSearch ? undefined : activeDescendant}
        aria-controls={isOpen ? listboxId : undefined}
        aria-expanded={isOpen}
        aria-haspopup="listbox"
        aria-labelledby={field?.labelId}
        aria-describedby={field?.descriptionId}
        disabled={disabled}
        className={clsx(
          'w-full border border-border-color rounded-md px-3 mr-4 py-2 flex justify-between items-center text-left disabled:opacity-50 disabled:cursor-not-allowed',
          'focus:ring-accent focus:border-accent focus:outline-hidden focus:ring-2',
          triggerClassName || 'bg-surface',
        )}
        onClick={() => (isOpen ? setIsOpen(false) : open())}
        // Space is handled on keydown; its keyup must not click the trigger again.
        onKeyUp={(e) => e.key === ' ' && e.preventDefault()}
        type="button"
      >
        <Text as="span" variant={TextVariants.label} color={TextColors.primary}>
          {selectedOption ? selectedOption.label : placeholder}
        </Text>
        <ChevronDown
          className={`${TEXT_COLOR_KEYS[TextColors.secondary]} transition-transform duration-200 ${isOpen ? 'rotate-180' : ''}`}
          size={20}
        />
      </button>

      <AnimatePresence>
        {isOpen && (
          <motion.div
            animate={{ opacity: 1, scale: 1 }}
            className="absolute right-0 mt-2 w-full origin-top-right z-20"
            exit={{ opacity: 0, scale: 0.95 }}
            initial={{ opacity: 0, scale: 0.95 }}
            transition={{ duration: 0.1, ease: 'easeOut' }}
          >
            <div className="bg-surface/95 backdrop-blur-md rounded-lg shadow-xl p-2 max-h-80 overflow-y-auto">
              {showSearch && (
                <Input
                  ref={searchInputRef}
                  role="combobox"
                  aria-activedescendant={activeDescendant}
                  aria-autocomplete="list"
                  aria-controls={listboxId}
                  aria-expanded={true}
                  aria-label={searchPlaceholder}
                  value={searchTerm}
                  onChange={(e) => setSearchTerm(e.target.value)}
                  placeholder={searchPlaceholder}
                  autoFocus={true}
                  className="mb-2"
                />
              )}

              <div aria-orientation="vertical" id={listboxId} role="listbox">
                {filteredOptions.map((option: OptionItem<T>, index) => {
                  const isSelected = value === option.value;
                  const isActive = index === activeIndex;
                  return (
                    <button
                      key={option.value}
                      id={optionId(index)}
                      // The options are not tab stops: the arrow keys move between them.
                      tabIndex={-1}
                      disabled={option.disabled}
                      onClick={() => handleSelect(option)}
                      onMouseDown={(e) => e.preventDefault()}
                      onMouseMove={() => !isActive && !option.disabled && setActiveIndex(index)}
                      className={clsx(
                        'w-full text-left px-3 py-2 rounded-md flex items-center justify-between',
                        'transition-colors duration-150 hover:bg-bg-primary',
                        'disabled:opacity-50 disabled:cursor-not-allowed',
                        {
                          'bg-bg-primary': isActive || (isSelected && activeIndex < 0),
                        },
                      )}
                      type="button"
                      data-active={isActive || undefined}
                      role="option"
                      aria-disabled={option.disabled || undefined}
                      aria-selected={isSelected}
                    >
                      <Text color={TextColors.primary} weight={isSelected ? TextWeights.semibold : TextWeights.normal}>
                        {option.label}
                      </Text>
                      {isSelected && <Check size={16} className={TEXT_COLOR_KEYS[TextColors.primary]} />}
                    </button>
                  );
                })}
              </div>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
};

export default Dropdown;
