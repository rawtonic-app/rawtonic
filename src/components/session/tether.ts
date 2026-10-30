import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { notify } from "../../activity/notify";
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

let experimental = false;
/** Whether the tethered camera is on an experimental driver path (Canon
 * EOS, the user's opt-in): a shot without its photo is then said to be on
 * the memory card. */
export const isTetherExperimental = () => experimental;
export const setTetherExperimental = (value: boolean) => {
  experimental = value;
};

/** The helper's report of a shot whose photo never came (its timeout, with
 * no exposure): not a download that failed or timed out, which names the
 * exposure it belongs to and keeps its own message. */
export const shotNotReceived = (payload: { kind?: string; exposure?: number | null }) =>
  payload.kind === "timeout" && payload.exposure == null;

/** The message for a shot that was not delivered (`tether-capture-failed`):
 * on an experimental path a timed-out shot is on the memory card, said in
 * the user's language and kept on screen until dismissed. */
export const notDeliveredToast = (payload: {
  kind?: string;
  message: string;
  request?: number | null;
  exposure?: number | null;
}) =>
  shotNotReceived(payload) && experimental
    ? { text: i18n.t("session.tether.canon.notReceived", { n: payload.request ?? "?" }), sticky: true }
    : { text: i18n.t("session.tether.captureFailed", { error: payload.message }), sticky: false };

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
      notify.error(i18n.t("session.tether.captureFailed", { error: String(e) }), { source: "tether" }),
    );
  });
};
