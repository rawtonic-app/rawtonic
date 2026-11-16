import i18next from 'i18next';
import { Camera } from 'lucide-react';
import { useEditorStore } from '../../store/useEditorStore';
import { OPTION_SEPARATOR, type Option, type Preset } from '../ui/AppProperties';
import { cameraName, type CameraIdentity } from '../../utils/developDefaults';
import {
  saveToolAsCameraDefault,
  setPhotoAsCameraDefault,
  setPresetAsCameraDefault,
  setPresetAsGlobalDefault,
  type CameraLevel
} from '../../hooks/developDefaultsActions';
import type { ToolParams } from './StudioToolCopy';

/** The open photograph's camera and whether it is a RAW, once its develop
 * defaults are known (`develop_defaults_for`). */
function openCamera(): { camera: CameraIdentity; isRaw: boolean } | null {
  const image = useEditorStore.getState().selectedImage;
  const camera = image?.photoDefaults?.camera;
  return image?.isReady && camera ? { camera, isRaw: !!image.isRaw } : null;
}

/** The camera's model and, with a serial number, its body. */
const levelsOf = (camera: CameraIdentity): CameraLevel[] => (camera.serial ? ['camera', 'body'] : ['camera']);

/** "… for <model>" / "… for this camera body (<serial>)". */
function labelFor(key: 'setForCamera' | 'saveToolForCamera', camera: CameraIdentity, level: CameraLevel) {
  const values = { camera: cameraName(camera), serial: camera.serial };
  if (key === 'setForCamera')
    return level === 'body'
      ? i18next.t('developDefaults.menu.setForCameraBody', values)
      : i18next.t('developDefaults.menu.setForCamera', values);
  return level === 'body'
    ? i18next.t('developDefaults.menu.saveToolForCameraBody', values)
    : i18next.t('developDefaults.menu.saveToolForCamera', values);
}

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** A tool header's menu entries: its settings on the open photo saved as
 * the default of the photo's camera (and body) — Capture One's per-tool
 * "Save as Defaults". */
export function toolDefaultItems(tool: string, params: ToolParams): Option[] {
  const open = openCamera();
  if (!open) return [];
  const keys = params.map(([key]) => key as string);
  return [
    ...levelsOf(open.camera).map((level) => ({
      label: labelFor('saveToolForCamera', open.camera, level),
      icon: Camera,
      onClick: () =>
        void saveToolAsCameraDefault(useEditorStore.getState().adjustments, keys, open.isRaw, tool, open.camera, level)
    })),
    { type: OPTION_SEPARATOR }
  ];
}

/** A preset's or style's menu entries: as the default of the open photo's
 * camera (and body) or as the global default. */
export function presetDefaultItems(preset: Preset, name: string): Option[] {
  const open = openCamera();
  return [
    { type: OPTION_SEPARATOR },
    ...(open
      ? levelsOf(open.camera).map((level) => ({
          label: labelFor('setForCamera', open.camera, level),
          icon: Camera,
          onClick: () => void setPresetAsCameraDefault(preset, name, open.camera, level)
        }))
      : []),
    {
      label: i18next.t('developDefaults.menu.setGlobal'),
      onClick: () => void setPresetAsGlobalDefault(preset, name)
    }
  ];
}

/** The photo's own menu entries in Develop: its settings (without its
 * masks, crop, transform and retouching) as its camera's (body's) default. */
export function photoDefaultItems(): Option[] {
  const open = openCamera();
  const image = useEditorStore.getState().selectedImage;
  if (!open || !image) return [];
  return levelsOf(open.camera).map((level) => ({
    label: labelFor('setForCamera', open.camera, level),
    icon: Camera,
    onClick: () =>
      void setPhotoAsCameraDefault(
        useEditorStore.getState().adjustments,
        open.isRaw,
        fileName(image.path),
        open.camera,
        level
      )
  }));
}
