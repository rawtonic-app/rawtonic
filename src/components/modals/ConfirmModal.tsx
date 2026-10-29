import { useEffect, useState, useCallback, useRef } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import Button from '../ui/Button';

interface ConfirmModalProps {
  cancelText?: string;
  confirmText?: string;
  confirmVariant?: string;
  isOpen: boolean;
  message?: string;
  onClose(): void;
  onConfirm?(): void;
  title?: string;
}

export default function ConfirmModal({
  cancelText,
  confirmText,
  confirmVariant = 'primary',
  isOpen,
  message,
  onClose,
  onConfirm,
  title,
}: ConfirmModalProps) {
  const { t } = useTranslation();
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);
  const confirmRef = useRef<HTMLButtonElement>(null);
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

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        e.stopPropagation();
        e.nativeEvent.stopImmediatePropagation();
        if (!destructive || e.target === confirmRef.current) {
          handleConfirm();
        } else {
          onClose();
        }
      } else if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        e.nativeEvent.stopImmediatePropagation();
        onClose();
      }
    },
    [destructive, handleConfirm, onClose],
  );

  if (!isMounted) {
    return null;
  }

  const modalContent = (
    <div
      aria-labelledby="confirm-modal-title"
      aria-modal="true"
      className={`studio-dialog-backdrop transition-opacity duration-300 ease-in-out ${show ? 'opacity-100' : 'opacity-0'}`}
      onClick={onClose}
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
            onClick={onClose}
            variant="ghost"
            tabIndex={0}
            autoFocus={destructive}
          >
            {resolvedCancelText}
          </Button>
          <Button onClick={handleConfirm} variant={confirmVariant} autoFocus={!destructive} ref={confirmRef}>
            {resolvedConfirmText}
          </Button>
        </div>
      </div>
    </div>
  );

  return typeof document !== 'undefined' ? createPortal(modalContent, document.body) : modalContent;
}
