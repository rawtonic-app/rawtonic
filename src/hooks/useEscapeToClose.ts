import { useEffect, useRef } from 'react';

/** Escape closes an open dialog, as Cancel does (macOS). `onClose` decides
 * whether it may close now (e.g. not while saving); an Escape another handler
 * already consumed (a context menu, a shortcut recorder) is left alone. */
export function useEscapeToClose(isOpen: boolean, onClose: () => void) {
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    if (!isOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      event.preventDefault();
      close.current();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [isOpen]);
}
