/** Fires the tethered camera (the Capture shortcut, Capture One's ⌘K). */
export const TETHER_CAPTURE_EVENT = "rawtonic:tether-capture";

let connected = false;

/** Whether a camera is tethered now (the Capture shortcut only fires then). */
export const isTetherConnected = () => connected;
export const setTetherConnected = (value: boolean) => {
  connected = value;
};
