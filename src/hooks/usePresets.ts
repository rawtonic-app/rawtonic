import { useEffect, useCallback, useMemo } from 'react';
import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import debounce from 'lodash.debounce';
import {
  Adjustments,
  COPYABLE_ADJUSTMENT_KEYS,
  ADJUSTMENT_GROUPS,
  initialAdjustmentsFor,
  sameAdjustmentValue,
  withAiDenoiseVersion,
  withLumaNoiseVersion,
  withSharpenVersion,
} from '../utils/adjustments';
import { Folder, Invokes, Preset } from '../components/ui/AppProperties';
import { frameDrawnMasks } from '../utils/maskGeometry';
import { registerQuitFlush } from '../utils/quit';

/** The settings a preset takes from `current`: the copyable keys (masks and
 * geometry only when asked), for a tool preset only those that differ from
 * `defaults`. The sharpening response is left out here: it goes along only
 * with sharpening values (`withSharpenVersion`), never on its own, so an
 * exposure preset never switches a photo's sharpening. */
export function collectPresetAdjustments(
  current: Partial<Adjustments>,
  defaults: Partial<Adjustments>,
  {
    includeMasks,
    includeCropTransform,
    presetType,
  }: { includeMasks: boolean; includeCropTransform: boolean; presetType: 'tool' | 'style' },
): Partial<Adjustments> {
  const GEOMETRY_KEYS = ADJUSTMENT_GROUPS.geometry.flatMap((group) => group.keys);
  const MASK_KEYS = ADJUSTMENT_GROUPS.masks.flatMap((group) => group.keys);
  const presetAdjustments: Partial<Adjustments> = {};
  for (const key of COPYABLE_ADJUSTMENT_KEYS) {
    if (!includeMasks && MASK_KEYS.includes(key)) continue;
    if (!includeCropTransform && GEOMETRY_KEYS.includes(key)) continue;
    if (key === 'sharpenVersion' || key === 'lumaNoiseVersion') continue;
    if (!Object.prototype.hasOwnProperty.call(current, key)) continue;
    const currentValue = current[key as keyof Adjustments];
    if (presetType === 'tool' && sameAdjustmentValue(currentValue, defaults[key as keyof Adjustments])) continue;
    presetAdjustments[key] = currentValue;
  }
  return withLumaNoiseVersion(withSharpenVersion(presetAdjustments, current), current);
}

export enum PresetListType {
  Folder = 'folder',
  Preset = 'preset',
}

export interface UserPreset {
  folder?: Folder;
  id?: string | undefined;
  name?: string | undefined;
  preset?: Preset;
}

export interface PresetImportFailure {
  fileName: string;
  error: string;
}

// Settings a Lightroom preset or Capture One style set that the import did
// not translate (codes from the converters, see `PresetImportUntranslated`).
export interface PresetImportUntranslated {
  fileName: string;
  settings: Array<string>;
  profile?: string;
}

export interface PresetImportResult {
  presets: Array<UserPreset>;
  failures: Array<PresetImportFailure>;
  untranslated?: Array<PresetImportUntranslated>;
}

function arrayMove<T>(array: T[], from: number, to: number): T[] {
  const newArray = array.slice();
  const [item] = newArray.splice(from, 1);
  newArray.splice(to, 0, item);
  return newArray;
}

/** `isRaw`: the open photo's defaults decide what "changed" means for presets
 * that keep only changed settings (RAWs have their own detail defaults). */
type PresetsUpdate = Array<UserPreset> | ((current: Array<UserPreset>) => Array<UserPreset>);

/** One preset list for every user of `usePresets` (the Develop tool, the
 * Library's context menu, the inherited panel), so a change shows everywhere. */
export const usePresetStore = create<{
  presets: Array<UserPreset>;
  isLoading: boolean;
  setPresets(update: PresetsUpdate): void;
  setIsLoading(loading: boolean): void;
}>((set) => ({
  presets: [],
  isLoading: true,
  setPresets: (update) =>
    set((state) => ({ presets: typeof update === 'function' ? update(state.presets) : update })),
  setIsLoading: (isLoading) => set({ isLoading }),
}));

/** Loads the presets into the shared list (for menus outside the Develop tool). */
export async function loadPresetLibrary() {
  try {
    const loaded: Array<UserPreset> | null = await invoke(Invokes.LoadPresets);
    usePresetStore.setState({ presets: Array.isArray(loaded) ? loaded : [], isLoading: false });
  } catch (error) {
    console.error('Failed to load presets:', error);
  }
}

/** `photoWidth` × `photoHeight`: the open photo's own size, recorded with
 * the drawn masks a preset takes along (`frameDrawnMasks`).
 * `currentAdjustments` is read when a preset is saved: a subscription
 * re-rendered the panel on every edit commit, ~10 per slider drag. */
export function usePresets(
  currentAdjustments: () => Adjustments,
  isRaw = false,
  photoWidth?: number,
  photoHeight?: number,
) {
  // Drawn masks a preset takes along keep the frame of the photo they were
  // drawn on, so other photos place them on the same part of themselves.
  const framed = (adjustments: Partial<Adjustments>, current: Adjustments): Partial<Adjustments> =>
    adjustments.masks
      ? {
          ...adjustments,
          masks: frameDrawnMasks(adjustments.masks, current, photoWidth, photoHeight),
        }
      : adjustments;
  const defaults = initialAdjustmentsFor(isRaw);
  const presets = usePresetStore((s) => s.presets);
  const isLoading = usePresetStore((s) => s.isLoading);
  const setPresets = usePresetStore((s) => s.setPresets);
  const setIsLoading = usePresetStore((s) => s.setIsLoading);

  const loadPresets = useCallback(async () => {
    setIsLoading(true);
    try {
      const loadedPresets: Array<UserPreset> | null = await invoke(Invokes.LoadPresets);
      setPresets(Array.isArray(loadedPresets) ? loadedPresets : []);
    } catch (error) {
      console.error('Failed to load presets:', error);
      setPresets([]);
    } finally {
      setIsLoading(false);
    }
  }, []);

  const savePresetsToBackend = useMemo(
    () =>
      debounce(
        (presetsToSave: Array<UserPreset>): Promise<string[]> =>
          invoke(Invokes.SavePresets, { presets: presetsToSave }).then(
            () => [],
            (err) => {
              console.error('Failed to save presets:', err);
              return ['Presets'];
            },
          ),
        500,
      ),
    [],
  );

  // A pending save is written when the tool closes and before quitting.
  useEffect(() => {
    const unregister = registerQuitFlush(async () => (await savePresetsToBackend.flush()) ?? []);
    return () => {
      unregister();
      savePresetsToBackend.flush();
    };
  }, [savePresetsToBackend]);

  useEffect(() => {
    loadPresets();
  }, [loadPresets]);

  const addPreset = (
    name: string,
    folderId: string | null = null,
    includeMasks: boolean = false,
    includeCropTransform: boolean = false,
    presetType: 'tool' | 'style' = 'style',
  ) => {
    const current = currentAdjustments();

    const presetAdjustments = collectPresetAdjustments(current, defaults, {
      includeMasks,
      includeCropTransform,
      presetType,
    });

    const newPresetData: Preset = {
      adjustments: framed(withLumaNoiseVersion(withSharpenVersion(withAiDenoiseVersion(presetAdjustments, current), current), current), current),
      id: crypto.randomUUID(),
      name,
      includeMasks,
      includeCropTransform,
      presetType,
    };

    let updatedPresets: Array<UserPreset>;
    if (folderId) {
      updatedPresets = presets.map((item: UserPreset) => {
        if (item.folder && item.folder.id === folderId) {
          return {
            folder: {
              ...item.folder,
              children: [...item.folder.children, newPresetData],
            },
          };
        }
        return item;
      });
    } else {
      updatedPresets = [...presets, { preset: newPresetData }];
    }

    setPresets(updatedPresets);
    savePresetsToBackend(updatedPresets);
    return newPresetData;
  };

  const addFolder = (name: string) => {
    const newFolder = {
      folder: {
        id: crypto.randomUUID(),
        name,
        children: [],
      },
    };

    setPresets((currentPresets: Array<UserPreset>) => {
      const updatedPresets = [...currentPresets];
      const firstPresetIndex = updatedPresets.findIndex((p: UserPreset) => p.preset);

      if (firstPresetIndex === -1) {
        updatedPresets.push(newFolder);
      } else {
        updatedPresets.splice(firstPresetIndex, 0, newFolder);
      }

      savePresetsToBackend(updatedPresets);
      return updatedPresets;
    });
  };

  const deleteItem = (id: string) => {
    let updatedPresets = presets.filter((item: UserPreset) => item.preset?.id !== id && item.folder?.id !== id);
    updatedPresets = updatedPresets.map((item: UserPreset) => {
      if (item.folder) {
        return {
          folder: {
            ...item.folder,
            children: item.folder.children.filter((child: Preset) => child.id !== id),
          },
        };
      }
      return item;
    });
    setPresets(updatedPresets);
    savePresetsToBackend(updatedPresets);
  };

  const renameItem = (id: string | null, newName: string) => {
    const updatedPresets = presets.map((item: UserPreset) => {
      if (item.preset?.id === id) {
        return { preset: { ...item.preset, name: newName } };
      }
      if (item.folder?.id === id) {
        return { folder: { ...item.folder, name: newName } };
      }
      if (item.folder) {
        return {
          folder: {
            ...item.folder,
            children: item.folder.children.map((child: Preset) => (child.id === id ? { ...child, name: newName } : child)),
          },
        };
      }
      return item;
    });
    setPresets(updatedPresets);
    savePresetsToBackend(updatedPresets);
  };

  const overwritePreset = (id: string | null) => {
    let existingPreset: Preset | null = null;

    for (const item of presets) {
      if (item.preset?.id === id) {
        existingPreset = item.preset;
        break;
      }
      if (item.folder) {
        const found = item.folder.children.find((p: Preset) => p.id === id);
        if (found) {
          existingPreset = found;
          break;
        }
      }
    }

    if (!existingPreset) return null;

    const GEOMETRY_KEYS = ADJUSTMENT_GROUPS.geometry.flatMap((group) => group.keys);

    const includeMasks =
      existingPreset.includeMasks ??
      (existingPreset.adjustments?.masks && existingPreset.adjustments.masks.length > 0) ??
      false;
    const includeCropTransform =
      existingPreset.includeCropTransform ??
      GEOMETRY_KEYS.some((key) => existingPreset.adjustments?.[key] !== undefined) ??
      false;
    const presetType = existingPreset.presetType || 'style';
    const current = currentAdjustments();

    const presetAdjustments = collectPresetAdjustments(current, defaults, {
      includeMasks,
      includeCropTransform,
      presetType,
    });

    let updatedPreset: Preset | null = null;
    const updatedPresets = presets.map((item: UserPreset) => {
      if (item.preset?.id === id) {
        updatedPreset = {
          ...item.preset,
          adjustments: framed(withLumaNoiseVersion(withSharpenVersion(withAiDenoiseVersion(presetAdjustments, current), current), current), current),
          includeMasks,
          includeCropTransform,
          presetType,
        };
        return { preset: updatedPreset };
      }
      if (item.folder) {
        let found = false;
        const newChildren = item.folder.children.map((child: Preset) => {
          if (child.id === id) {
            found = true;
            updatedPreset = {
              ...child,
              adjustments: framed(withLumaNoiseVersion(withSharpenVersion(withAiDenoiseVersion(presetAdjustments, current), current), current), current),
              includeMasks,
              includeCropTransform,
              presetType,
            };
            return updatedPreset;
          }
          return child;
        });
        if (found) {
          return { folder: { ...item.folder, children: newChildren } };
        }
      }
      return item;
    });

    setPresets(updatedPresets);
    savePresetsToBackend(updatedPresets);
    return updatedPreset;
  };

  const duplicatePreset = useCallback(
    (presetId: string | null) => {
      let presetToDuplicate: Preset | null = null;
      let sourceFolderId = null;

      for (const item of presets) {
        if (item.preset?.id === presetId) {
          presetToDuplicate = item.preset;
          break;
        }
        if (item.folder) {
          const found = item.folder.children.find((p: Preset) => p.id === presetId);
          if (found) {
            presetToDuplicate = found;
            sourceFolderId = item.folder.id;
            break;
          }
        }
      }

      if (!presetToDuplicate) {
        return null;
      }

      const newPreset: Preset = {
        adjustments: JSON.parse(JSON.stringify(presetToDuplicate.adjustments)),
        id: crypto.randomUUID(),
        name: `${presetToDuplicate.name} Copy`,
        includeMasks: presetToDuplicate.includeMasks,
        includeCropTransform: presetToDuplicate.includeCropTransform,
        presetType: presetToDuplicate.presetType || 'style',
      };

      let updatedPresets;
      if (sourceFolderId) {
        updatedPresets = presets.map((item: UserPreset) => {
          if (item.folder?.id === sourceFolderId) {
            const originalIndex = item.folder.children.findIndex((p: Preset) => p.id === presetId);
            const newChildren = [...item.folder.children];
            newChildren.splice(originalIndex + 1, 0, newPreset);
            return { folder: { ...item.folder, children: newChildren } };
          }
          return item;
        });
      } else {
        const originalIndex = presets.findIndex((item: UserPreset) => item.preset?.id === presetId);
        updatedPresets = [...presets];
        updatedPresets.splice(originalIndex + 1, 0, { preset: newPreset });
      }

      setPresets(updatedPresets);
      savePresetsToBackend(updatedPresets);
      return newPreset;
    },
    [presets, savePresetsToBackend],
  );

  const movePreset = useCallback(
    (presetId: string, targetFolderId: string | null, overId: string | null = null) => {
      let presetToMove: Preset | null = null;
      let sourceFolderId = null;

      for (const item of presets) {
        if (item.preset?.id === presetId) {
          presetToMove = item.preset;
          break;
        }
        if (item.folder) {
          const found = item.folder.children.find((p: Preset) => p.id === presetId);
          if (found) {
            presetToMove = found;
            sourceFolderId = item.folder.id;
            break;
          }
        }
      }

      if (!presetToMove) {
        return;
      }

      let updatedPresets = [...presets];

      if (sourceFolderId) {
        updatedPresets = updatedPresets.map((item: UserPreset) =>
          item.folder?.id === sourceFolderId
            ? { folder: { ...item.folder, children: item.folder.children.filter((p: Preset) => p.id !== presetId) } }
            : item,
        );
      } else {
        updatedPresets = updatedPresets.filter((item: UserPreset) => item.preset?.id !== presetId);
      }

      if (targetFolderId) {
        updatedPresets = updatedPresets.map((item: UserPreset) => {
          if (item.folder?.id === targetFolderId) {
            const newChildren = [...item.folder.children];
            if (overId) {
              const overIndex = newChildren.findIndex((p) => p.id === overId);
              if (overIndex !== -1) {
                newChildren.splice(overIndex, 0, presetToMove!);
              } else {
                newChildren.push(presetToMove!);
              }
            } else {
              newChildren.push(presetToMove!);
            }
            return { folder: { ...item.folder, children: newChildren } };
          }
          return item;
        });
      } else {
        if (overId) {
          const overIndex = updatedPresets.findIndex(
            (item) => item.preset?.id === overId || item.folder?.id === overId,
          );
          if (overIndex !== -1) {
            updatedPresets.splice(overIndex, 0, { preset: presetToMove });
          } else {
            updatedPresets.push({ preset: presetToMove });
          }
        } else {
          updatedPresets.push({ preset: presetToMove });
        }
      }

      setPresets(updatedPresets);
      savePresetsToBackend(updatedPresets);
    },
    [presets, savePresetsToBackend],
  );

  const reorderItems = useCallback(
    (activeId: string, overId: string) => {
      setPresets((currentPresets: Array<UserPreset>) => {
        const getIndex = (arr: Array<UserPreset>, id: string) =>
          arr.findIndex((item: UserPreset) => item.preset?.id === id || item.folder?.id === id || item?.id === id);

        const activeRootIndex = getIndex(currentPresets, activeId);
        const overRootIndex = getIndex(currentPresets, overId);

        if (activeRootIndex !== -1 && overRootIndex !== -1) {
          const newPresets: Array<UserPreset> = arrayMove(currentPresets, activeRootIndex, overRootIndex);
          savePresetsToBackend(newPresets);
          return newPresets;
        }

        for (const item of currentPresets) {
          if (item.folder) {
            const activeChildIndex = getIndex(item.folder.children, activeId);
            const overChildIndex = getIndex(item.folder.children, overId);

            if (activeChildIndex !== -1 && overChildIndex !== -1) {
              const newPresets = currentPresets.map((p: UserPreset) => {
                if (p.folder?.id === item.folder?.id) {
                  return {
                    folder: {
                      ...p?.folder,
                      // p.folder is set: its id matched the folder that holds both children.
                      children: arrayMove(p.folder?.children as Preset[], activeChildIndex, overChildIndex),
                    },
                  };
                }
                return p;
              });
              savePresetsToBackend(newPresets);
              return newPresets;
            }
          }
        }

        return currentPresets;
      });
    },
    [savePresetsToBackend],
  );

  const sortAllPresetsAlphabetically = useCallback(() => {
    setPresets((currentPresets) => {
      const newPresets: Array<UserPreset> = JSON.parse(JSON.stringify(currentPresets));
      const sortOptions: Intl.CollatorOptions = { numeric: true, sensitivity: 'base' };

      newPresets.forEach((item: UserPreset) => {
        if (item.folder && item.folder.children) {
          item.folder.children.sort((a: Preset, b: Preset) => a.name.localeCompare(b.name, undefined, sortOptions));
        }
      });

      // The filters keep only folder or preset items; backend folders always have a name.
      const folders = newPresets.filter((item: UserPreset) => item.folder) as Array<{
        folder: Folder & { name: string };
      }>;
      const rootPresets = newPresets.filter((item: UserPreset) => item.preset) as Array<{ preset: Preset }>;

      folders.sort((a, b) => a.folder.name.localeCompare(b.folder.name, undefined, sortOptions));
      rootPresets.sort((a, b) => a.preset.name.localeCompare(b.preset.name, undefined, sortOptions));

      const sortedPresets = [...folders, ...rootPresets];

      savePresetsToBackend(sortedPresets);
      return sortedPresets;
    });
  }, [savePresetsToBackend]);

  const importPresetsFromFile = useCallback(
    async (filePath: string) => {
      setIsLoading(true);
      try {
        const updatedPresetList: Array<UserPreset> = await invoke(Invokes.HandleImportPresetsFromFile, { filePath });
        setPresets(updatedPresetList);
      } catch (error) {
        console.error('Failed to import presets from file:', error);
        throw error;
      } finally {
        setIsLoading(false);
      }
    },
    [setPresets],
  );

  const importLegacyPresetsFromFile = useCallback(
    async (filePath: string) => {
      setIsLoading(true);
      try {
        const updatedPresetList: Array<UserPreset> = await invoke(Invokes.HandleImportLegacyPresetsFromFile, {
          filePath,
        });
        setPresets(updatedPresetList);
      } catch (error) {
        console.error('Failed to import legacy presets from file:', error);
        throw error;
      } finally {
        setIsLoading(false);
      }
    },
    [setPresets],
  );

  const importPresetsFromFiles = useCallback(
    async (filePaths: Array<string>): Promise<PresetImportResult> => {
      setIsLoading(true);
      try {
        const result: PresetImportResult = await invoke(Invokes.HandleImportPresetsFromFiles, { filePaths });
        setPresets(result.presets);
        return result;
      } catch (error) {
        console.error('Failed to import presets from files:', error);
        throw error;
      } finally {
        setIsLoading(false);
      }
    },
    [setPresets],
  );

  const exportPresetsToFile = useCallback(async (presetsToExport: Array<UserPreset>, filePath: string) => {
    try {
      await invoke(Invokes.HandleExportPresetsToFile, { presetsToExport, filePath });
    } catch (error) {
      console.error('Failed to export presets to file:', error);
      throw error;
    }
  }, []);

  return {
    addFolder,
    addPreset,
    deleteItem,
    duplicatePreset,
    exportPresetsToFile,
    importPresetsFromFile,
    importPresetsFromFiles,
    importLegacyPresetsFromFile,
    isLoading,
    movePreset,
    overwritePreset,
    presets,
    refreshPresets: loadPresets,
    renameItem,
    reorderItems,
    sortAllPresetsAlphabetically,
  };
}
