import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { exit } from '@tauri-apps/plugin-process';

import { useProcessStore } from '../store/useProcessStore';
import { currentEditForExport, useEditorStore } from '../store/useEditorStore';
import { Invokes } from '../components/ui/AppProperties';
import { ExportSettings, Status } from '../components/ui/ExportImportProperties';
import { debouncedSave } from './useEditorActions';
import { useLibraryStore } from '../store/useLibraryStore';
import { submittedFrom } from '../utils/runExportJobs';
import { allPhotoWritesSettled } from '../utils/photoWrites';

/**
 * Handles files handed to the app from outside (OS "open with" and the
 * external editor protocol: rawtonic-app --edit <file> --output <file>).
 * Opens the requested image in the editor and, for edit sessions, exports
 * the result to the caller-provided output path and exits the app.
 */
export function useExternalEditSession(handleImageSelect: (path: string) => void) {
  const initialFileToOpen = useProcessStore((state) => state.initialFileToOpen);
  const externalEditSession = useProcessStore((state) => state.externalEditSession);
  const exportQueue = useProcessStore((state) => state.exportQueue);
  const [isFinishing, setIsFinishing] = useState(false);
  /** The queued export of the finish (`export_images` returns its job id):
   * the app quits when this job is done, not when any export completes (a
   * batch queued before it may finish first). */
  const [finishJob, setFinishJob] = useState<string | null>(null);

  const handleImageSelectRef = useRef(handleImageSelect);
  useEffect(() => {
    handleImageSelectRef.current = handleImageSelect;
  });

  useEffect(() => {
    if (!initialFileToOpen) return;
    useProcessStore.getState().setProcess({ initialFileToOpen: null });
    handleImageSelectRef.current(initialFileToOpen);
  }, [initialFileToOpen]);

  useEffect(() => {
    if (!externalEditSession) return;
    setIsFinishing(false);
    handleImageSelectRef.current(externalEditSession.source);
  }, [externalEditSession]);

  // Whether the queue has listed the finish's job yet: one gone from it
  // after that was removed (from the Export queue), so Done may be pressed
  // again.
  const listedJob = useRef(false);
  useEffect(() => {
    if (!isFinishing || !finishJob) return;
    const job = exportQueue.find((j) => j.id === finishJob);
    if (job) listedJob.current = true;
    if (job?.state === 'done') {
      exit(0);
    } else if (job ? job.state !== 'queued' && job.state !== 'running' : listedJob.current) {
      setIsFinishing(false);
      setFinishJob(null);
    }
  }, [isFinishing, finishJob, exportQueue]);

  const finishExternalEdit = useCallback(async () => {
    const session = useProcessStore.getState().externalEditSession;
    const { selectedImage } = useEditorStore.getState();
    if (!session || !selectedImage) return;

    debouncedSave.flush();
    // Taken now: the edit and library at the click, the metadata once the
    // writes given before it (a rating set just before) are in.
    const edit = currentEditForExport(useEditorStore.getState());
    const library = submittedFrom(useLibraryStore.getState().sessionDirectory);

    const exportSettings: ExportSettings = {
      filenameTemplate: null,
      jpegQuality: session.jpegQuality,
      tiffBitDepth: 16,
      keepMetadata: true,
      preserveTimestamps: false,
      preserveFolders: false,
      destinationType: 'customFolder',
      subfolder: '',
      resize: null,
      border: null,
      pad: null,
      stripGps: false,
      exportMasks: false,
      watermark: null,
    };

    setIsFinishing(true);
    setFinishJob(null);
    listedJob.current = false;
    useProcessStore.getState().setExportState({
      status: Status.Exporting,
      progress: { current: 0, total: 1 },
      errorMessage: '',
    });

    try {
      await allPhotoWritesSettled();
      const job = await invoke<string>(Invokes.ExportImages, {
        paths: [session.source],
        outputFolderOrFile: session.output,
        isExplicitFilePath: true,
        baseOriginFolders: [],
        exportSettings,
        outputFormat: session.format,
        ...edit,
        library,
      });
      setFinishJob(job);
    } catch (error) {
      setIsFinishing(false);
      useProcessStore.getState().setExportState({
        status: Status.Error,
        errorMessage: typeof error === 'string' ? error : 'Export failed',
      });
    }
  }, []);

  return { externalEditSession, isFinishing, finishExternalEdit };
}
