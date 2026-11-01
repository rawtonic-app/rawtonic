import { invoke } from '@tauri-apps/api/core';
import i18n from 'i18next';
import { notify } from '../activity/notify';
import { Invokes, Panel } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useMultiViewStore } from '../store/useMultiViewStore';
import { useRelightStore } from '../store/useRelightStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useUIStore } from '../store/useUIStore';

/**
 * The camera preview: the JPEG the camera itself wrote into a RAW, shown in
 * place of the edit (Lightroom's embedded preview, Capture One's camera
 * thumbnail). It shows what the camera rendered — its own colour, tone and
 * JPEG crop — not the edit, so it is a view only: the recipe, the history
 * and the file are never touched.
 *
 * The native display draws it (src-tauri camera_preview.rs → the gpu
 * crate's camera_cover): the same camera JPEG the app shows as a RAW's
 * still while stepping (`embedded_still`), fitted to the viewer whatever the
 * edit's zoom, in place of the developed frame — never a webview image over
 * an edit still on screen. Shift+\ (View ▸ Camera Preview, the toolbar
 * button) switches it on and off; Escape, stepping to another photograph,
 * leaving Develop and a tool that edits end it (useCameraPreviewGuard).
 */

/** The key, menu item and toolbar button a view toggle shares. */
export const TOGGLE_CAMERA_PREVIEW = 'toggle_camera_preview';

/** The RAW whose camera preview can be shown now, or null. */
export function cameraPreviewPath(): string | null {
  if (useUIStore.getState().activeView !== 'editor') return null;
  // The native display draws it; the webview renderer has none.
  if (useSettingsStore.getState().appSettings?.useWgpuRenderer === false) return null;
  const image = useEditorStore.getState().selectedImage;
  if (!image?.path || !image.isReady || !image.isRaw) return null;
  return image.path;
}

/** Whether the photograph has a camera JPEG at all: a RAW that is not a
 *  cloud placeholder (the JPEG lives in the RAW file, and the read finds none
 *  in other files). The listing says so, or with none the opened photograph. */
export function cameraPreviewPossible(path: string | null | undefined): boolean {
  if (!path) return false;
  const listed = useLibraryStore.getState().imageList.find((image) => image.path === path);
  if (listed) return listed.is_raw === true && !listed.is_cloud_placeholder;
  const open = useEditorStore.getState().selectedImage;
  return open?.path === path ? open.isRaw === true : false;
}

/**
 * The camera preview is a view of the edit's photograph, so a tool that
 * works on the photo (crop and straighten, a mask, the WB, colour, Levels
 * and luminance pickers, targeted adjustment, the spot tool, the face
 * retouch's brush and overlays, Relight's handles, guided perspective)
 * brings the edit back instead of acting under the JPEG.
 */
export function editingToolOnScreen(): boolean {
  const state = useEditorStore.getState();
  const panel = useUIStore.getState().activePanel;
  const relight = useRelightStore.getState();
  return (
    panel === Panel.Crop ||
    panel === Panel.Masks ||
    !!state.isWbPickerActive ||
    !!state.isStraightenActive ||
    !!state.isRotationActive ||
    !!state.isSpotToolActive ||
    !!state.isGuidedPerspectiveActive ||
    !!state.activeMaskContainerId ||
    !!state.shapeDrawTarget ||
    state.colorPickerTarget != null ||
    !!state.lumaPickerLayer ||
    // Levels' pickers and the on-photo curve/HSL drags.
    !!state.tatTarget ||
    !!state.faceProtect?.painting ||
    !!state.showFaceBlemishes ||
    !!state.showFaceProtection ||
    (relight.panelOpen && relight.showHandles)
  );
}

/**
 * Whether the camera preview is on screen or can be switched on: Develop, a
 * ready RAW, no multi view (those are photographs side by side), neither of
 * the before/after views (they compare the edit with its own before) and no
 * tool that edits on screen.
 */
export function cameraPreviewTarget(): string | null {
  const path = cameraPreviewPath();
  if (!path || !cameraPreviewPossible(path)) return null;
  const multi = useMultiViewStore.getState();
  if (multi.active || multi.libraryEnabled) return null;
  const { splitView, showOriginal, toolBefore } = useEditorStore.getState();
  return splitView || showOriginal || toolBefore || editingToolOnScreen() ? null : path;
}

/** Whether the camera preview is on screen (for `path`: of that photograph). */
export function cameraPreviewShown(path?: string | null): boolean {
  const { cameraPreview, selectedImage } = useEditorStore.getState();
  if (!cameraPreview || cameraPreview.path !== selectedImage?.path) return false;
  return !path || cameraPreview.path === path;
}

/** Counts requests: a hide (or another show) after a show was sent makes
 *  its late answer void. */
let request = 0;
/** The show that is wanted now (sent or on screen); null after a hide. */
let live: number | null = null;

/**
 * Switches the camera preview on for the open photograph (a no-op when it
 * cannot be shown); resolves once it is on, or found missing (no camera
 * JPEG in the file).
 */
export async function showCameraPreview(): Promise<{ shown: boolean; missing: boolean }> {
  const path = cameraPreviewTarget();
  if (!path) return { shown: false, missing: false };
  if (cameraPreviewShown(path)) return { shown: true, missing: false };
  const asked = ++request;
  live = asked;
  // Optional view: a read that failed shows the edit.
  const shown = await invoke<boolean>(Invokes.ShowCameraPreview, { path }).then(
    (answer) => answer === true,
    () => false,
  );
  if (asked !== request) {
    // Voided. Its cover only goes when no newer show is wanted: a newer one
    // shows the same photograph's cover (the native side refuses another
    // photograph's), so taking it away would hide the newer preview.
    if (shown && live === null) void invoke(Invokes.HideCameraPreview).catch(() => {});
    return { shown: false, missing: false };
  }
  if (!shown) {
    live = null;
    // An earlier voided show may have put its cover up meanwhile.
    void invoke(Invokes.HideCameraPreview).catch(() => {});
    return { shown: false, missing: true };
  }
  // Still showable (a view or tool switched on meanwhile)?
  if (cameraPreviewTarget() !== path) {
    hideCameraPreview();
    return { shown: false, missing: false };
  }
  useEditorStore.getState().setEditor({ cameraPreview: { path } });
  return { shown: true, missing: false };
}

/** Back to the edit: every way out (the toggle, Escape, a step, a tool
 *  that edits, leaving Develop) ends here. */
export function hideCameraPreview(): void {
  request += 1;
  live = null;
  if (useEditorStore.getState().cameraPreview) useEditorStore.getState().setEditor({ cameraPreview: null });
  void invoke(Invokes.HideCameraPreview).catch(() => {});
}

/** The toggle (Shift+\, View ▸ Camera Preview, the toolbar button). A
 *  RAW without a camera JPEG says so instead of doing nothing. */
export function toggleCameraPreview(): void {
  // On screen, or asked for and not answered yet: the key again cancels.
  if (cameraPreviewShown() || live !== null) hideCameraPreview();
  else
    void showCameraPreview().then(({ missing }) => {
      if (missing) notify.info(i18n.t('studio.workspace.cameraPreview.unavailable'));
    });
}
