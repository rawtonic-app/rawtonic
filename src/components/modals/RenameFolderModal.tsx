import { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';

interface RenameFolderModalProps {
  currentName: string;
  isOpen: boolean;
  onClose(): void;
  onSave(name: string): void;
  title?: string;
  placeholder?: string;
  buttonText?: string;
}

export default function RenameFolderModal({
  currentName,
  isOpen,
  onClose,
  onSave,
  title,
  placeholder,
  buttonText,
}: RenameFolderModalProps) {
  const { t } = useTranslation();
  const [name, setName] = useState('');
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);

  useEffect(() => {
    if (isOpen) {
      setName(currentName || '');
      setIsMounted(true);
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    } else {
      setShow(false);
      const timer = setTimeout(() => {
        setIsMounted(false);
        setName('');
      }, 300);
      return () => clearTimeout(timer);
    }
  }, [isOpen, currentName]);

  const handleSave = useCallback(() => {
    const trimmed = name.trim();
    if (trimmed && trimmed !== currentName) {
      onSave(trimmed);
    }
    onClose();
  }, [name, currentName, onSave, onClose]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        e.stopPropagation();
        handleSave();
      } else if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    },
    [handleSave, onClose],
  );

  if (!isMounted) {
    return null;
  }

  return (
    <div
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
      >
        <h2 className="studio-dialog-title">
          {title || t('modals.renameFolder.title')}
        </h2>
        <input
          autoFocus
          className="w-full px-2 py-1.5"
          onChange={(e: React.ChangeEvent<HTMLInputElement>) => setName(e.target.value)}
          onKeyDown={handleKeyDown}
          onFocus={(e) => e.target.select()}
          placeholder={placeholder || t('modals.renameFolder.placeholder')}
          type="text"
          value={name}
        />
        <div className="studio-dialog-footer">
          <button
            className="studio-button"
            onClick={onClose}
          >
            {t('modals.renameFolder.cancel')}
          </button>
          <button
            className="studio-button is-primary"
            disabled={!name.trim() || name.trim() === currentName}
            onClick={handleSave}
          >
            {buttonText || t('modals.renameFolder.save')}
          </button>
        </div>
      </div>
    </div>
  );
}
