import { useShallow } from 'zustand/react/shallow';
import { useUIStore } from '../../store/useUIStore';
import type { MergeKind } from '../../store/useUIStore';
import { closeMergeModal } from '../../hooks/useProductivityActions';
import { useProcessStore } from '../../store/useProcessStore';
import { useEditorStore } from '../../store/useEditorStore';
import MergeModal from './MergeModal';
import NegativeConversionModal from './NegativeConversionModal';
import CreateFolderModal from './CreateFolderModal';
import RenameFolderModal from './RenameFolderModal';
import RenameFileModal, { type PlannedFile } from './RenameFileModal';
import ConfirmModal from './ConfirmModal';
import LibrarySwitchShield from './LibrarySwitchShield';
import CullingModal from './CullingModal';
import CollageModal, { type CollageSpec } from './CollageModal';

export interface AppModalsProps {
  handleImageSelect: (path: string) => void;
  handleRunMerge: (kind: MergeKind, options: unknown) => void;
  handleSaveMerge: (autoSettings: boolean, fillEdges: boolean) => Promise<string>;
  handleRefinishMerge: (options: unknown) => Promise<void>;
  refreshImageList: () => Promise<void>;
  handleCreateFolder: (folderName: string) => Promise<void>;
  handleRenameFolder: (newName: string) => Promise<void>;
  handleSaveRename: (nameTemplate: string, expected: PlannedFile[]) => Promise<void>;
  handleSetColorLabel: (color: string | null, paths?: string[]) => Promise<void>;
  handleRate: (rating: number, paths?: string[]) => void;
  executeDelete: (paths: string[], options: { includeAssociated: boolean }) => Promise<void>;
  handleSaveCollage: (spec: CollageSpec) => Promise<string>;
}

export default function AppModals(props: AppModalsProps) {
  const {
    isCreateFolderModalOpen,
    isRenameFolderModalOpen,
    isRenameFileModalOpen,
    folderActionTarget,
    renameTargetPaths,
    confirmModalState,
    mergeModalState,
    negativeModalState,
    cullingModalState,
    collageModalState,
    setUI,
  } = useUIStore(
    useShallow((state) => ({
      isCreateFolderModalOpen: state.isCreateFolderModalOpen,
      isRenameFolderModalOpen: state.isRenameFolderModalOpen,
      isRenameFileModalOpen: state.isRenameFileModalOpen,
      folderActionTarget: state.folderActionTarget,
      renameTargetPaths: state.renameTargetPaths,
      confirmModalState: state.confirmModalState,
      mergeModalState: state.mergeModalState,
      negativeModalState: state.negativeModalState,
      cullingModalState: state.cullingModalState,
      collageModalState: state.collageModalState,
      setUI: state.setUI,
    })),
  );

  const { thumbnails } = useProcessStore(
    useShallow((state) => ({
      thumbnails: state.thumbnails,
    })),
  );

  const { selectedImage } = useEditorStore(
    useShallow((state) => ({
      selectedImage: state.selectedImage,
    })),
  );

  const closeConfirmModal = () => {
    setUI((state) => ({ confirmModalState: { ...state.confirmModalState, isOpen: false } }));
  };

  return (
    <>
      <MergeModal
        state={mergeModalState}
        thumbnail={
          mergeModalState.sourcePaths.length > 0
            ? thumbnails[mergeModalState.sourcePaths[Math.floor(mergeModalState.sourcePaths.length / 2)]] || null
            : null
        }
        onClose={closeMergeModal}
        onOpenFile={(path: string) => props.handleImageSelect(path)}
        onRun={props.handleRunMerge}
        onRefinish={props.handleRefinishMerge}
        onSave={props.handleSaveMerge}
      />
      <NegativeConversionModal
        isOpen={negativeModalState.isOpen}
        onClose={() => setUI((state) => ({ negativeModalState: { ...state.negativeModalState, isOpen: false } }))}
        targetPaths={negativeModalState.targetPaths}
        onSave={(savedPaths) => {
          props.refreshImageList().then(() => {
            if (selectedImage && negativeModalState.targetPaths.includes(selectedImage.path) && savedPaths.length > 0) {
              props.handleImageSelect(savedPaths[0]);
            }
          });
        }}
      />
      <CreateFolderModal
        isOpen={isCreateFolderModalOpen}
        onClose={() => setUI({ isCreateFolderModalOpen: false })}
        onSave={props.handleCreateFolder}
      />
      <RenameFolderModal
        currentName={folderActionTarget ? folderActionTarget.split(/[\\/]/).pop() || '' : ''}
        isOpen={isRenameFolderModalOpen}
        onClose={() => setUI({ isRenameFolderModalOpen: false })}
        onSave={props.handleRenameFolder}
      />
      <RenameFileModal
        filesToRename={renameTargetPaths}
        isOpen={isRenameFileModalOpen}
        onClose={() => setUI({ isRenameFileModalOpen: false })}
        onSave={props.handleSaveRename}
      />
      <ConfirmModal {...confirmModalState} onClose={closeConfirmModal} />
      <LibrarySwitchShield />
      <CullingModal
        isOpen={cullingModalState.isOpen}
        onClose={() =>
          setUI({
            cullingModalState: { isOpen: false, progress: null, suggestions: null, error: null, pathsToCull: [] },
          })
        }
        progress={cullingModalState.progress}
        suggestions={cullingModalState.suggestions}
        error={cullingModalState.error}
        imagePaths={cullingModalState.pathsToCull}
        thumbnails={thumbnails}
        onApply={(action, paths) => {
          if (action === 'reject') {
            props.handleSetColorLabel('red', paths);
          } else if (action === 'rate_zero') {
            props.handleRate(1, paths);
          } else if (action === 'delete') {
            props.executeDelete(paths, { includeAssociated: false });
          }
          setUI({
            cullingModalState: { isOpen: false, progress: null, suggestions: null, error: null, pathsToCull: [] },
          });
        }}
        onError={(err) => {
          setUI((state) => ({ cullingModalState: { ...state.cullingModalState, error: err, progress: null } }));
        }}
      />
      <CollageModal
        isOpen={collageModalState.isOpen}
        onClose={() => setUI({ collageModalState: { isOpen: false, sourceImages: [] } })}
        onSave={props.handleSaveCollage}
        sourceImages={collageModalState.sourceImages}
        thumbnails={thumbnails}
      />
    </>
  );
}
