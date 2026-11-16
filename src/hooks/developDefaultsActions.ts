import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import i18next from 'i18next';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes, type Preset } from '../components/ui/AppProperties';
import { notify } from '../activity/notify';
import type { Adjustments } from '../utils/adjustments';
import {
  cameraName,
  NO_DEVELOP_DEFAULTS,
  presetDefaultSettings,
  toolDefaultSettings,
  withCameraDefault,
  withCameraToolDefault,
  withDevelopDefaults,
  withGlobalDefault,
  type CameraIdentity,
  type DevelopDefaults,
  type PhotoDefaults,
  type ResolvedDefaults
} from '../utils/developDefaults';
import { initialAdjustmentsFor } from '../utils/adjustments';
import { effectiveVersions, expressPastedSettings } from '../utils/pastedVersions';
import { collectPresetAdjustments } from './usePresets';
import { showNewDevelopDefaults, updateAdjustments } from './useEditorActions';
import { COPIED_ALONG, pick } from '../components/studio/StudioToolCopy';

/** A photo's settings as a default: as a style saved from it would carry
 * them (the versions its values render with stated, also those an older
 * edit leaves implicit), applied to a new edit. */
export function photoDefaultSettings(current: Adjustments, isRaw: boolean): Record<string, unknown> {
  const versions = effectiveVersions(current, isRaw);
  const stated = { ...current, ...versions } as Adjustments;
  const collected = {
    ...collectPresetAdjustments(stated, initialAdjustmentsFor(isRaw), {
      includeMasks: false,
      includeCropTransform: false,
      presetType: 'style',
      isRaw
    }),
    ...versions
  };
  const asStyle = {
    id: 'photo',
    name: 'photo',
    presetType: 'style',
    adjustments: collected
  } as Preset;
  return presetDefaultSettings(asStyle, isRaw);
}

/** `prev` with a tool's default settings `picked` as a paste of them: a
 * response key stays the photo's while its layers (or other sliders) use
 * it, the values re-expressed in it. */
export const withToolDefaults = (prev: Adjustments, picked: Partial<Adjustments>, isRaw: boolean): Adjustments => ({
  ...prev,
  ...expressPastedSettings(picked, { recipe: prev, isRaw, replaceLayers: false, isNew: false })
});

/** The develop defaults as saved now. */
export const currentDevelopDefaults = (): DevelopDefaults =>
  useSettingsStore.getState().appSettings?.developDefaults ?? NO_DEVELOP_DEFAULTS;

/**
 * Saves new develop defaults: the settings first (the backend applies them
 * from then on), then the open photograph's defaults are asked again
 * (`showNewDevelopDefaults`) and the Library's thumbnails are asked for,
 * so never-edited photographs show the new defaults (their cached
 * thumbnails are keyed by them). Edited photographs never change.
 */
export async function saveDevelopDefaults(next: DevelopDefaults): Promise<void> {
  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
  if (!appSettings) return;
  await handleSettingsChange({ ...appSettings, developDefaults: next });
  const open = useEditorStore.getState().selectedImage?.path;
  if (open) {
    const photoDefaults = await invoke<PhotoDefaults>(Invokes.DevelopDefaultsFor, { path: open }).catch(() => null);
    showNewDevelopDefaults(open, photoDefaults);
  }
  const paths = useLibraryStore.getState().imageList.map((image) => image.path);
  if (paths.length) void invoke(Invokes.UpdateThumbnailQueue, { paths }).catch(() => undefined);
}

const saved = (next: DevelopDefaults, message: string) =>
  saveDevelopDefaults(next).then(
    () => notify.success(message),
    (error) => notify.error(i18next.t('developDefaults.toasts.saveFailed', { error: String(error) }))
  );

/** The target of a camera menu entry: its model, or its body (serial). */
export type CameraLevel = 'camera' | 'body';

const targetName = (camera: CameraIdentity, level: CameraLevel) =>
  level === 'body'
    ? i18next.t('developDefaults.bodyName', { camera: cameraName(camera), serial: camera.serial })
    : cameraName(camera);

/** A preset or style as the camera's (body's) default (replaces the more general defaults). */
export function setPresetAsCameraDefault(preset: Preset, name: string, camera: CameraIdentity, level: CameraLevel) {
  const next = withCameraDefault(currentDevelopDefaults(), camera, level, {
    name,
    settings: presetDefaultSettings(preset)
  });
  return saved(next, i18next.t('developDefaults.toasts.set', { name, camera: targetName(camera, level) }));
}

/** A preset or style as the global default. */
export function setPresetAsGlobalDefault(preset: Preset, name: string) {
  const next = withGlobalDefault(currentDevelopDefaults(), { name, settings: presetDefaultSettings(preset) });
  return saved(next, i18next.t('developDefaults.toasts.setGlobal', { name }));
}

/** The open photograph's settings (without its masks, crop, transform and
 * patches) as its camera's (body's) default. */
export function setPhotoAsCameraDefault(
  current: Adjustments,
  isRaw: boolean,
  photoName: string,
  camera: CameraIdentity,
  level: CameraLevel
) {
  const next = withCameraDefault(currentDevelopDefaults(), camera, level, {
    name: photoName,
    settings: photoDefaultSettings(current, isRaw)
  });
  return saved(next, i18next.t('developDefaults.toasts.set', { name: photoName, camera: targetName(camera, level) }));
}

/** One tool's settings of the open photograph saved into its camera's
 * (body's) default (Capture One's per-tool "Save as Defaults"). */
export function saveToolAsCameraDefault(
  current: Adjustments,
  keys: string[],
  isRaw: boolean,
  tool: string,
  camera: CameraIdentity,
  level: CameraLevel
) {
  const next = withCameraToolDefault(
    currentDevelopDefaults(),
    camera,
    level,
    toolDefaultSettings(current, keys, isRaw)
  );
  return saved(next, i18next.t('developDefaults.toasts.toolSaved', { tool, camera: targetName(camera, level) }));
}

/** A tool's reset on the open photograph: its keys (`keys`, with those
 * that go along with them) back to the user's develop defaults for the
 * photo, after the tool's own reset set them neutral. Nothing without a
 * default for them. */
export function resetToolToUserDefaults(keys: string[]) {
  const image = useEditorStore.getState().selectedImage;
  const resolved = image?.photoDefaults?.resolved;
  if (!resolved) return;
  const isRaw = !!image?.isRaw;
  updateAdjustments((prev) => withToolResetToDefaults(prev, resolved, keys, isRaw));
}

/** `prev` with the tool's `keys` that the defaults `resolved` set back to
 * them: copied from a new edit with those defaults as a tool copy does
 * (Blacks as amounts, their versions along), then pasted (`withToolDefaults`). */
export function withToolResetToDefaults(
  prev: Adjustments,
  resolved: ResolvedDefaults,
  keys: string[],
  isRaw: boolean
): Adjustments {
  const set = keys.filter((key) =>
    [key, ...(COPIED_ALONG[key] ?? [])].some((k) => Object.prototype.hasOwnProperty.call(resolved.settings, k))
  );
  if (!set.length) return prev;
  const picked = pick(withDevelopDefaults(initialAdjustmentsFor(isRaw), resolved), set, isRaw);
  return withToolDefaults(prev, picked, isRaw);
}

/** Asks the open photograph's camera (and defaults) once it is ready, when
 * loading did not (no default set then): the "Set as default for <camera>"
 * menus name it. */
export function useOpenPhotoCamera() {
  const path = useEditorStore((s) => (s.selectedImage?.isReady ? s.selectedImage.path : null));
  const known = useEditorStore((s) => s.selectedImage?.photoDefaults !== undefined);
  useEffect(() => {
    if (!path || known) return;
    let current = true;
    void invoke<PhotoDefaults>(Invokes.DevelopDefaultsFor, { path })
      .catch(() => null)
      .then((photoDefaults) => {
        const { selectedImage, setEditor } = useEditorStore.getState();
        if (!current || selectedImage?.path !== path || selectedImage.photoDefaults !== undefined) return;
        // Only the camera: the recipe shown was made without defaults.
        setEditor({
          selectedImage: { ...selectedImage, photoDefaults: { camera: photoDefaults?.camera ?? null, resolved: null } }
        });
      });
    return () => {
      current = false;
    };
  }, [path, known]);
}
