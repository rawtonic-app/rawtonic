// SPDX-License-Identifier: AGPL-3.0-or-later
import { type ReactNode } from 'react';
import { cssTransition, ToastContainer } from 'react-toastify';
import { X } from 'lucide-react';
import { useTranslation } from 'react-i18next';

/** A short fade, as the studio has no decorative motion (DESIGN.md); with
 * reduced motion the CSS makes it instant. */
const Fade = cssTransition({ enter: 'app-notice-enter', exit: 'app-notice-exit', collapseDuration: 120 });

function CloseButton({ closeToast }: { closeToast(): void }) {
  const { t } = useTranslation();
  return (
    <button type="button" className="app-notice-close" aria-label={t('notifications.close')} onClick={closeToast}>
      <X size={14} aria-hidden />
    </button>
  );
}

/** The one place for transient messages: toasts and, closest to the corner,
 * persistent notices such as the update notice, in one bottom-right column
 * so they stack instead of covering each other. */
export default function AppNotices({ children }: { children?: ReactNode }) {
  const { t } = useTranslation();
  return (
    <div className="app-notices">
      <ToastContainer
        position="bottom-right"
        autoClose={5000}
        hideProgressBar
        newestOnTop={false}
        // Toasts may hold an action (undo's "Skip"); only the close button closes.
        closeOnClick={false}
        pauseOnFocusLoss
        pauseOnHover
        draggable={false}
        role="status"
        theme="dark"
        transition={Fade}
        closeButton={CloseButton}
        aria-label={t('notifications.region')}
        toastClassName={(context) => `${context?.defaultClassName ?? ''} app-notice app-notice--${context?.type ?? 'default'}`}
      />
      {children}
    </div>
  );
}
