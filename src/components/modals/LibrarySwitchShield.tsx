import { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { useUIStore } from '../../store/useUIStore';

/**
 * While a library switch opens the next library (`switchLibraryAfterSaves`),
 * a modal dialog makes the app inert: no click, key, shortcut or menu
 * command reaches the photo still shown, whose edits would belong to the
 * library being left. Its text shows only when opening takes a while.
 */
export default function LibrarySwitchShield() {
  const { t } = useTranslation();
  const switching = useUIStore((state) => state.switchingLibrary);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = dialog.current;
    if (!switching || !element) return;
    // jsdom has no showModal; the dialog is then simply open.
    if (!element.open) {
      if (element.showModal) element.showModal();
      else element.setAttribute('open', '');
    }
    return () => {
      if (element.close) element.close();
      else element.removeAttribute('open');
    };
  }, [switching]);
  if (!switching) return null;
  return (
    <dialog
      ref={dialog}
      className="library-switch-shield"
      aria-live="polite"
      onCancel={(event) => event.preventDefault()}
    >
      <p>{t('session.switching')}</p>
    </dialog>
  );
}
