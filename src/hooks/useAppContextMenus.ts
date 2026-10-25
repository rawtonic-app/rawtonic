import { ComponentProps, useCallback, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  Aperture,
  Check,
  ClipboardPaste,
  Copy,
  CopyPlus,
  Edit,
  FileEdit,
  FileInput,
  Folder,
  Sparkles,
  FolderInput,
  FolderPlus,
  Images,
  LayoutTemplate,
  LayersArrowDown,
  Redo,
  RotateCcw,
  Star,
  SquaresUnite,
  Palette,
  Tag,
  Trash2,
  Undo,
  X,
  Users,
  Gauge,
  Layers,
  Film,
  Plane,
  Mountain,
  Sun,
  Camera,
  Map,
  Heart,
  Car,
  Briefcase,
  User,
  Album as AlbumIcon,
  PencilSparkles,
  Ruler,
} from 'lucide-react';
import { toast } from 'react-toastify';
import { loadPresetLibrary, usePresetStore, type UserPreset } from './usePresets';
import { useTranslation } from 'react-i18next';
import { useContextMenu } from '../context/ContextMenuContext';
import { useEditorStore } from '../store/useEditorStore';
import { isRawInLibrary, useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore } from '../store/useProcessStore';
import { CLOSED_MERGE_MODAL, useUIStore } from '../store/useUIStore';
import { useSettingsStore } from '../store/useSettingsStore';
import {
  Invokes,
  Option,
  OPTION_SEPARATOR,
  type RelinkReport,
  Panel,
  AlbumItem,
  Album,
  AlbumGroup,
  ImageMetadata,
} from '../components/ui/AppProperties';
import {
  Color,
  COLOR_LABELS,
  initialAdjustmentsFor,
  normalizeLoadedAdjustments,
  withPresetClarityVersion,
} from '../utils/adjustments';
import TaggingSubMenu from '../context/TaggingSubMenu';
import SessionAlbumSubMenu from '../components/studio/SessionAlbumSubMenu';
import { SESSION_ALBUM_PREFIX, SESSION_ALBUMS_CHANGED } from '../components/studio/StudioAlbums';
import { isCurrentEditorImage, useEditorActions } from './useEditorActions';
import { saveFailure, useLibraryActions } from './useLibraryActions';
import { globalImageCache } from '../utils/ImageLRUCache';
import { runAutoStraighten } from '../utils/autoStraighten';
import { previewsMenu } from '../utils/previews';
import { MISSING_VIEW_ID, offlineMenu } from '../utils/offline';
import { PREVIOUS_IMPORT_VIEW_ID } from '../utils/folderSync';
import { noteTool } from '../components/contribute/usageStats';
import { invokeRecipeWrite, ViewOnlyRefusal } from '../utils/editGate';

type TaggingSubMenuProps = Omit<ComponentProps<typeof TaggingSubMenu>, 'hideContextMenu'>;

export interface UseAppContextMenusProps {
  handleImageSelect: (path: string, openInEditor?: boolean) => void;
  handleBackToLibrary: () => void;
  handleRenameFiles: (paths: string[]) => void;
  /** Capture One's Move to Selects for the selection (sessions only). */
  handleMoveToFolder?: (paths: string[], destination: string) => Promise<void>;
  handleLibraryRefresh: () => Promise<void>;
  refreshImageList: () => Promise<void>;
  executeDelete: (paths: string[], options: { includeAssociated: boolean }) => Promise<void>;
  /** Shows the session's Missing Photographs view. */
  handleShowMissing?: () => void;
  /** After Locate Photo / Locate Folder (else the view is refreshed). */
  handleRelinked?: (report: RelinkReport) => Promise<void>;
}

export function useAppContextMenus(props: UseAppContextMenusProps) {
  const { t } = useTranslation();
  const { showContextMenu } = useContextMenu();
  useEffect(() => {
    // The Library's Apply Preset menu needs the list before Develop opens.
    if (usePresetStore.getState().presets.length === 0) void loadPresetLibrary();
  }, []);

  const {
    handleAutoAdjustments,
    handleAutoLensCorrection,
    handleAutoStraighten,
    handleResetAdjustments,
    handleCopyAdjustments,
    handlePasteAdjustments,
    applyAdjustmentsToPaths,
  } = useEditorActions();
  const { handleRate, handleSetColorLabel, handleTagsChanged } = useLibraryActions();

  /** Capture One's Styles / Lightroom's Saved Preset for the selection, in the Library. */
  const presetMenu = (items: UserPreset[], paths: string[]): Option => {
    const entry = (preset: { id: string; name: string; adjustments: unknown }): Option => ({
      label: preset.name,
      onClick: () => {
        applyAdjustmentsToPaths(withPresetClarityVersion(structuredClone(preset.adjustments) as never), paths);
        noteTool('presets');
      },
    });
    const submenu = items.flatMap((item): Option[] =>
      item.preset
        ? [entry(item.preset)]
        : item.folder && item.folder.children.length
          ? [{ label: item.folder.name ?? '', icon: Folder, submenu: item.folder.children.map(entry) }]
          : [],
    );
    return {
      icon: Sparkles,
      label: t('editor.masks.actions.applyPreset'),
      disabled: submenu.length === 0,
      submenu: submenu.length ? submenu : [{ label: t('editor.masks.actions.noPresets'), disabled: true }],
    };
  };

  const albumIcons = useMemo(
    () => [
      { label: t('contextMenus.albumIcons.default'), value: undefined, icon: Folder },
      { label: t('contextMenus.albumIcons.travel'), value: 'plane', icon: Plane },
      { label: t('contextMenus.albumIcons.nature'), value: 'mountain', icon: Mountain },
      { label: t('contextMenus.albumIcons.summer'), value: 'sun', icon: Sun },
      { label: t('contextMenus.albumIcons.photography'), value: 'camera', icon: Camera },
      { label: t('contextMenus.albumIcons.locations'), value: 'map', icon: Map },
      { label: t('contextMenus.albumIcons.favorites'), value: 'heart', icon: Heart },
      { label: t('contextMenus.albumIcons.featured'), value: 'star', icon: Star },
      { label: t('contextMenus.albumIcons.people'), value: 'users', icon: Users },
      { label: t('contextMenus.albumIcons.person'), value: 'user', icon: User },
      { label: t('contextMenus.albumIcons.automotive'), value: 'car', icon: Car },
      { label: t('contextMenus.albumIcons.portfolio'), value: 'briefcase', icon: Briefcase },
    ],
    [t],
  );

  const getCommonTags = useCallback((paths: string[]): { tag: string; isUser: boolean }[] => {
    const { imageList } = useLibraryStore.getState();
    if (paths.length === 0) return [];
    const imageFiles = imageList.filter((img) => paths.includes(img.path));
    if (imageFiles.length === 0) return [];

    const allTagsSets = imageFiles.map((img) => {
      const tagsWithPrefix = (img.tags || []).filter((t: string) => !t.startsWith('color:'));
      return new Set(tagsWithPrefix);
    });

    if (allTagsSets.length === 0) return [];

    const commonTagsWithPrefix = allTagsSets.reduce((intersection, currentSet) => {
      return new Set([...intersection].filter((tag) => currentSet.has(tag)));
    });

    return Array.from(commonTagsWithPrefix)
      .map((tag: string) => ({
        tag: tag.startsWith('user:') ? tag.substring(5) : tag,
        isUser: tag.startsWith('user:'),
      }))
      .sort((a, b) => a.tag.localeCompare(b.tag));
  }, []);

  const buildAddToAlbumMenu = useCallback(
    (items: AlbumItem[], pathsToAdd: string[]): Option[] => {
      return items.map((item) => {
        const customIconDef = item.icon ? albumIcons.find((i) => i.value === item.icon) : null;
        const ResolvedIcon = customIconDef?.icon || (item.type === 'group' ? Folder : AlbumIcon);

        if (item.type === 'group') {
          return {
            label: item.name,
            icon: ResolvedIcon,
            submenu:
              (item as AlbumGroup).children.length > 0
                ? buildAddToAlbumMenu((item as AlbumGroup).children, pathsToAdd)
                : [{ label: t('contextMenus.album.emptyGroup'), disabled: true }],
          };
        } else {
          return {
            label: item.name,
            icon: ResolvedIcon,
            onClick: () => {
              invoke(Invokes.AddToAlbum, { albumId: item.id, paths: pathsToAdd })
                .then(() => {
                  console.log(`Added image(s) to ${item.name}`);
                  invoke<AlbumItem[]>(Invokes.GetAlbums).then((res) =>
                    useLibraryStore.getState().setLibrary({ albumTree: res }),
                  );
                })
                .catch((err) => toast.error(t('contextMenus.toasts.failedAddToAlbum', { err })));
            },
          };
        }
      });
    },
    [albumIcons, t],
  );

  const handleEditorContextMenu = useCallback(
    (event: React.MouseEvent) => {
      event.preventDefault();
      event.stopPropagation();

      const { selectedImage, history, historyIndex, historyPending, undo, redo, resetHistory, copiedAdjustments, setEditor } =
        useEditorStore.getState();
      const { appSettings } = useSettingsStore.getState();
      const { setPanel, setUI } = useUIStore.getState();

      if (!selectedImage) return;

      const canUndo = historyPending || historyIndex > 0;
      const canRedo = !historyPending && historyIndex < history.length - 1;
      const commonTags = getCommonTags([selectedImage.path]);

      const options: Array<Option> = [
        {
          label: t('contextMenus.editor.exportImage'),
          icon: FileInput,
          onClick: () => setPanel(Panel.Export),
        },
        { type: OPTION_SEPARATOR },
        { label: t('contextMenus.editor.undo'), icon: Undo, onClick: undo, disabled: !canUndo },
        { label: t('contextMenus.editor.redo'), icon: Redo, onClick: redo, disabled: !canRedo },
        { type: OPTION_SEPARATOR },
        {
          label: t('contextMenus.editor.copyAdjustments'),
          icon: Copy,
          onClick: () => handleCopyAdjustments(),
        },
        {
          label: t('contextMenus.editor.pasteAdjustments'),
          icon: ClipboardPaste,
          onClick: () => handlePasteAdjustments(),
          disabled: copiedAdjustments === null,
        },
        {
          label: t('contextMenus.editor.productivity'),
          icon: Gauge,
          submenu: [
            {
              label: t('contextMenus.editor.autoAdjust'),
              icon: PencilSparkles,
              onClick: handleAutoAdjustments,
              disabled: !selectedImage?.isReady,
            },
            {
              label: t('contextMenus.editor.autoLensCorrection'),
              icon: Aperture,
              onClick: () => handleAutoLensCorrection([selectedImage.path]),
              disabled: !selectedImage?.isReady,
            },
            {
              label: t('contextMenus.editor.autoStraighten'),
              icon: Ruler,
              onClick: () => void runAutoStraighten(handleAutoStraighten),
              disabled: !selectedImage?.isReady,
            },
            {
              label: t('contextMenus.editor.convertNegative'),
              icon: Film,
              onClick: () => {
                if (selectedImage) {
                  setUI({ negativeModalState: { isOpen: true, targetPaths: [selectedImage.path] } });
                }
              },
            },
            {
              icon: LayoutTemplate,
              label: t('contextMenus.editor.frameImage'),
              onClick: () => {
                setUI({ collageModalState: { isOpen: true, sourceImages: [selectedImage] } });
              },
            },
            { label: t('contextMenus.editor.cullImage'), icon: Users, disabled: true },
          ],
        },
        {
          label: t('contextMenus.merge.title'),
          icon: LayersArrowDown,
          submenu: [
            { disabled: true, icon: SquaresUnite, label: t('contextMenus.editor.stitchPanorama') },
            { disabled: true, icon: Images, label: t('contextMenus.editor.mergeHdr') },
            { disabled: true, icon: SquaresUnite, label: t('contextMenus.merge.hdrPanorama') },
            { disabled: true, icon: Layers, label: t('contextMenus.merge.focusStack') },
          ],
        },
        { type: OPTION_SEPARATOR },
        {
          label: t('contextMenus.editor.rating'),
          icon: Star,
          submenu: [0, 1, 2, 3, 4, 5].map((rating: number) => ({
            label:
              rating === 0
                ? t('contextMenus.editor.noRating')
                : t('contextMenus.editor.ratingLabel', { count: rating }),
            onClick: () => handleRate(rating),
          })),
        },
        {
          label: t('contextMenus.editor.colorLabel'),
          icon: Palette,
          submenu: [
            { label: t('contextMenus.editor.noLabel'), onClick: () => handleSetColorLabel(null) },
            ...COLOR_LABELS.map((label: Color) => ({
              label: t(`contextMenus.colors.${label.name}`, { defaultValue: `contextMenus.colors.${label.name}` }),
              color: label.color,
              onClick: () => handleSetColorLabel(label.name),
            })),
          ],
        },
        {
          label: t('contextMenus.editor.tagging'),
          icon: Tag,
          submenu: [
            {
              customComponent: TaggingSubMenu,
              customProps: {
                paths: [selectedImage.path],
                initialTags: commonTags,
                onTagsChanged: handleTagsChanged,
                appSettings,
              } satisfies TaggingSubMenuProps,
            },
          ],
        },
        { type: OPTION_SEPARATOR },
        {
          label: t('contextMenus.editor.resetAdjustments'),
          icon: RotateCcw,
          submenu: [
            { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
            {
              label: t('contextMenus.editor.confirmReset'),
              icon: Check,
              isDestructive: true,
              onClick: () => {
                const originalAspectRatio =
                  selectedImage.width && selectedImage.height ? selectedImage.width / selectedImage.height : null;
                const resetData = {
                  ...initialAdjustmentsFor(!!selectedImage.isRaw),
                  aspectRatio: originalAspectRatio,
                  aiPatches: [],
                };
                resetHistory(resetData);
                setEditor({ adjustments: resetData });
              },
            },
          ],
        },
      ];
      showContextMenu(event.clientX, event.clientY, options);
    },
    [
      getCommonTags,
      handleCopyAdjustments,
      handlePasteAdjustments,
      handleAutoAdjustments,
      handleAutoLensCorrection,
      handleAutoStraighten,
      handleRate,
      handleSetColorLabel,
      handleTagsChanged,
      showContextMenu,
      t,
    ],
  );

  const handleThumbnailContextMenu = useCallback(
    (event: React.MouseEvent, path: string, forceSingleSelection: boolean = false) => {
      event.preventDefault();
      event.stopPropagation();

      const { selectedImage, copiedAdjustments, setEditor } = useEditorStore.getState();
      const { multiSelectedPaths, imageList, libraryActivePath, albumTree, activeAlbumId, setLibrary, sessionDirectory } =
        useLibraryStore.getState();
      const { appSettings } = useSettingsStore.getState();
      const { activeView, setUI, setPanel } = useUIStore.getState();
      const { setProcess } = useProcessStore.getState();

      const isTargetInSelection = multiSelectedPaths.includes(path);
      let finalSelection: string[];

      if (forceSingleSelection) {
        finalSelection = [path];
      } else if (!isTargetInSelection) {
        finalSelection = [path];
        if (activeView === 'library') {
          // In the grid a right-click selects the photograph like a click, so the
          // Navigator and the metadata tools show the one the menu acts on.
          setLibrary({ multiSelectedPaths: [path], libraryActivePath: path, selectionAnchorPath: path });
          props.handleImageSelect(path, false);
        } else {
          setLibrary({ multiSelectedPaths: [path] });
          if (!selectedImage) {
            setLibrary({ libraryActivePath: path });
          }
        }
      } else {
        finalSelection = multiSelectedPaths;
      }

      const commonTags = getCommonTags(finalSelection);

      const selectionCount = finalSelection.length;
      const isSingleSelection = selectionCount === 1;
      const isEditingThisImage = activeView === 'editor' && selectedImage?.path === path;
      const deleteLabel = t('contextMenus.thumbnail.deleteImage', { count: selectionCount });
      const exportLabel = t('contextMenus.thumbnail.exportImage', { count: selectionCount });

      const selectionHasVirtualCopies =
        isSingleSelection &&
        !finalSelection[0].includes('?vc=') &&
        imageList.some((image) => image.path.startsWith(`${finalSelection[0]}?vc=`));

      const hasAssociatedFiles = finalSelection.some((selectedPath) => {
        // A virtual copy is only its own edit: its original and the other copies
        // are not "associated files" to delete with it.
        if (selectedPath.includes('?vc=')) return false;
        const image = imageList.find((img) => img.path === selectedPath);
        if (image?.group_id != null) return true;

        const getBasePath = (p: string) => {
          const qMark = p.indexOf('?');
          const clean = qMark === -1 ? p : p.substring(0, qMark);
          const dot = clean.lastIndexOf('.');
          return dot === -1 ? clean : clean.substring(0, dot);
        };
        const basePath = getBasePath(selectedPath);

        return imageList.some((img) => img.path !== selectedPath && getBasePath(img.path) === basePath);
      });

      let deleteSubmenu;
      if (selectionHasVirtualCopies) {
        deleteSubmenu = [
          { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
          {
            label: t('contextMenus.thumbnail.confirmDeleteVc'),
            icon: Check,
            isDestructive: true,
            onClick: () => props.executeDelete(finalSelection, { includeAssociated: false }),
          },
        ];
      } else if (hasAssociatedFiles) {
        deleteSubmenu = [
          { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
          {
            label: t('contextMenus.thumbnail.deleteSelected'),
            icon: Check,
            isDestructive: true,
            onClick: () => props.executeDelete(finalSelection, { includeAssociated: false }),
          },
          {
            label: t('contextMenus.thumbnail.deleteAssociated'),
            icon: Check,
            isDestructive: true,
            onClick: () => props.executeDelete(finalSelection, { includeAssociated: true }),
          },
        ];
      } else {
        deleteSubmenu = [
          { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
          {
            label: t('contextMenus.thumbnail.confirmDelete'),
            icon: Check,
            isDestructive: true,
            onClick: () => props.executeDelete(finalSelection, { includeAssociated: false }),
          },
        ];
      }

      const pasteLabel = t('contextMenus.thumbnail.pasteAdjustments', { count: selectionCount });
      const resetLabel = t('contextMenus.thumbnail.resetAdjustments', { count: selectionCount });
      const copyLabel = t('contextMenus.thumbnail.copyImage', { count: selectionCount });
      const autoAdjustLabel = t('contextMenus.thumbnail.autoAdjust', { count: selectionCount });
      const renameLabel = t('contextMenus.thumbnail.renameImage', { count: selectionCount });
      const sessionSelects = sessionDirectory ? `${sessionDirectory}/Selects` : null;
      const cullLabel = t('contextMenus.thumbnail.cullImage', { count: selectionCount });
      const collageLabel = t('contextMenus.thumbnail.collage', { count: selectionCount });
      const stitchLabel = t('contextMenus.editor.stitchPanorama');
      const conversionLabel = t('contextMenus.thumbnail.convertNegative', { count: selectionCount });
      const mergeLabel = t('contextMenus.editor.mergeHdr');

      const handleCreateVirtualCopy = async (sourcePath: string) => {
        try {
          await invoke(Invokes.CreateVirtualCopy, {
            sourceVirtualPath: sourcePath,
            targetAlbumId: activeAlbumId || null,
          });

          if (activeAlbumId) {
            const sortedTree = await invoke<AlbumItem[]>(Invokes.GetAlbums);
            setLibrary({ albumTree: sortedTree });
          }
          await props.refreshImageList();
        } catch (err) {
          toast.error(t('contextMenus.toasts.failedCreateVirtualCopy', { err }));
        }
      };

      const handleApplyAutoAdjustmentsToSelection = () => {
        if (finalSelection.length === 0) return;
        if (selectedImage && !selectedImage.isReady && finalSelection.includes(selectedImage.path)) return;
        finalSelection.forEach((p) => globalImageCache.delete(p));

        invokeRecipeWrite(Invokes.ApplyAutoAdjustmentsToPaths, { paths: finalSelection })
          .then(
            () => [] as string[],
            (err) => {
              if (err instanceof ViewOnlyRefusal) return finalSelection;
              // The photos that were saved still update below.
              console.error('Failed to apply auto adjustments to paths:', err);
              const { message, failed } = saveFailure(err, finalSelection);
              toast.error(t('contextMenus.toasts.failedApplyAuto', { err: message }));
              return failed;
            },
          )
          .then(async (failed) => {
            const applied = finalSelection.filter((p) => !failed.includes(p));
            // The open photograph may have changed meanwhile: its editor
            // only takes the result while it is still the one open.
            if (selectedImage && applied.includes(selectedImage.path) && isCurrentEditorImage(selectedImage.path)) {
              const metadata = await invoke<ImageMetadata>(Invokes.LoadMetadata, { path: selectedImage.path });
              if (isCurrentEditorImage(selectedImage.path) && metadata.adjustments && !metadata.adjustments.is_null) {
                const normalized = normalizeLoadedAdjustments(metadata.adjustments, selectedImage.isRaw);
                setEditor({ adjustments: normalized });
                useEditorStore.getState().resetHistory(normalized);
              }
            }
            if (libraryActivePath && applied.includes(libraryActivePath)
              && useLibraryStore.getState().libraryActivePath === libraryActivePath) {
              const metadata = await invoke<ImageMetadata>(Invokes.LoadMetadata, { path: libraryActivePath });
              if (useLibraryStore.getState().libraryActivePath === libraryActivePath
                && metadata.adjustments && !metadata.adjustments.is_null) {
                const normalized = normalizeLoadedAdjustments(metadata.adjustments, isRawInLibrary(libraryActivePath));
                setLibrary({ libraryActiveAdjustments: normalized });
              }
            }
          })
          .catch((err) => {
            console.error('Failed to apply auto adjustments to paths:', err);
            toast.error(t('contextMenus.toasts.failedApplyAuto', { err }));
          });
      };

      const onExportClick = () => {
        setLibrary({ multiSelectedPaths: finalSelection });
        if (activeView === 'editor' && selectedImage && selectedImage.path !== path) {
          props.handleImageSelect(path);
        }
        setPanel(Panel.Export);
      };

      const handleRemoveFromAlbum = async () => {
        if (!activeAlbumId) return;
        if (sessionDirectory && activeAlbumId.startsWith(SESSION_ALBUM_PREFIX)) {
          try {
            await invoke(Invokes.SessionAlbumRemove, {
              directory: sessionDirectory,
              id: activeAlbumId.slice(SESSION_ALBUM_PREFIX.length),
              paths: finalSelection,
            });
            window.dispatchEvent(new CustomEvent(SESSION_ALBUMS_CHANGED));
          } catch (err) {
            toast.error(t('contextMenus.toasts.failedAddToAlbum', { err }));
          }
          return;
        }
        const newTree = JSON.parse(JSON.stringify(albumTree));

        const removeImages = (nodes: AlbumItem[]): boolean => {
          for (const n of nodes) {
            if (n.id === activeAlbumId && n.type === 'album') {
              (n as Album).images = (n as Album).images.filter((p) => !finalSelection.includes(p));
              return true;
            } else if (n.type === 'group') {
              if (removeImages(n.children)) return true;
            }
          }
          return false;
        };

        if (removeImages(newTree)) {
          try {
            await invoke(Invokes.SaveAlbums, { tree: newTree });
            const sortedTree = await invoke<AlbumItem[]>(Invokes.GetAlbums);
            setLibrary({ albumTree: sortedTree });

            const albumObj = sortedTree.reduce((acc: AlbumItem | null, cur: AlbumItem) => {
              const find = (n: AlbumItem): AlbumItem | null =>
                n.id === activeAlbumId
                  ? n
                  : n.type === 'group'
                    ? n.children.reduce((a: AlbumItem | null, c: AlbumItem) => a || find(c), null)
                    : null;
              return acc || find(cur);
            }, null) as Album;

            if (albumObj) {
              setLibrary({ imageList: imageList.filter((i) => albumObj.images.includes(i.path)) });
            }
          } catch (e) {
            toast.error(t('contextMenus.toasts.failedRemoveImages', { err: e }));
          }
        }
      };

      // Offline photographs: Locate photo / Locate folder first (Lightroom's
      // missing-photo "!" leads to Locate), then the usual entries.
      const offlineItems = offlineMenu(t, {
        directory: sessionDirectory,
        selection: imageList.filter((image) => finalSelection.includes(image.path)),
        onRelinked: (report) =>
          void (props.handleRelinked ? props.handleRelinked(report) : props.handleLibraryRefresh()),
        onShowMissing: props.handleShowMissing,
      });
      const options = [
        ...(offlineItems.length ? [...offlineItems, { type: OPTION_SEPARATOR }] : []),
        ...(!isEditingThisImage
          ? [
              {
                disabled: !isSingleSelection,
                icon: Edit,
                label: t('contextMenus.editor.editImage'),
                onClick: () => props.handleImageSelect(finalSelection[0]),
              },
              { icon: FileInput, label: exportLabel, onClick: onExportClick },
              { type: OPTION_SEPARATOR },
            ]
          : [{ icon: FileInput, label: exportLabel, onClick: onExportClick }, { type: OPTION_SEPARATOR }]),
        {
          disabled: !isSingleSelection,
          icon: Copy,
          label: t('contextMenus.editor.copyAdjustments'),
          onClick: () => handleCopyAdjustments(finalSelection[0]),
        },
        {
          disabled: copiedAdjustments === null,
          icon: ClipboardPaste,
          label: pasteLabel,
          onClick: () => handlePasteAdjustments(finalSelection),
        },
        presetMenu(usePresetStore.getState().presets, finalSelection),
        {
          label: t('contextMenus.editor.productivity'),
          icon: Gauge,
          submenu: [
            { label: autoAdjustLabel, icon: PencilSparkles, onClick: handleApplyAutoAdjustmentsToSelection },
            {
              label: t('contextMenus.thumbnail.autoLensCorrection', { count: selectionCount }),
              icon: Aperture,
              onClick: () => handleAutoLensCorrection(finalSelection),
            },
            {
              label: conversionLabel,
              icon: Film,
              disabled: selectionCount === 0,
              onClick: () => {
                setUI({ negativeModalState: { isOpen: true, targetPaths: finalSelection } });
              },
            },
            {
              icon: LayoutTemplate,
              label: collageLabel,
              onClick: () => {
                const imagesForCollage = imageList.filter((img) => finalSelection.includes(img.path));
                setUI({ collageModalState: { isOpen: true, sourceImages: imagesForCollage } });
              },
              disabled: selectionCount === 0 || selectionCount > 9,
            },
            {
              label: cullLabel,
              icon: Users,
              onClick: () =>
                setUI({
                  cullingModalState: {
                    isOpen: true,
                    progress: null,
                    suggestions: null,
                    error: null,
                    pathsToCull: finalSelection,
                  },
                }),
              disabled: selectionCount < 2,
            },
          ],
        },
        {
          label: t('contextMenus.merge.title'),
          icon: LayersArrowDown,
          submenu: [
            {
              disabled: selectionCount < 2 || selectionCount > 30,
              icon: SquaresUnite,
              label: stitchLabel,
              onClick: () => {
                setUI({ mergeModalState: { ...CLOSED_MERGE_MODAL, isOpen: true, kind: 'panorama', sourcePaths: finalSelection } });
              },
            },
            {
              disabled: selectionCount < 2 || selectionCount > 9,
              icon: Images,
              label: mergeLabel,
              onClick: () => {
                setUI({ mergeModalState: { ...CLOSED_MERGE_MODAL, isOpen: true, kind: 'hdr', sourcePaths: finalSelection } });
              },
            },
            {
              disabled: selectionCount < 4,
              icon: SquaresUnite,
              label: t('contextMenus.merge.hdrPanorama'),
              onClick: () => {
                setUI({ mergeModalState: { ...CLOSED_MERGE_MODAL, isOpen: true, kind: 'hdrPanorama', sourcePaths: finalSelection } });
              },
            },
            {
              disabled: selectionCount < 2,
              icon: Layers,
              label: t('contextMenus.merge.focusStack'),
              onClick: () => {
                setUI({ mergeModalState: { ...CLOSED_MERGE_MODAL, isOpen: true, kind: 'focusStack', sourcePaths: finalSelection } });
              },
            },
          ],
        },
        previewsMenu(t, finalSelection),
        { type: OPTION_SEPARATOR },
        {
          label: copyLabel,
          icon: Copy,
          onClick: () => {
            setProcess({ copiedFilePaths: finalSelection, isCopied: true });
          },
        },
        {
          icon: CopyPlus,
          label: t('contextMenus.thumbnail.duplicateImage'),
          submenu: [
            {
              label: t('contextMenus.thumbnail.physicalCopy'),
              icon: Copy,
              disabled: !isSingleSelection,
              onClick: async () => {
                try {
                  await invoke(Invokes.DuplicateFile, {
                    path: finalSelection[0],
                    targetAlbumId: activeAlbumId || null,
                  });
                  if (activeAlbumId) {
                    const sortedTree = await invoke<AlbumItem[]>(Invokes.GetAlbums);
                    setLibrary({ albumTree: sortedTree });
                  }
                  await props.refreshImageList();
                } catch (err) {
                  console.error('Failed to duplicate file:', err);
                  toast.error(t('contextMenus.toasts.failedDuplicate', { err }));
                }
              },
            },
            {
              label: t('contextMenus.thumbnail.virtualCopy'),
              icon: CopyPlus,
              disabled: !isSingleSelection,
              onClick: () => handleCreateVirtualCopy(finalSelection[0]),
            },
          ],
        },
        { icon: FileEdit, label: renameLabel, onClick: () => props.handleRenameFiles(finalSelection) },
        ...(sessionSelects && props.handleMoveToFolder
          ? [
              {
                icon: FolderInput,
                label: t('settings.keybinds.actions.move_to_selects'),
                disabled: finalSelection.every((p) => p.split('?vc=')[0].startsWith(`${sessionSelects}/`)),
                onClick: () => props.handleMoveToFolder?.(finalSelection, sessionSelects),
              },
            ]
          : []),
        { type: OPTION_SEPARATOR },
        {
          icon: Star,
          label: t('contextMenus.editor.rating'),
          submenu: [0, 1, 2, 3, 4, 5].map((rating: number) => ({
            label:
              rating === 0
                ? t('contextMenus.editor.noRating')
                : t('contextMenus.editor.ratingLabel', { count: rating }),
            onClick: () => handleRate(rating, finalSelection),
          })),
        },
        {
          label: t('contextMenus.editor.colorLabel'),
          icon: Palette,
          submenu: [
            { label: t('contextMenus.editor.noLabel'), onClick: () => handleSetColorLabel(null, finalSelection) },
            ...COLOR_LABELS.map((label: Color) => ({
              label: t(`contextMenus.colors.${label.name}`, { defaultValue: `contextMenus.colors.${label.name}` }),
              color: label.color,
              onClick: () => handleSetColorLabel(label.name, finalSelection),
            })),
          ],
        },
        {
          label: t('contextMenus.editor.tagging'),
          icon: Tag,
          submenu: [
            {
              customComponent: TaggingSubMenu,
              customProps: {
                paths: finalSelection,
                initialTags: commonTags,
                onTagsChanged: handleTagsChanged,
                appSettings,
              } satisfies TaggingSubMenuProps,
            },
          ],
        },
        { type: OPTION_SEPARATOR },
        {
          label: t('contextMenus.thumbnail.addToAlbum'),
          icon: FolderPlus,
          // A session keeps its own albums (SQLite, Library › Albums).
          submenu: sessionDirectory
            ? [
                {
                  customComponent: SessionAlbumSubMenu,
                  customProps: { directory: sessionDirectory, paths: finalSelection },
                },
              ]
            : albumTree.length > 0
              ? buildAddToAlbumMenu(albumTree, finalSelection)
              : [{ label: t('contextMenus.thumbnail.noAlbums'), disabled: true }],
        },
        ...(activeAlbumId && activeAlbumId !== MISSING_VIEW_ID && activeAlbumId !== PREVIOUS_IMPORT_VIEW_ID
          ? [
              {
                label: t('contextMenus.thumbnail.removeFromAlbum', { count: selectionCount }),
                icon: Trash2,
                isDestructive: true,
                onClick: handleRemoveFromAlbum,
              },
            ]
          : []),
        { type: OPTION_SEPARATOR },
        {
          disabled: !isSingleSelection,
          icon: Folder,
          label: useSettingsStore.getState().osPlatform === 'macos' ? t('contextMenus.thumbnail.showFinder') : t('contextMenus.thumbnail.showExplorer'),
          onClick: () => {
            invoke(Invokes.ShowInFinder, { path: finalSelection[0] }).catch((err) =>
              toast.error(t('contextMenus.toasts.couldNotShowExplorer', { err })),
            );
          },
        },
        {
          label: resetLabel,
          icon: RotateCcw,
          submenu: [
            { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
            {
              label: t('contextMenus.editor.confirmReset'),
              icon: Check,
              isDestructive: true,
              onClick: () => handleResetAdjustments(finalSelection),
            },
          ],
        },
        {
          label: deleteLabel,
          icon: Trash2,
          isDestructive: true,
          submenu: deleteSubmenu,
        },
      ];
      showContextMenu(event.clientX, event.clientY, options);
    },
    [
      getCommonTags,
      buildAddToAlbumMenu,
      handleCopyAdjustments,
      handlePasteAdjustments,
      handleAutoLensCorrection,
      handleRate,
      handleSetColorLabel,
      handleTagsChanged,
      handleResetAdjustments,
      showContextMenu,
      props,
      t,
    ],
  );

  return {
    handleEditorContextMenu,
    handleThumbnailContextMenu,
  };
}
