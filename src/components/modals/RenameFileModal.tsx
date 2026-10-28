import { useState, useEffect, useCallback, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { FILENAME_VARIABLES } from '../ui/ExportImportProperties';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';

interface RenameFileModalProps {
  filesToRename: Array<string>;
  isOpen: boolean;
  onClose(): void;
  onSave(template: string): void;
}

export default function RenameFileModal({ filesToRename, isOpen, onClose, onSave }: RenameFileModalProps) {
  const { t } = useTranslation();
  const [nameTemplate, setNameTemplate] = useState('');
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);
  const nameInputRef = useRef<HTMLInputElement>(null);

  const fileCount = filesToRename.length;
  const isSingleFile = fileCount === 1;

  useEffect(() => {
    if (isOpen) {
      if (isSingleFile && filesToRename[0]) {
        const fileName = filesToRename[0].split(/[\\/]/).pop();
        const nameWithoutExt = fileName?.substring(0, fileName.lastIndexOf('.'));
        if (nameWithoutExt) {
          setNameTemplate(nameWithoutExt);
        }
      } else {
        setNameTemplate('{original_filename}');
      }
      setIsMounted(true);
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    } else {
      setShow(false);
      const timer = setTimeout(() => {
        setIsMounted(false);
        setNameTemplate('');
      }, 300);
      return () => clearTimeout(timer);
    }
  }, [isOpen, filesToRename, isSingleFile]);

  const handleSave = useCallback(() => {
    const trimmed = nameTemplate.trim();
    if (trimmed) {
      let finalTemplate = trimmed;
      if (!isSingleFile && !finalTemplate.includes('{sequence}') && !finalTemplate.includes('{original_filename}')) {
        finalTemplate = `${finalTemplate}_{sequence}`;
      }
      onSave(finalTemplate);
      onClose();
    }
  }, [nameTemplate, onSave, onClose, isSingleFile]);

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

  const handleVariableClick = (variable: string) => {
    if (!nameInputRef.current) {
      return;
    }
    const input = nameInputRef.current;
    const start = input?.selectionStart || 0;
    const end = input?.selectionEnd || 0;
    const currentValue = input.value;
    const newValue = currentValue.substring(0, start) + variable + currentValue.substring(end);
    setNameTemplate(newValue);
    setTimeout(() => {
      input.focus();
      const newCursorPos = start + variable.length;
      input.setSelectionRange(newCursorPos, newCursorPos);
    }, 0);
  };

  if (!isMounted) {
    return null;
  }

  return (
    <div
      aria-modal="true"
      className={`studio-dialog-backdrop transition-opacity duration-300 ease-in-out ${
        show ? 'opacity-100' : 'opacity-0'
      }`}
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
        <h2 className="studio-dialog-title">
          {isSingleFile
            ? t('modals.renameFile.titleSingle')
            : t('modals.renameFile.titleMultiple', { count: fileCount })}
        </h2>

        <div>
          <div>
            <Text variant={TextVariants.label} className="block mb-2">
              {isSingleFile ? t('modals.renameFile.newName') : t('modals.renameFile.fileNamingTemplate')}
            </Text>
            <input
              autoFocus
              className="w-full px-2 py-1.5"
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setNameTemplate(e.target.value)}
              onKeyDown={handleKeyDown}
              ref={nameInputRef}
              type="text"
              value={nameTemplate}
            />
            {!isSingleFile && (
              <div className="flex flex-wrap gap-2 mt-2">
                {FILENAME_VARIABLES.map((variable: string) => (
                  <button
                    className="studio-button"
                    key={variable}
                    onClick={() => handleVariableClick(variable)}
                  >
                    {variable}
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>

        <div className="studio-dialog-footer">
          <button
            className="studio-button"
            onClick={onClose}
          >
            {t('modals.renameFile.cancel')}
          </button>
          <button
            className="studio-button is-primary"
            disabled={!nameTemplate.trim()}
            onClick={handleSave}
          >
            {t('modals.renameFile.save')}
          </button>
        </div>
      </div>
    </div>
  );
}
