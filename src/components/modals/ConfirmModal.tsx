import { useEffect, useState, useCallback, useRef } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import Button from '../ui/Button';
import { overlayHost } from '../../utils/topLayer';

interface ConfirmModalProps {
  cancelText?: string;
  confirmText?: string;
  confirmVariant?: string;
  isOpen: boolean;
  message?: string;
  onClose(): void;
  /** Closed without a choice: Cancel, Escape or a click beside it. */
  onCancel?(): void;
  onConfirm?(): void;
  /** A third choice, between Cancel and the confirmation. */
  onSecondary?(): void;
  secondaryText?: string;
  secondaryVariant?: string;
  title?: string;
}

export default function ConfirmModal({
  cancelText,
  confirmText,
  confirmVariant = 'primary',
  isOpen,
  message,
  onClose,
  onCancel,
  onConfirm,
  onSecondary,
  secondaryText,
  secondaryVariant = 'secondary',
  title,
}: ConfirmModalProps) {
  const { t } = useTranslation();
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);
  const confirmRef = useRef<HTMLButtonElement>(null);
  const secondaryRef = useRef<HTMLButtonElement>(null);
  // A destructive confirmation (Delete Permanently, …) is never the default: Cancel
  // has the focus and Enter only confirms when the confirm button itself is focused,
  // as macOS alerts do for destructive actions.
  const destructive = confirmVariant === 'destructive';

  const resolvedCancelText = cancelText || t('modals.confirm.cancel');
  const resolvedConfirmText = confirmText || t('modals.confirm.confirm');

  useEffect(() => {
    if (isOpen) {
      setIsMounted(true);
      const timer = setTimeout(() => {
        setShow(true);
      }, 10);
      return () => clearTimeout(timer);
    } else {
      setShow(false);
      const timer = setTimeout(() => {
        setIsMounted(false);
      }, 300);
      return () => clearTimeout(timer);
    }
  }, [isOpen]);

  const handleConfirm = useCallback(() => {
    if (onConfirm) {
      onConfirm();
    }
    onClose();
  }, [onConfirm, onClose]);

  const handleCancel = useCallback(() => {
    onCancel?.();
    onClose();
  }, [onCancel, onClose]);

  const handleSecondary = useCallback(() => {
    onSecondary?.();
    onClose();
  }, [onSecondary, onClose]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        e.stopPropagation();
        e.nativeEvent.stopImmediatePropagation();
        if (secondaryRef.current && e.target === secondaryRef.current) {
          handleSecondary();
        } else if (!destructive || e.target === confirmRef.current) {
          handleConfirm();
        } else {
          handleCancel();
        }
      } else if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        e.nativeEvent.stopImmediatePropagation();
        handleCancel();
      }
    },
    [destructive, handleConfirm, handleCancel, handleSecondary],
  );

  if (!isMounted) {
    return null;
  }

  const modalContent = (
    <div
      aria-labelledby="confirm-modal-title"
      aria-modal="true"
      className={`studio-dialog-backdrop transition-opacity duration-300 ease-in-out ${show ? 'opacity-100' : 'opacity-0'}`}
      onClick={handleCancel}
      role="dialog"
    >
      <div
        className={`studio-dialog transform transition-all duration-300 ease-out ${
          show ? 'scale-100 opacity-100 translate-y-0' : 'scale-95 opacity-0 -translate-y-4'
        }`}
        onClick={(e: React.MouseEvent<HTMLDivElement>) => e.stopPropagation()}
        onKeyDown={handleKeyDown}
      >
        <h2 className="studio-dialog-title" id="confirm-modal-title">
          {title}
        </h2>
        <p className="m-0 whitespace-pre-wrap wrap-anywhere">{message}</p>
        <div className="studio-dialog-footer">
          <Button
            // The default button shows that it is: Enter cancels.
            className={destructive ? 'is-default' : undefined}
            onClick={handleCancel}
            variant="ghost"
            tabIndex={0}
            autoFocus={destructive}
          >
            {resolvedCancelText}
          </Button>
          {secondaryText && (
            <Button onClick={handleSecondary} variant={secondaryVariant} ref={secondaryRef}>
              {secondaryText}
            </Button>
          )}
          <Button onClick={handleConfirm} variant={confirmVariant} autoFocus={!destructive} ref={confirmRef}>
            {resolvedConfirmText}
          </Button>
        </div>
      </div>
    </div>
  );

  // Above a modal dialog it was asked from (Create library), not under it.
  return typeof document !== 'undefined' ? createPortal(modalContent, overlayHost()) : modalContent;
}
