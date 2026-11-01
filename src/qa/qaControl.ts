// Webview side of the QA control channel (src-tauri/src/qa_control.rs).
// Loaded only in builds with VITE_RAWTONIC_QA=1; ordinary builds drop it.
// Methods inspect the interface or act on it as a user could; none of them
// writes files or changes stores directly.
import { emit, listen } from "@tauri-apps/api/event";
import {
  clearDialogAnswers,
  type DialogAnswer,
  dialogState,
  queueDialogAnswers,
} from "./dialogAnswers";
import { useActivityStore } from "../store/useActivityStore";
import { useEditorStore } from "../store/useEditorStore";
import { onDraftShown } from "../utils/browserDraftPatch";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProcessStore } from "../store/useProcessStore";
import { useSettingsStore } from "../store/useSettingsStore";
import { useUIStore } from "../store/useUIStore";

type Params = Record<string, unknown>;

interface QaRequest {
  id: number;
  method: string;
  params?: Params;
}

export interface QaElement {
  index: number;
  tag: string;
  role: string | null;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  disabled: boolean;
  value?: string;
}

const stores: Record<string, { getState: () => unknown }> = {
  activity: useActivityStore,
  editor: useEditorStore,
  library: useLibraryStore,
  process: useProcessStore,
  settings: useSettingsStore,
  ui: useUIStore,
};

const INTERACTIVE = [
  "button",
  "a[href]",
  "input",
  "select",
  "textarea",
  "label",
  "summary",
  "canvas",
  "[role]",
  "[aria-label]",
  "[title]",
  "[tabindex]",
  '[contenteditable="true"]',
].join(",");

const squeeze = (text: string | null | undefined) =>
  (text ?? "").replace(/\s+/g, " ").trim();

/** What a user would read as the element's name. */
export function elementName(element: Element): string {
  const labelled = element.getAttribute("aria-labelledby");
  const byId = labelled
    ? squeeze(document.getElementById(labelled)?.textContent)
    : "";
  return (
    squeeze(element.getAttribute("aria-label")) ||
    byId ||
    squeeze(element.getAttribute("title")) ||
    squeeze(element.getAttribute("placeholder")) ||
    squeeze(element.getAttribute("alt")) ||
    squeeze((element as HTMLElement).innerText ?? element.textContent).slice(
      0,
      80,
    ) ||
    squeeze(element.getAttribute("name"))
  );
}

function isVisible(element: Element, rect: DOMRect) {
  if (rect.width <= 0 || rect.height <= 0) return false;
  const style = getComputedStyle(element);
  return (
    style.visibility !== "hidden" &&
    style.display !== "none" &&
    style.opacity !== "0"
  );
}

function describe(element: Element, index: number): QaElement {
  const rect = element.getBoundingClientRect();
  const value = (element as HTMLInputElement).value;
  return {
    index,
    tag: element.tagName.toLowerCase(),
    role: element.getAttribute("role"),
    name: elementName(element),
    x: Math.round(rect.x),
    y: Math.round(rect.y),
    width: Math.round(rect.width),
    height: Math.round(rect.height),
    disabled:
      (element as HTMLButtonElement).disabled === true ||
      element.getAttribute("aria-disabled") === "true",
    ...(typeof value === "string" && element.matches("input, select, textarea")
      ? { value }
      : {}),
  };
}

/** Visible interactive elements, filtered by name (case-insensitive substring,
 * or exact), role/tag or a CSS selector. Indices count matches in document order. */
export function findElements(
  params: Params,
): { element: Element; info: QaElement }[] {
  const name =
    typeof params.name === "string" ? params.name.toLowerCase() : null;
  const exact = params.exact === true;
  const role = typeof params.role === "string" ? params.role : null;
  const selector =
    typeof params.selector === "string" ? params.selector : INTERACTIVE;
  const matches: { element: Element; info: QaElement }[] = [];
  for (const element of document.querySelectorAll(selector)) {
    if (!isVisible(element, element.getBoundingClientRect())) continue;
    if (
      role &&
      element.getAttribute("role") !== role &&
      element.tagName.toLowerCase() !== role
    )
      continue;
    const label = elementName(element).toLowerCase();
    if (name !== null && (exact ? label !== name : !label.includes(name)))
      continue;
    matches.push({ element, info: describe(element, matches.length) });
  }
  return matches;
}

function findOne(params: Params) {
  const matches = findElements(params);
  const index = typeof params.index === "number" ? params.index : 0;
  const match = matches[index];
  if (!match) {
    throw new Error(
      `no element matches ${JSON.stringify({ ...params, index })} (${matches.length} found)`,
    );
  }
  return match;
}

function point(params: Params): { x: number; y: number; target: Element } {
  if (typeof params.x === "number" && typeof params.y === "number") {
    const target = document.elementFromPoint(params.x, params.y);
    if (!target) throw new Error(`nothing at ${params.x},${params.y}`);
    return { x: params.x, y: params.y, target };
  }
  const { element, info } = findOne(params);
  return {
    x: info.x + info.width / 2,
    y: info.y + info.height / 2,
    target: element,
  };
}

const PointerCtor: typeof MouseEvent =
  typeof PointerEvent === "function" ? PointerEvent : MouseEvent;

function modifiers(params: Params) {
  const list = Array.isArray(params.modifiers) ? params.modifiers : [];
  return {
    shiftKey: list.includes("shift"),
    ctrlKey: list.includes("control"),
    altKey: list.includes("option") || list.includes("alt"),
    metaKey: list.includes("command") || list.includes("meta"),
  };
}

function firePointer(
  target: Element,
  type: string,
  x: number,
  y: number,
  buttons: number,
  extra: object,
) {
  const init = {
    bubbles: true,
    cancelable: true,
    composed: true,
    clientX: x,
    clientY: y,
    button: 0,
    buttons,
    pointerId: 1,
    pointerType: "mouse",
    isPrimary: true,
    ...extra,
  };
  target.dispatchEvent(new PointerCtor(`pointer${type}`, init));
  const mouse =
    type === "down" ? "mousedown" : type === "up" ? "mouseup" : "mousemove";
  target.dispatchEvent(new MouseEvent(mouse, init));
}

/** Synthetic (untrusted) DOM click; native input.click is closer to real input.
 * `button: "right"` presses the secondary button and opens a context menu. */
function domClick(params: Params) {
  const { x, y, target } = point(params);
  const extra = modifiers(params);
  if (params.button === "right") {
    const right = { ...extra, button: 2 };
    firePointer(target, "down", x, y, 2, right);
    firePointer(target, "up", x, y, 0, right);
    target.dispatchEvent(
      new MouseEvent("contextmenu", {
        bubbles: true,
        cancelable: true,
        clientX: x,
        clientY: y,
        button: 2,
        ...extra,
      }),
    );
    return { x, y, target: describe(target, 0) };
  }
  const count = typeof params.count === "number" ? params.count : 1;
  for (let detail = 1; detail <= count; detail += 1) {
    firePointer(target, "down", x, y, 1, { ...extra, detail });
    firePointer(target, "up", x, y, 0, { ...extra, detail });
    target.dispatchEvent(
      new MouseEvent("click", {
        bubbles: true,
        cancelable: true,
        clientX: x,
        clientY: y,
        detail,
        ...extra,
      }),
    );
  }
  if (count === 2) {
    target.dispatchEvent(
      new MouseEvent("dblclick", {
        bubbles: true,
        cancelable: true,
        clientX: x,
        clientY: y,
        detail: 2,
        ...extra,
      }),
    );
  }
  return { x, y, target: describe(target, 0) };
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** Synthetic drag for platforms without native input (Linux). With
 * `pressures` (one 0–1 per point) it is a pen drag: synthetic pen pressure
 * tests the stroke's pressure mapping, not a tablet's hardware path. */
async function domDrag(params: Params) {
  const points = params.points as [number, number][];
  if (!Array.isArray(points) || points.length < 2)
    throw new Error("a drag needs at least two points");
  const pressures = Array.isArray(params.pressures)
    ? (params.pressures as number[])
    : null;
  if (pressures && pressures.length !== points.length)
    throw new Error("pressures needs one value per point");
  const pen = (i: number) =>
    pressures ? { pointerType: "pen", pressure: pressures[i] } : {};
  const interval =
    typeof params.interval_ms === "number" ? params.interval_ms : 1000 / 60;
  const extra = modifiers(params);
  const [x0, y0] = points[0];
  // `name`/`selector` (as ui.click takes them) pins every event to that
  // element: a press held on one control, where hit-testing by point is
  // unreliable (WebKitGTK under Xvfb returned the panel for its buttons).
  const pinned = params.name !== undefined || params.selector !== undefined ? findOne(params).element : null;
  const start = pinned ?? document.elementFromPoint(x0, y0);
  if (!start) throw new Error(`nothing at ${x0},${y0}`);
  firePointer(start, "down", x0, y0, 1, { ...extra, ...pen(0) });
  for (const [i, [x, y]] of points.entries()) {
    if (i === 0) continue;
    await sleep(interval);
    firePointer(
      pinned ?? document.elementFromPoint(x, y) ?? start,
      "move",
      x,
      y,
      1,
      { ...extra, ...pen(i) },
    );
  }
  const [x1, y1] = points[points.length - 1];
  firePointer(
    pinned ?? document.elementFromPoint(x1, y1) ?? start,
    "up",
    x1,
    y1,
    0,
    extra,
  );
  return { points: points.length, interval_ms: interval };
}

/** Synthetic hover: pointer moves without a pressed button over `points`
 * (each to the element there), `interval_ms` apart. Reaches the page in a
 * background window, where native mouse moves are not delivered; it does not
 * set CSS :hover. */
async function domHover(params: Params) {
  const points = params.points as [number, number][];
  if (!Array.isArray(points) || points.length < 1)
    throw new Error("a hover needs at least one point");
  const interval =
    typeof params.interval_ms === "number" ? params.interval_ms : 1000 / 60;
  const extra = modifiers(params);
  for (const [i, [x, y]] of points.entries()) {
    if (i > 0) await sleep(interval);
    const target = document.elementFromPoint(x, y);
    if (target) firePointer(target, "move", x, y, 0, extra);
  }
  return { points: points.length, interval_ms: interval };
}

function key(params: Params) {
  if (typeof params.key !== "string") throw new Error("key must be a string");
  const target = document.activeElement ?? document.body;
  const init = {
    key: params.key,
    code: typeof params.code === "string" ? params.code : "",
    bubbles: true,
    cancelable: true,
    ...modifiers(params),
  };
  // phase "down" or "up" sends one half of a press (holding a key while
  // other input arrives, e.g. Quick Adjust); default both.
  const phase = params.phase === "down" || params.phase === "up" ? params.phase : "press";
  const handled =
    phase !== "up" && !target.dispatchEvent(new KeyboardEvent("keydown", init));
  if (phase !== "down") target.dispatchEvent(new KeyboardEvent("keyup", init));
  return { target: target.tagName.toLowerCase(), defaultPrevented: handled };
}

/** Text into a field as typing would leave it: focus, the value set through
 * the native setter (so React sees the change) and an `input` event. A field
 * is named like other elements; a matched `<label>` stands for its control.
 * Without a name the focused element is used. */
function typeText(params: Params) {
  if (typeof params.text !== "string") throw new Error("text must be a string");
  let target: Element | null =
    params.name || params.selector ? findOne(params).element : document.activeElement;
  if (target instanceof HTMLLabelElement) target = target.control;
  if (target instanceof HTMLSelectElement) {
    // Selects take the option value (or its text).
    const option = [...target.options].find(
      (o) => o.value === params.text || o.text === params.text,
    );
    if (!option) throw new Error(`no option ${params.text}`);
    target.focus();
    const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set;
    setter?.call(target, option.value);
    target.dispatchEvent(new Event("change", { bubbles: true }));
    return { target: "select", value: target.value };
  }
  if (!(target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement))
    throw new Error("no text field matched");
  target.focus();
  const prototype = target instanceof HTMLInputElement ? HTMLInputElement : HTMLTextAreaElement;
  const setter = Object.getOwnPropertyDescriptor(prototype.prototype, "value")?.set;
  const value = params.append === true ? target.value + params.text : params.text;
  setter?.call(target, value);
  target.dispatchEvent(new Event("input", { bubbles: true }));
  return { target: target.tagName.toLowerCase(), value: target.value };
}

/** Timed range inputs in the webview, without one control-channel roundtrip
 * per value. A bounded QA-only synthetic drag, not physical mouse input. */
async function rangeSequence(params: Params) {
  const target = findOne(params).element;
  const values = params.values;
  const interval = typeof params.interval_ms === 'number' ? params.interval_ms : 1000 / 60;
  if (!(target instanceof HTMLInputElement) || target.type !== 'range' || target.disabled)
    throw new Error('an enabled range input is required');
  if (!Array.isArray(values) || values.length < 1 || values.length > 600 ||
    values.some((v) => typeof v !== 'number' || !Number.isFinite(v)) || !Number.isFinite(interval) || interval < 4 || interval > 100)
    throw new Error('1–600 finite values and an interval of 4–100 ms are required');
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
  const rect = target.getBoundingClientRect();
  const x = rect.left + rect.width / 2, y = rect.top + rect.height / 2;
  const original = target.value;
  // `measure`: animation frames and browser drafts shown between press and
  // release, for builds without performance profiling (production React, no
  // [PERF] records) — counts in the webview, not presented display frames.
  const frames: number[] = [];
  const drafts: number[] = [];
  let measuring = params.measure === true;
  const frame = () => {
    if (!measuring) return;
    frames.push(performance.now());
    requestAnimationFrame(frame);
  };
  const stopDrafts = measuring ? onDraftShown(() => drafts.push(performance.now())) : () => {};
  if (measuring) requestAnimationFrame(frame);
  const pressed = performance.now();
  target.focus();
  firePointer(target, 'down', x, y, 1, modifiers(params));
  // Let the range's dragging state reach the renderer before its first input.
  await sleep(interval);
  const started = performance.now();
  const startEpoch = Date.now();
  const inputTimes: number[] = [];
  try {
    for (let i = 0; i < values.length; i++) {
      const remaining = started + i * interval - performance.now();
      if (remaining > 0) await sleep(remaining);
      if (!target.isConnected) throw new Error('range was removed during the sequence');
      firePointer(target, 'move', x, y, 1, modifiers(params));
      setter?.call(target, String(values[i]));
      target.dispatchEvent(new Event('input', { bubbles: true }));
      inputTimes.push(performance.now() - started);
    }
  } finally {
    // Restore before releasing, including when a sequence is interrupted.
    setter?.call(target, original);
    target.dispatchEvent(new Event('input', { bubbles: true }));
    firePointer(target, 'up', x, y, 0, modifiers(params));
  }
  const released = performance.now();
  const result = { start_epoch_ms: startEpoch, end_epoch_ms: Date.now(), input_times_ms: inputTimes, restored: target.value === original };
  if (!measuring) return result;
  measuring = false;
  stopDrafts();
  // As pointer sessions count them: each frame in the drag with its gap to the one before.
  const intervals = frames.slice(1).map((t, i) => [t, t - frames[i]])
    .filter(([t]) => t >= pressed && t <= released).map(([, gap]) => gap).sort((a, b) => a - b);
  const at = (q: number) => intervals[Math.min(intervals.length - 1, Math.floor(intervals.length * q))] ?? 0;
  return {
    ...result,
    frames: intervals.length,
    frame_p50_ms: at(0.5),
    frame_p95_ms: at(0.95),
    drafts_shown: drafts.filter((t) => t >= pressed && t <= released).length,
  };
}

/** Value at `store.path.to.value` ("editor.selectedImage.path"). */
export function stateAt(path: string): unknown {
  const [storeName, ...keys] = path.split(".").filter(Boolean);
  const store = stores[storeName];
  if (!store)
    throw new Error(
      `unknown store ${storeName}; one of ${Object.keys(stores).join(", ")}`,
    );
  let value: unknown = store.getState();
  for (const keyName of keys) {
    if (value === null || typeof value !== "object") return undefined;
    value = (value as Record<string, unknown>)[keyName];
  }
  return value;
}

/** JSON-safe copy: no functions, long arrays and deep objects cut off. */
export function snapshot(
  value: unknown,
  maxArray = 50,
  maxDepth = 6,
  depth = 0,
): unknown {
  if (
    value === null ||
    typeof value === "string" ||
    typeof value === "number" ||
    typeof value === "boolean"
  )
    return value;
  if (
    typeof value === "function" ||
    typeof value === "symbol" ||
    value === undefined
  )
    return undefined;
  if (typeof value === "bigint") return value.toString();
  if (depth >= maxDepth) return "[…]";
  if (value instanceof Map) value = Object.fromEntries(value);
  if (value instanceof Set) value = [...value];
  if (Array.isArray(value)) {
    const items = value
      .slice(0, maxArray)
      .map((item) => snapshot(item, maxArray, maxDepth, depth + 1) ?? null);
    return value.length > maxArray
      ? [...items, `[… ${value.length - maxArray} more]`]
      : items;
  }
  if (ArrayBuffer.isView(value))
    return `[${value.constructor.name} ${(value as ArrayBufferView).byteLength} bytes]`;
  const out: Record<string, unknown> = {};
  for (const [name, item] of Object.entries(value as object)) {
    const copy = snapshot(item, maxArray, maxDepth, depth + 1);
    if (copy !== undefined) out[name] = copy;
  }
  return out;
}

async function waitFor(params: Params) {
  const timeout =
    typeof params.timeout_ms === "number" ? params.timeout_ms : 10_000;
  const started = performance.now();
  for (;;) {
    if (typeof params.path === "string") {
      const value = stateAt(params.path);
      const done =
        "equals" in params
          ? JSON.stringify(value) === JSON.stringify(params.equals)
          : Boolean(value);
      if (done)
        return { ms: performance.now() - started, value: snapshot(value) };
    } else {
      const matches = findElements(params);
      if (matches.length > 0)
        return { ms: performance.now() - started, element: matches[0].info };
    }
    if (performance.now() - started > timeout)
      throw new Error(`timed out after ${timeout} ms`);
    await sleep(50);
  }
}

/** `dialog.answer` params: `path` (string, list or null = cancel) or
 * `paths` (several answers for consecutive dialogs). */
function dialogAnswers(params: Params): DialogAnswer[] {
  const valid = (value: unknown): value is DialogAnswer =>
    value === null ||
    typeof value === "string" ||
    (Array.isArray(value) && value.every((item) => typeof item === "string"));
  if (Array.isArray(params.paths)) {
    if (!params.paths.every(valid))
      throw new Error("paths must hold strings, string lists or null");
    return params.paths as DialogAnswer[];
  }
  if (!("path" in params) || !valid(params.path))
    throw new Error("path must be a string, a string list or null");
  return [params.path];
}

export async function handleQaRequest(
  method: string,
  params: Params,
): Promise<unknown> {
  const maxArray = typeof params.max_array === "number" ? params.max_array : 50;
  switch (method) {
    case "ui.elements":
      return findElements(params)
        .slice(0, typeof params.limit === "number" ? params.limit : 200)
        .map((m) => m.info);
    case "ui.find":
      return findOne(params).info;
    case "ui.click":
      return domClick(params);
    case "ui.drag":
      return domDrag(params);
    case "ui.hover":
      return domHover(params);
    case "ui.key":
      return key(params);
    case "ui.type":
      return typeText(params);
    case "ui.range_sequence":
      return rangeSequence(params);
    case "ui.text": {
      const root =
        params.name || params.selector
          ? findOne(params).element
          : document.body;
      const max = typeof params.max === "number" ? params.max : 4000;
      return squeeze((root as HTMLElement).innerText ?? root.textContent).slice(
        0,
        max,
      );
    }
    case "ui.viewport":
      return {
        width: innerWidth,
        height: innerHeight,
        devicePixelRatio,
        focused: document.hasFocus(),
      };
    case "state.get":
      if (typeof params.path !== "string")
        throw new Error('path must be "store.key…"');
      return (
        snapshot(
          stateAt(params.path),
          maxArray,
          typeof params.max_depth === "number" ? params.max_depth : 6,
        ) ?? null
      );
    case "state.wait":
    case "ui.wait":
      return waitFor(params);
    case "dialog.answer":
      return queueDialogAnswers(dialogAnswers(params));
    case "dialog.state":
      return dialogState();
    case "dialog.clear":
      return clearDialogAnswers();
    default:
      throw new Error(`unknown method ${method}`);
  }
}

export function startQaControl() {
  void listen<QaRequest>("qa-control-request", async ({ payload }) => {
    let reply: { id: number; result?: unknown; error?: string };
    try {
      reply = {
        id: payload.id,
        result:
          (await handleQaRequest(payload.method, payload.params ?? {})) ?? null,
      };
    } catch (error) {
      reply = {
        id: payload.id,
        error: error instanceof Error ? error.message : String(error),
      };
    }
    await emit("qa-control-reply", reply);
  });
}
