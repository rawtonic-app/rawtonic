import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "react-toastify";
import i18n from "i18next";
import { Invokes } from "../ui/AppProperties";

/** Fires the tethered camera (the Capture shortcut, Capture One's ⌘K). */
export const TETHER_CAPTURE_EVENT = "rawtonic:tether-capture";

let connected = false;
/** Tether panels listening for the shortcut (they fire it themselves). */
let panels = 0;

/** Whether a camera is tethered now (the Capture shortcut only fires then). */
export const isTetherConnected = () => connected;
export const setTetherConnected = (value: boolean) => {
  install();
  connected = value;
};

/** A tether panel handles the shortcut while it is mounted; returns its
 * release. */
export const panelHandlesCapture = () => {
  panels += 1;
  return () => {
    panels -= 1;
  };
};

// The camera stays connected while no panel is mounted (the session view is
// hidden in fullscreen): the backend's state, not a panel's lifetime, says
// whether it is, and the shortcut still fires it then. Installed once, by
// the first panel (every connection starts in one).
let installed = false;
const install = () => {
  if (installed || typeof window === "undefined" || !isTauri()) return;
  installed = true;
  void listen<{ connected: boolean }>("tether-state", (event) => {
    connected = event.payload.connected;
  });
  window.addEventListener(TETHER_CAPTURE_EVENT, () => {
    if (panels > 0 || !connected) return;
    invoke(Invokes.TetherCapture).catch((e) =>
      toast.error(i18n.t("session.tether.captureFailed", { error: String(e) })),
    );
  });
};
