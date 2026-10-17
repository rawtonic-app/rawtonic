import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  elementName,
  findElements,
  handleQaRequest,
  snapshot,
} from "./qaControl";
import { useEditorStore } from "../store/useEditorStore";

vi.mock("@tauri-apps/api/event", () => ({ emit: vi.fn(), listen: vi.fn() }));

function layout(
  element: Element,
  x: number,
  y: number,
  width: number,
  height: number,
) {
  element.getBoundingClientRect = () =>
    ({
      x,
      y,
      width,
      height,
      left: x,
      top: y,
      right: x + width,
      bottom: y + height,
      toJSON: () => ({}),
    }) as DOMRect;
}

describe("QA control bridge", () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <button id="export" title="Export photos">Export</button>
      <button id="icon" aria-label="Show mask"></button>
      <button id="hidden">Export later</button>
      <input id="search" placeholder="Search" value="sky" />
      <span id="plain">Not interactive</span>`;
    layout(document.getElementById("export")!, 10, 20, 100, 30);
    layout(document.getElementById("icon")!, 120, 20, 24, 24);
    layout(document.getElementById("search")!, 10, 60, 200, 24);
    layout(document.getElementById("plain")!, 10, 90, 50, 10);
  });
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("names elements as a user reads them", () => {
    expect(elementName(document.getElementById("icon")!)).toBe("Show mask");
    expect(elementName(document.getElementById("export")!)).toBe(
      "Export photos",
    );
    expect(elementName(document.getElementById("search")!)).toBe("Search");
  });

  it("lists visible interactive elements with their rectangles", () => {
    const names = findElements({}).map((match) => match.info.name);
    expect(names).toEqual(["Export photos", "Show mask", "Search"]);
    const [exportButton] = findElements({ name: "export" });
    expect(exportButton.info).toMatchObject({
      index: 0,
      tag: "button",
      x: 10,
      y: 20,
      width: 100,
      height: 30,
    });
    expect(findElements({ name: "search", exact: true })[0].info.value).toBe(
      "sky",
    );
    expect(findElements({ name: "expo", exact: true })).toHaveLength(0);
  });

  it("clicks the centre of a named element", async () => {
    const clicked = vi.fn();
    document.getElementById("icon")!.addEventListener("click", clicked);
    const result = await handleQaRequest("ui.click", { name: "show mask" });
    expect(clicked).toHaveBeenCalledOnce();
    expect(result).toMatchObject({ x: 132, y: 32 });
    await expect(
      handleQaRequest("ui.click", { name: "missing" }),
    ).rejects.toThrow(/no element/);
  });

  it('runs a timed range sequence and restores the value before releasing', async () => {
    const field = document.getElementById('search') as HTMLInputElement;
    field.type = 'range'; field.min = '-5'; field.max = '5'; field.step = '.1'; field.value = '.5';
    const inputs: string[] = [];
    field.addEventListener('input', () => inputs.push(field.value));
    const released = vi.fn(() => expect(Number(field.value)).toBe(0.5));
    field.addEventListener('mouseup', released);
    const result = await handleQaRequest('ui.range_sequence', { selector: '#search', values: [1, 2, -1], interval_ms: 4 });
    expect(inputs).toEqual(['1', '2', '-1', '.5']);
    expect(released).toHaveBeenCalledOnce();
    expect(result).toMatchObject({ restored: true });
  });

  it('counts animation frames during a measured range sequence', async () => {
    const field = document.getElementById('search') as HTMLInputElement;
    field.type = 'range'; field.min = '0'; field.max = '5'; field.value = '0';
    const result = await handleQaRequest('ui.range_sequence', { selector: '#search', values: [1, 2, 3, 4], interval_ms: 20, measure: true });
    expect(result).toMatchObject({ restored: true, drafts_shown: 0 });
    expect((result as { frames: number }).frames).toBeGreaterThan(0);
  });

  it("dispatches keys to the focused element", async () => {
    const pressed = vi.fn(
      (event: KeyboardEvent) =>
        event.altKey && event.key === "m" && event.preventDefault(),
    );
    window.addEventListener("keydown", pressed);
    const result = await handleQaRequest("ui.key", {
      key: "m",
      code: "KeyM",
      modifiers: ["alt"],
    });
    window.removeEventListener("keydown", pressed);
    expect(pressed).toHaveBeenCalledOnce();
    expect(result).toMatchObject({ defaultPrevented: true });
  });

  it("reads store values by path and waits for them", async () => {
    useEditorStore.setState({ hasRenderedFirstFrame: true });
    expect(
      await handleQaRequest("state.get", {
        path: "editor.hasRenderedFirstFrame",
      }),
    ).toBe(true);
    await expect(
      handleQaRequest("state.wait", {
        path: "editor.hasRenderedFirstFrame",
        equals: true,
      }),
    ).resolves.toMatchObject({ value: true });
    await expect(
      handleQaRequest("state.get", { path: "nope.x" }),
    ).rejects.toThrow(/unknown store/);
  });

  it("snapshots without functions, long arrays or deep nesting", () => {
    const value = {
      run: () => 1,
      list: [1, 2, 3, 4],
      deep: { a: { b: { c: 1 } } },
      bytes: new Uint8Array(8),
      set: new Set(["x"]),
    };
    expect(snapshot(value, 2, 3)).toEqual({
      list: [1, 2, "[… 2 more]"],
      deep: { a: { b: "[…]" } },
      bytes: "[Uint8Array 8 bytes]",
      set: ["x"],
    });
  });
});

describe("dialog answers", () => {
  afterEach(() => handleQaRequest("dialog.clear", {}));

  it("queues paths, a cancel and lists for the next dialogs", async () => {
    expect(
      await handleQaRequest("dialog.answer", { path: "/QA/A.NEF" }),
    ).toEqual({ pending: 1 });
    expect(
      await handleQaRequest("dialog.answer", {
        paths: [null, ["/QA/B.NEF", "/QA/C.NEF"]],
      }),
    ).toEqual({ pending: 3 });
    expect(await handleQaRequest("dialog.state", {})).toEqual({
      pending: 3,
      asked: [],
    });
  });

  it("refuses answers that are not paths", async () => {
    await expect(handleQaRequest("dialog.answer", {})).rejects.toThrow(
      /path must be/,
    );
    await expect(
      handleQaRequest("dialog.answer", { path: 3 }),
    ).rejects.toThrow(/path must be/);
    await expect(
      handleQaRequest("dialog.answer", { paths: [[1]] }),
    ).rejects.toThrow(/paths must/);
    expect(await handleQaRequest("dialog.state", {})).toEqual({
      pending: 0,
      asked: [],
    });
  });
});
