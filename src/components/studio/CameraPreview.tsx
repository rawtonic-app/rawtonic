import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import type { SelectedImage } from "../ui/AppProperties";
import { useMultiViewStore } from "../../store/useMultiViewStore";
import { useRelightStore } from "../../store/useRelightStore";
import {
  TOGGLE_CAMERA_PREVIEW,
  cameraPreviewTarget,
  hideCameraPreview,
  toggleCameraPreview,
} from "../../utils/cameraPreview";
import { shortcutHint } from "../../utils/keyboardUtils";

/**
 * While the camera preview is shown: stepping to another photograph,
 * leaving Develop, another view of the photograph (multi view, before/after)
 * or a tool that edits it end it (back to the edit, never a camera JPEG of
 * the photograph left). Escape ends it in the shortcut handler, which takes
 * the key for it alone (useKeyboardShortcuts).
 */
export function useCameraPreviewGuard(): void {
  useEffect(() => {
    const check = () => {
      const state = useEditorStore.getState();
      if (!state.cameraPreview) return;
      if (
        state.selectedImage?.path !== state.cameraPreview.path ||
        cameraPreviewTarget() !== state.cameraPreview.path
      ) {
        hideCameraPreview();
      }
    };
    const stops = [
      useEditorStore.subscribe(check),
      useUIStore.subscribe(check),
      useMultiViewStore.subscribe(check),
      useRelightStore.subscribe(check),
    ];
    return () => {
      stops.forEach((stop) => stop());
      // Develop's viewer goes: so does its camera preview.
      if (useEditorStore.getState().cameraPreview) hideCameraPreview();
    };
  }, []);
}

/** The camera preview is on screen for the open photograph. */
export const cameraPreviewOnScreen = (state: ReturnType<typeof useEditorStore.getState>) =>
  !!state.cameraPreview && !!state.selectedImage && state.cameraPreview.path === state.selectedImage.path;

/**
 * Over the camera JPEG the native display draws in the edit's place: a
 * transparent layer that keeps the pointer from the edit underneath (Editor
 * leaves its gestures inert meanwhile too) and a label that says what it
 * is — the camera's own rendering of the photograph, not the edit (it keeps
 * the camera's colour, tone and JPEG crop).
 */
export function CameraPreview() {
  const { t } = useTranslation();
  const shown = useEditorStore(cameraPreviewOnScreen);
  const keybinds = useSettingsStore((s) => s.appSettings?.keybinds);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const key = shortcutHint(TOGGLE_CAMERA_PREVIEW, keybinds, osPlatform);
  useCameraPreviewGuard();
  if (!shown) return null;
  return (
    <div
      className="studio-camera-preview"
      data-bench-id="camera-preview"
      role="group"
      aria-label={t("studio.workspace.cameraPreview.label")}
    >
      <p className="studio-camera-preview-note" role="note">
        <strong>{t("studio.workspace.cameraPreview.label")}</strong>
        {t("studio.workspace.cameraPreview.note")}
        {/* How to leave it (the toggle's key, or Escape). */}
        <span className="studio-camera-preview-keys">
          {key ? t("studio.workspace.cameraPreview.returnWith", { key, esc: t("studio.workspace.cameraPreview.esc") }) : null}
        </span>
      </p>
    </div>
  );
}

/** Whether the photograph has a camera JPEG the toggle can show. */
export function cameraPreviewToggleState(selectedImage: SelectedImage | null | undefined) {
  // A JPEG embedded in a RAW: the read finds nothing in other files.
  const possible = !!selectedImage?.path && selectedImage.isRaw === true;
  return { possible, disabled: !possible || !selectedImage.isReady };
}

/**
 * The toggle beside the viewer (Shift+\, View ▸ Camera Preview): on, the camera
 * JPEG; off, the developed photo. Disabled, with a tooltip that says so,
 * when the photograph has no camera JPEG.
 */
export function CameraPreviewButton() {
  const { t } = useTranslation();
  const shown = useEditorStore(cameraPreviewOnScreen);
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const keybinds = useSettingsStore((s) => s.appSettings?.keybinds);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const { disabled, possible } = cameraPreviewToggleState(selectedImage);
  const key = shortcutHint(TOGGLE_CAMERA_PREVIEW, keybinds, osPlatform);
  const label = t("studio.workspace.cameraPreview.label");
  const title = !possible
    ? t("studio.workspace.cameraPreview.unavailable")
    : key
      ? t("studio.workspace.withShortcut", { label, shortcut: key })
      : label;
  return (
    <button
      type="button"
      aria-pressed={shown}
      aria-label={label}
      title={title}
      disabled={disabled}
      onClick={toggleCameraPreview}
    >
      <CameraIcon />
      <span className="optional-label">{t("studio.workspace.cameraPreview.button")}</span>
    </button>
  );
}

function CameraIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8"
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M4 7h3l1.5-2h7L17 7h3a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2z" />
      <circle cx="12" cy="13" r="3.5" />
    </svg>
  );
}
