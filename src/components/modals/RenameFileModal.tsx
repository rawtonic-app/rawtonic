import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { FILENAME_VARIABLES } from '../ui/ExportImportProperties';
import { Invokes } from '../ui/AppProperties';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';

/** One file the rename moves (`file_management::rename_plan::PlannedFile`). */
export interface PlannedFile {
  from: string;
  to: string;
  role: 'photo' | 'editSidecar' | 'virtualCopy' | 'exifCache' | 'xmp' | 'companion';
}

type Note = { kind: 'xmpWithRaw'; xmp: string; others: string[] } | { kind: 'xmpStays'; xmp: string; owner: string };

type Problem =
  | { kind: 'notFound' }
  | { kind: 'invalidName'; name: string }
  | { kind: 'occupied'; path: string }
  | { kind: 'sameTarget'; path: string };

interface PlannedPhoto {
  from: string;
  to: string;
  files: PlannedFile[];
  notes: Note[];
  problems: Problem[];
}

/** The plan the rename runs, as the backend made it (`preview_rename`). */
export interface RenamePlan {
  photos: PlannedPhoto[];
}

/** Rows shown before the rest is summed up. */
export const PREVIEW_ROWS = 50;

interface RenameFileModalProps {
  filesToRename: Array<string>;
  isOpen: boolean;
  onClose(): void;
  /** Runs the previewed plan: `expected` is its files, refused if they changed since. */
  onSave(template: string, expected: PlannedFile[]): void;
}

const nameOf = (path: string) => path.split(/[\\/]/).pop() ?? path;

export default function RenameFileModal({ filesToRename, isOpen, onClose, onSave }: RenameFileModalProps) {
  const { t } = useTranslation();
  const [nameTemplate, setNameTemplate] = useState('');
  const [plan, setPlan] = useState<RenamePlan | null>(null);
  const [planFor, setPlanFor] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);
  const nameInputRef = useRef<HTMLInputElement>(null);
  const request = useRef(0);
  // The first preview of each opening reads the photos' facts again.
  const fresh = useRef(true);

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
      fresh.current = true;
      setIsMounted(true);
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    } else {
      setShow(false);
      const timer = setTimeout(() => {
        setIsMounted(false);
        setNameTemplate('');
        setPlan(null);
        setPlanFor(null);
      }, 300);
      return () => clearTimeout(timer);
    }
  }, [isOpen, filesToRename, isSingleFile]);

  // The final names, sidecars and conflicts for the current template,
  // planned by the backend exactly as Save runs it.
  const template = nameTemplate.trim();
  useEffect(() => {
    if (!isOpen || !template || !filesToRename.length) return;
    const id = ++request.current;
    const timer = setTimeout(() => {
      const refresh = fresh.current;
      fresh.current = false;
      invoke<RenamePlan>(Invokes.PreviewRename, { paths: filesToRename, nameTemplate: template, refresh })
        .then((next) => {
          if (id !== request.current) return;
          setPlan(next);
          setPlanFor(template);
          setPreviewError(null);
        })
        .catch((err) => {
          if (id !== request.current) return;
          setPlan(null);
          setPlanFor(null);
          setPreviewError(String(err));
        });
    }, 150);
    return () => clearTimeout(timer);
  }, [isOpen, template, filesToRename]);

  const current = plan && planFor === template ? plan : null;
  const files = useMemo(() => (current ? current.photos.flatMap((photo) => photo.files) : []), [current]);
  const blocked = current?.photos.some((photo) => photo.problems.length > 0) ?? false;
  const canSave = !!current && !blocked && files.length > 0;

  const handleSave = useCallback(() => {
    if (current && canSave) {
      onSave(template, files);
      onClose();
    }
  }, [current, canSave, template, files, onSave, onClose]);

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

  const describeNote = (note: Note) =>
    note.kind === 'xmpWithRaw'
      ? t('modals.renameFile.noteXmpWithRaw', { xmp: nameOf(note.xmp), others: note.others.map(nameOf).join(', ') })
      : t('modals.renameFile.noteXmpStays', { xmp: nameOf(note.xmp), owner: nameOf(note.owner) });

  const describeProblem = (problem: Problem) => {
    switch (problem.kind) {
      case 'notFound':
        return t('modals.renameFile.problemNotFound');
      case 'invalidName':
        return t('modals.renameFile.problemInvalidName', { name: problem.name });
      case 'occupied':
        return t('modals.renameFile.problemOccupied', { name: nameOf(problem.path) });
      case 'sameTarget':
        return t('modals.renameFile.problemSameTarget', { name: nameOf(problem.path) });
    }
  };

  if (!isMounted) {
    return null;
  }

  const shown = current ? current.photos.slice(0, PREVIEW_ROWS) : [];
  const hidden = current ? current.photos.length - shown.length : 0;
  const renamedPhotos = current ? current.photos.filter((photo) => photo.files.length > 0).length : 0;

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
        className={`studio-dialog is-wide transform transition-all duration-300 ease-out ${
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

        <div aria-label={t('modals.renameFile.preview')} role="region">
          <Text variant={TextVariants.label} className="block mb-1">
            {t('modals.renameFile.preview')}
          </Text>
          {previewError ? (
            <p className="text-red-400">{t('modals.renameFile.previewFailed', { error: previewError })}</p>
          ) : !current ? (
            <p className="text-text-secondary">{template ? t('modals.renameFile.checking') : ''}</p>
          ) : (
            <>
              <ul className="max-h-64 overflow-y-auto space-y-1" data-testid="rename-preview">
                {shown.map((photo) => {
                  const others = photo.files.filter((file) => file.role !== 'photo');
                  return (
                    <li key={photo.from}>
                      <div className="flex gap-2 min-w-0">
                        <span className="truncate">{nameOf(photo.from)}</span>
                        <span aria-hidden="true">→</span>
                        <span className={`truncate ${photo.problems.length ? 'text-red-400' : ''}`}>
                          {photo.files.length || photo.problems.length
                            ? nameOf(photo.to)
                            : t('modals.renameFile.unchanged')}
                        </span>
                      </div>
                      {others.length > 0 && (
                        <div className="text-xs text-text-secondary truncate">
                          {t('modals.renameFile.withFiles', { files: others.map((file) => nameOf(file.to)).join(', ') })}
                        </div>
                      )}
                      {photo.notes.map((note, i) => (
                        <div className="text-xs text-text-secondary" key={`n${i}`}>
                          {describeNote(note)}
                        </div>
                      ))}
                      {photo.problems.map((problem, i) => (
                        <div className="text-xs text-red-400" key={`p${i}`}>
                          {describeProblem(problem)}
                        </div>
                      ))}
                    </li>
                  );
                })}
              </ul>
              {hidden > 0 && (
                <p className="text-xs text-text-secondary mt-1">{t('modals.renameFile.more', { hidden })}</p>
              )}
              <p className="text-xs text-text-secondary mt-1">
                {blocked
                  ? t('modals.renameFile.blocked')
                  : files.length
                    ? t('modals.renameFile.summary', { photos: renamedPhotos, files: files.length })
                    : t('modals.renameFile.nothingChanges')}
              </p>
            </>
          )}
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
            disabled={!canSave}
            onClick={handleSave}
          >
            {t('modals.renameFile.save')}
          </button>
        </div>
      </div>
    </div>
  );
}
