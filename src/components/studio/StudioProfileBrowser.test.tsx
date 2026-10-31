import "../../i18n";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invoke, open } = vi.hoisted(() => ({ invoke: vi.fn(), open: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open }));

import StudioProfileBrowser, { splitTiles, withVariant } from "./StudioProfileBrowser";
import { useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { debouncedSave, debouncedSetHistory } from "../../hooks/useEditorActions";
import { resetBuiltinPresets } from "../../utils/builtinPresets";
import { INITIAL_ADJUSTMENTS, type Adjustments } from "../../utils/adjustments";
import type { AppSettings } from "../ui/AppProperties";

const studio = { value: "profiles/Studio.dcp", name: "Studio flash", camera: "Canon EOS R5" };
const luts = [
  { name: "Warm 200", path: "/res/Warm 200.cube", isBuiltIn: true },
  { name: "Night.cube", path: "/luts/Night.cube", isBuiltIn: false, group: "Moody pack" },
];
const mixers = [
  {
    id: "bw-mix-darker-skies",
    version: 1,
    name: "Darker skies",
    presetType: "tool",
    adjustments: { blackAndWhite: true, grayMixer: { red: 15, orange: 10, yellow: 5, green: 0, aqua: -25, blue: -40, purple: -10, magenta: 0 } },
  },
];

/** A framed answer: one 2-byte "JPEG" per variant. */
const framed = (count: number) => {
  const out = new Uint8Array(count * 6);
  for (let i = 0; i < count; i++) out.set([2, 0, 0, 0, 0xff, 0xd8], i * 6);
  return out.buffer;
};

let tileCalls: { variants: Record<string, unknown>[]; generation: number }[] = [];

beforeEach(() => {
  tileCalls = [];
  resetBuiltinPresets();
  invoke.mockReset();
  invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
    switch (command) {
      case "list_luts":
        return luts;
      case "list_camera_profiles":
        return [studio];
      case "load_builtin_presets":
        return mixers;
      case "load_and_parse_lut":
        return { size: 33 };
      case "profile_thumbnails": {
        const variants = args.variants as Record<string, unknown>[];
        tileCalls.push({ variants, generation: args.generation as number });
        return framed(variants.length);
      }
      default:
        return undefined;
    }
  });
  let n = 0;
  URL.createObjectURL = vi.fn(() => `blob:tile-${n++}`);
  URL.revokeObjectURL = vi.fn();
  const adjustments = { ...INITIAL_ADJUSTMENTS, exposure: 0.5 } as Adjustments;
  useEditorStore.setState({
    selectedImage: { path: "/s/Capture/a.CR3", isReady: true, isRaw: true } as never,
    adjustments,
    history: [adjustments],
    historyIndex: 0,
    previewOverride: null,
    isSliderDragging: false,
  });
  useSettingsStore.setState({ appSettings: {} as AppSettings });
});
afterEach(() => {
  cleanup();
  debouncedSave.cancel();
  debouncedSetHistory.cancel();
});

const openBrowser = async () => {
  render(<StudioProfileBrowser disabled={false} onInteraction={() => undefined} />);
  fireEvent.click(screen.getByRole("button", { name: "Browse profiles" }));
  const browser = await screen.findByRole("region", { name: "Profile browser" });
  await within(browser).findByRole("button", { name: "Studio flash" });
  await within(browser).findByRole("button", { name: "Darker skies" });
  return browser;
};
const tile = (browser: HTMLElement, name: string) => within(browser).getByRole("button", { name });

describe("profile browser", () => {
  it("groups camera profiles, looks, black-and-white mixes and imported packs, each tile a thumbnail of the photo", async () => {
    const browser = await openBrowser();
    const groups = within(browser)
      .getAllByRole("region")
      .map((section) => section.getAttribute("aria-label"));
    expect(groups).toEqual(["Camera", "Looks", "Black & white", "Moody pack"]);
    expect(within(within(browser).getByRole("region", { name: "Looks" })).getAllByRole("button", { pressed: false }).map((b) => b.title))
      .toContain("Warm");
    // The chosen ones are marked.
    expect(tile(browser, "Rawtonic").getAttribute("aria-pressed")).toBe("true");
    expect(tile(browser, "None").getAttribute("aria-pressed")).toBe("true");
    expect(tile(browser, "Colour").getAttribute("aria-pressed")).toBe("true");
    // Rendered natively from the open photo, with each tile's keys only.
    await waitFor(() => expect(browser.querySelectorAll("img")).toHaveLength(7));
    const variants = tileCalls.flatMap((call) => call.variants);
    expect(variants).toContainEqual({ cameraProfile: "profiles/Studio.dcp" });
    expect(variants).toContainEqual({ lutPath: "/res/Warm 200.cube", lutIntensity: 100, lutIsSceneReferred: true });
    expect(variants).toContainEqual(expect.objectContaining({ blackAndWhite: true }));
    expect(invoke).toHaveBeenCalledWith("profile_thumbnails", expect.objectContaining({ path: "/s/Capture/a.CR3", adjustments: expect.objectContaining({ exposure: 0.5 }) }));
  });

  it("previews on hover and focus without a history step, and restores on leaving or Esc", async () => {
    const browser = await openBrowser();
    fireEvent.mouseEnter(tile(browser, "Warm"));
    expect(useEditorStore.getState().previewOverride).toMatchObject({ lutPath: "/res/Warm 200.cube", exposure: 0.5 });
    expect(useEditorStore.getState().adjustments.lutPath ?? null).toBeNull();
    fireEvent.mouseLeave(browser.querySelector(".studio-profile-groups")!);
    expect(useEditorStore.getState().previewOverride).toBeNull();

    tile(browser, "Studio flash").focus();
    expect(useEditorStore.getState().previewOverride).toMatchObject({ cameraProfile: "profiles/Studio.dcp" });
    fireEvent.keyDown(tile(browser, "Studio flash"), { key: "Escape" });
    expect(useEditorStore.getState().previewOverride).toBeNull();
    expect(useEditorStore.getState().history).toHaveLength(1);
    // A second Esc closes the browser.
    fireEvent.keyDown(tile(browser, "Studio flash"), { key: "Escape" });
    expect(screen.queryByRole("region", { name: "Profile browser" })).toBeNull();
  });

  it("applies a click as one history step", async () => {
    const browser = await openBrowser();
    fireEvent.mouseEnter(tile(browser, "Studio flash"));
    fireEvent.click(tile(browser, "Studio flash"));
    await act(async () => debouncedSetHistory.flush());
    const { adjustments, history, historyIndex, previewOverride } = useEditorStore.getState();
    expect(previewOverride).toBeNull();
    expect(adjustments.cameraProfile).toBe("profiles/Studio.dcp");
    expect(history).toHaveLength(2);
    expect(historyIndex).toBe(1);
    expect(tile(browser, "Studio flash").getAttribute("aria-pressed")).toBe("true");

    fireEvent.click(tile(browser, "Warm"));
    await waitFor(() => expect(useEditorStore.getState().adjustments.lutPath).toBe("/res/Warm 200.cube"));
    await act(async () => debouncedSetHistory.flush());
    expect(useEditorStore.getState().history).toHaveLength(3);

    fireEvent.click(tile(browser, "Darker skies"));
    await act(async () => debouncedSetHistory.flush());
    expect(useEditorStore.getState().adjustments).toMatchObject({ blackAndWhite: true, grayMixer: { blue: -40 } });
    expect(useEditorStore.getState().history).toHaveLength(4);
    fireEvent.click(tile(browser, "Colour"));
    await act(async () => debouncedSetHistory.flush());
    expect(useEditorStore.getState().adjustments.blackAndWhite).toBe(false);
    expect(useEditorStore.getState().history).toHaveLength(5);
  });

  it("keeps favourites in the settings and lists them first", async () => {
    const browser = await openBrowser();
    fireEvent.click(within(browser).getByRole("button", { name: "Add Warm to favourites" }));
    expect(invoke).toHaveBeenCalledWith("save_settings", { settings: { favoriteProfiles: ["look:builtin:Warm 200"] } });
    const groups = within(browser).getAllByRole("region");
    expect(groups[0].getAttribute("aria-label")).toBe("Favourites");
    expect(within(groups[0]).getByRole("button", { name: "Warm" })).toBeTruthy();
    const star = within(groups[0]).getByRole("button", { name: "Remove Warm from favourites" });
    expect(star.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(star);
    expect(useSettingsStore.getState().appSettings?.favoriteProfiles).toEqual([]);
    expect(within(browser).getAllByRole("region")[0].getAttribute("aria-label")).toBe("Camera");
  });

  it("has an Amount for looks only, kept in the recipe's lutIntensity", async () => {
    render(<StudioProfileBrowser disabled={false} onInteraction={() => undefined} />);
    expect(screen.queryByRole("slider", { name: "Look amount" })).toBeNull();
    act(() =>
      useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, lutPath: "/res/Warm 200.cube", lutIntensity: 100 } })),
    );
    const amount = screen.getByRole("slider", { name: "Look amount" }) as HTMLInputElement;
    expect(amount.max).toBe("200");
    fireEvent.change(amount, { target: { value: "150" } });
    await act(async () => debouncedSetHistory.flush());
    expect(useEditorStore.getState().adjustments.lutIntensity).toBe(150);
  });

  it("searches by name and moves with the arrow keys", async () => {
    const browser = await openBrowser();
    expect(document.activeElement).toBe(within(browser).getByRole("searchbox", { name: "Search profiles" }));
    const outside = vi.fn();
    window.addEventListener("keydown", outside);
    tile(browser, "Rawtonic").focus();
    fireEvent.keyDown(tile(browser, "Rawtonic"), { key: "ArrowRight" });
    // Not also the photo navigation's arrow.
    expect(outside).not.toHaveBeenCalled();
    window.removeEventListener("keydown", outside);
    expect(document.activeElement).toBe(tile(browser, "Studio flash"));
    expect(useEditorStore.getState().previewOverride).toMatchObject({ cameraProfile: "profiles/Studio.dcp" });
    const search = within(browser).getByRole("searchbox", { name: "Search profiles" });
    search.focus();
    tile(browser, "Rawtonic").blur();
    fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(document.activeElement).toBe(tile(browser, "Rawtonic"));
    fireEvent.change(search, { target: { value: "SKIES" } });
    expect(within(browser).getAllByRole("button", { name: "Darker skies" })).toHaveLength(1);
    expect(within(browser).queryByRole("button", { name: "Warm" })).toBeNull();
  });

  it("cancels the tiles still rendering when it closes", async () => {
    let finish: (value: ArrayBuffer) => void = () => undefined;
    invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
      if (command === "profile_thumbnails") {
        tileCalls.push({ variants: args.variants as Record<string, unknown>[], generation: args.generation as number });
        return new Promise<ArrayBuffer>((resolve) => (finish = resolve));
      }
      if (command === "list_camera_profiles") return [studio];
      if (command === "list_luts") return luts;
      if (command === "load_builtin_presets") return mixers;
      return undefined;
    });
    await openBrowser();
    await waitFor(() => expect(tileCalls.length).toBeGreaterThan(0));
    fireEvent.click(screen.getByRole("button", { name: "Browse profiles" }));
    expect(invoke).toHaveBeenCalledWith("cancel_profile_thumbnails", { generation: tileCalls.at(-1)!.generation });
    finish(framed(8));
  });

  it("imports a DCP into the session and selects it", async () => {
    open.mockResolvedValue("/Downloads/Studio2.dcp");
    invoke.mockImplementation(async (command: string) => {
      if (command === "import_camera_profile") return { value: "profiles/Studio2.dcp", name: "Studio 2", camera: "Canon EOS R5" };
      if (command === "list_camera_profiles") return [studio];
      if (command === "list_luts") return luts;
      if (command === "load_builtin_presets") return mixers;
      return command === "profile_thumbnails" ? new ArrayBuffer(0) : undefined;
    });
    const browser = await openBrowser();
    fireEvent.click(within(browser).getByRole("button", { name: "Import DCP…" }));
    await within(browser).findByRole("button", { name: "Studio 2" });
    expect(invoke).toHaveBeenCalledWith("import_camera_profile", { source: "/Downloads/Studio2.dcp", imagePath: "/s/Capture/a.CR3" });
    expect(useEditorStore.getState().adjustments.cameraProfile).toBe("profiles/Studio2.dcp");
    expect(screen.getByText("For Canon EOS R5")).toBeTruthy();
  });

  it("shows only looks and mixes for a non-RAW photo", async () => {
    useEditorStore.setState({ selectedImage: { path: "/s/a.jpg", isReady: true, isRaw: false } as never });
    render(<StudioProfileBrowser disabled={false} onInteraction={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "Browse profiles" }));
    const browser = await screen.findByRole("region", { name: "Profile browser" });
    await within(browser).findByRole("button", { name: "Darker skies" });
    expect(within(browser).queryByRole("region", { name: "Camera" })).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith("list_camera_profiles", expect.anything());
  });
});

describe("helpers", () => {
  it("splits framed tiles and drops empty ones", () => {
    const bytes = new Uint8Array([1, 0, 0, 0, 7, 0, 0, 0, 0]).buffer;
    const tiles = splitTiles(bytes, 2);
    expect(tiles[0]?.size).toBe(1);
    expect(tiles[1]).toBeNull();
  });
  it("removes the camera profile for the Rawtonic tile", () => {
    const base = { ...INITIAL_ADJUSTMENTS, cameraProfile: "profiles/x.dcp" } as Adjustments;
    expect("cameraProfile" in withVariant(base, { cameraProfile: null })).toBe(false);
  });
});
