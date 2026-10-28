import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import {
  SmartAlbumEditorHost,
  formatExposure,
  incomplete,
  parseExposure,
  useSmartAlbumEditor,
} from "./SmartAlbumEditor";
import { useLibraryStore } from "../../store/useLibraryStore";
import { SESSION_ALBUMS_CHANGED } from "./StudioAlbums";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn() } }));
const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  vi.useFakeTimers();
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command) => {
    if (command === "session_albums") return [{ id: "p", name: "Picks", parent: null, count: 2, kind: "album" }];
    if (command === "session_search") return ["/job/a.arw", "/job/b.arw"];
    return undefined;
  });
  useLibraryStore.setState({ sessionDirectory: "/job", currentFolderPath: "/job/Capture" });
});
afterEach(() => {
  cleanup();
  act(() => useSmartAlbumEditor.getState().close());
  vi.useRealTimers();
});

const settle = () =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(200);
  });

describe("smart album editor", () => {
  it("shows a project-relative folder while retaining its full path for editing and saving", async () => {
    render(<SmartAlbumEditorHost />);
    act(() => useSmartAlbumEditor.getState().open({ name: "Folder", parent: null, rules: { combine: "all", rules: [{ field: "folder", op: "is", value: "/job/Capture/Portraits" }] } }));
    await settle();
    const input = within(screen.getByRole("group", { name: "Folder" })).getByLabelText("Value") as HTMLInputElement;
    expect(input.value).toBe("Capture/Portraits");
    expect(input.title).toBe("/job/Capture/Portraits");
    fireEvent.focus(input);
    expect(input.value).toBe("/job/Capture/Portraits");
    fireEvent.blur(input);
    expect(input.value).toBe("Capture/Portraits");
    await act(async () => fireEvent.click(screen.getByText("Create")));
    expect(mockInvoke).toHaveBeenCalledWith("session_create_smart_album", expect.objectContaining({ rules: { combine: "all", rules: [{ field: "folder", op: "is", value: "/job/Capture/Portraits" }] } }));
  });

  it("builds nested rules, shows what they match and creates the album", async () => {
    render(<SmartAlbumEditorHost />);
    act(() =>
      useSmartAlbumEditor.getState().open({
        name: "Smart album",
        parent: "g",
        rules: { combine: "all", rules: [{ field: "rating", op: "ge", value: 3 }] },
      }),
    );
    await settle();
    const dialog = screen.getByRole("dialog", { name: "New smart album" });
    expect(within(dialog).getByRole("status").textContent).toBe("Matches 2 photographs now");

    // The first rule becomes "Camera contains …"; a group of any two rules follows.
    const rule = within(dialog).getByRole("group", { name: "Rating" });
    fireEvent.change(within(rule).getByLabelText("Field"), { target: { value: "camera" } });
    const camera = within(dialog).getByRole("group", { name: "Camera" });
    expect((within(camera).getByLabelText("Comparison") as HTMLSelectElement).value).toBe("contains");
    fireEvent.change(within(camera).getByLabelText("Value"), { target: { value: "7RM4" } });
    fireEvent.click(within(dialog).getByText("Group of rules"));
    const nested = within(dialog).getAllByLabelText("How the rules combine")[1];
    expect((nested as HTMLSelectElement).value).toBe("any");
    fireEvent.change(nested, { target: { value: "none" } });
    fireEvent.change(within(dialog).getByLabelText("Name"), { target: { value: "Sharp portraits" } });
    await settle();
    await act(async () => {
      fireEvent.click(within(dialog).getByText("Create"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("session_create_smart_album", {
      directory: "/job",
      name: "Sharp portraits",
      parent: "g",
      rules: {
        combine: "all",
        rules: [
          { field: "camera", op: "contains", value: "7RM4" },
          { combine: "none", rules: [{ field: "rating", op: "ge", value: 3 }] },
        ],
      },
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("edits an existing smart album's rules and name", async () => {
    const changed = vi.fn();
    window.addEventListener(SESSION_ALBUMS_CHANGED, changed);
    render(<SmartAlbumEditorHost />);
    act(() =>
      useSmartAlbumEditor.getState().open({
        id: "s",
        name: "Recent",
        parent: null,
        rules: { combine: "all", rules: [{ field: "captureDate", op: "inLast", value: { amount: 30, unit: "days" } }] },
      }),
    );
    await settle();
    const dialog = screen.getByRole("dialog", { name: "Edit smart album" });
    fireEvent.change(within(dialog).getByLabelText("Amount"), { target: { value: "2" } });
    fireEvent.change(within(dialog).getByLabelText("Unit"), { target: { value: "weeks" } });
    fireEvent.change(within(dialog).getByLabelText("Name"), { target: { value: "Last two weeks" } });
    await settle();
    await act(async () => {
      fireEvent.click(within(dialog).getByText("Save"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("session_set_smart_album_rules", {
      directory: "/job",
      id: "s",
      rules: { combine: "all", rules: [{ field: "captureDate", op: "inLast", value: { amount: 2, unit: "weeks" } }] },
    });
    expect(mockInvoke).toHaveBeenCalledWith("session_rename_album", { directory: "/job", id: "s", name: "Last two weeks" });
    expect(changed).toHaveBeenCalled();
    window.removeEventListener(SESSION_ALBUMS_CHANGED, changed);
  });

  it("offers choices, albums and nothing to type where a rule needs none", async () => {
    render(<SmartAlbumEditorHost />);
    act(() =>
      useSmartAlbumEditor.getState().open({
        name: "x",
        parent: null,
        rules: {
          combine: "any",
          rules: [
            { field: "label", op: "is", value: ["red"] },
            { field: "album", op: "is", value: "p" },
            { field: "keyword", op: "empty" },
          ],
        },
      }),
    );
    await settle();
    const label = screen.getByRole("group", { name: "Color label" });
    fireEvent.click(within(label).getByLabelText("Red"));
    // No label chosen: the rule is incomplete and cannot be saved.
    expect((screen.getByText("Create") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(within(label).getByLabelText("No label"));
    expect((screen.getByText("Create") as HTMLButtonElement).disabled).toBe(false);
    const album = screen.getByRole("group", { name: "Album" });
    expect(within(album).getByRole("option", { name: "Picks" })).toBeTruthy();
    expect(within(screen.getByRole("group", { name: "Keyword" })).queryByLabelText("Value")).toBeNull();
  });

  it("shows why the catalog refuses rules", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "session_search") throw "Ratings are 0–5";
      return [];
    });
    render(<SmartAlbumEditorHost />);
    act(() =>
      useSmartAlbumEditor.getState().open({
        name: "x",
        parent: null,
        rules: { combine: "all", rules: [{ field: "rating", op: "ge", value: 9 }] },
      }),
    );
    await settle();
    expect(screen.getByRole("status").textContent).toBe("Ratings are 0–5");
    expect((screen.getByText("Create") as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("rule values", () => {
  it("reads and writes shutter speeds as photographers write them", () => {
    expect(parseExposure("1/250")).toBeCloseTo(0.004);
    expect(parseExposure("2s")).toBe(2);
    expect(parseExposure("0.5")).toBe(0.5);
    expect(Number.isNaN(parseExposure("fast"))).toBe(true);
    for (const malformed of ["1/250/2", "/250", "1/", ""]) expect(Number.isNaN(parseExposure(malformed))).toBe(true);
    expect(formatExposure(0.004)).toBe("1/250");
    expect(formatExposure(2)).toBe("2");
    // Round trip: what is shown is what is saved.
    for (const seconds of [0.625, 0.35, 1 / 3, 0.004, 1 / 8000, 30, 2.5]) {
      expect(parseExposure(formatExposure(seconds))).toBe(seconds);
    }
  });

  it("finds incomplete rules", () => {
    expect(incomplete({ field: "flag", op: "is", value: [] })).toBe(true);
    // What the filter bar saves: one value, not a list.
    expect(incomplete({ field: "flag", op: "is", value: "picked" })).toBe(false);
    expect(incomplete({ field: "fileType", op: "is", value: "jpeg" })).toBe(false);
    expect(incomplete({ field: "fileType", op: "is", value: "" })).toBe(true);
    expect(incomplete({ field: "album", op: "is", value: "" })).toBe(true);
    expect(incomplete({ field: "captureDate", op: "between", value: ["2026-01-01", ""] })).toBe(true);
    expect(incomplete({ combine: "all", rules: [{ field: "rating", op: "ge", value: 1 }] })).toBe(false);
    expect(incomplete({ field: "iso", op: "ge", value: NaN })).toBe(true);
    expect(incomplete({ field: "iso", op: "between", value: [100, NaN] })).toBe(true);
    expect(incomplete({ field: "iso", op: "empty" })).toBe(false);
  });
});

describe("rule rows", () => {
  it("shows each remaining rule's own number after a rule above it is removed", async () => {
    render(<SmartAlbumEditorHost />);
    act(() =>
      useSmartAlbumEditor.getState().open({
        name: "x",
        parent: null,
        rules: {
          combine: "any",
          rules: [
            { field: "rating", op: "ge", value: 3 },
            { field: "rating", op: "ge", value: 5 },
          ],
        },
      }),
    );
    await settle();
    const [first] = screen.getAllByRole("group", { name: "Rating" });
    fireEvent.click(within(first).getByLabelText("Remove the Rating rule"));
    const [left] = screen.getAllByRole("group", { name: "Rating" });
    expect((within(left).getByLabelText("Value") as HTMLInputElement).value).toBe("5");
    // Typing a fraction keeps what is typed while it is incomplete.
    fireEvent.change(within(left).getByLabelText("Field"), { target: { value: "exposure" } });
    const exposure = screen.getByRole("group", { name: "Shutter speed (s)" });
    const input = within(exposure).getByLabelText("Value") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "1/" } });
    expect(input.value).toBe("1/");
    // Not a number yet: incomplete, nothing different from it can be saved.
    expect((screen.getByText("Create") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(input, { target: { value: "abc" } });
    expect(input.value).toBe("abc");
    expect((screen.getByText("Create") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(input, { target: { value: "1/500" } });
    expect(input.value).toBe("1/500");
    await settle();
    expect((screen.getByText("Create") as HTMLButtonElement).disabled).toBe(false);
  });
});

describe("range rows", () => {
  it("shows a range afresh when its field changes", async () => {
    render(<SmartAlbumEditorHost />);
    act(() =>
      useSmartAlbumEditor.getState().open({
        name: "x",
        parent: null,
        rules: { combine: "all", rules: [{ field: "exposure", op: "between", value: [0.004, 0.004] }] },
      }),
    );
    await settle();
    const row = screen.getByRole("group", { name: "Shutter speed (s)" });
    expect((within(row).getAllByLabelText("Value")[0] as HTMLInputElement).value).toBe("1/250");
    fireEvent.change(within(row).getByLabelText("Field"), { target: { value: "iso" } });
    const iso = screen.getByRole("group", { name: "ISO" });
    const shown = within(iso).getAllByLabelText("Value").map((input) => (input as HTMLInputElement).value);
    expect(shown.every((value) => value !== "1/250")).toBe(true);
  });
});

it("saves localized AI and provenance choices using catalog codes", async () => {
  render(<SmartAlbumEditorHost />);
  act(() => useSmartAlbumEditor.getState().open({
    name: "AI work", parent: null,
    rules: { combine: "all", rules: [
      { field: "ai", op: "is", value: "generated" },
      { field: "sourceType", op: "is", value: "trainedAlgorithmicMedia" },
    ] },
  }));
  await settle();
  const ai = within(screen.getByRole("group", { name: "AI edit" }));
  expect((ai.getByLabelText("Generated areas") as HTMLInputElement).checked).toBe(true);
  fireEvent.click(ai.getByLabelText("AI selection not detected yet"));
  const source = within(screen.getByRole("group", { name: "Digital source type (in the file)" }));
  expect((source.getByLabelText("Generated by AI") as HTMLInputElement).checked).toBe(true);
  fireEvent.click(source.getByLabelText("Not stated"));
  await settle();
  await act(async () => { fireEvent.click(screen.getByText("Create")); });
  expect(mockInvoke).toHaveBeenCalledWith("session_create_smart_album", {
    directory: "/job", name: "AI work", parent: null,
    rules: { combine: "all", rules: [
      { field: "ai", op: "is", value: ["generated", "pendingDetection"] },
      { field: "sourceType", op: "is", value: ["trainedAlgorithmicMedia", "none"] },
    ] },
  });
});

it("shows an unfamiliar source type selected from the Library so it can be removed", async () => {
  render(<SmartAlbumEditorHost />);
  act(() => useSmartAlbumEditor.getState().open({
    name: "Imported source", parent: null,
    rules: { combine: "all", rules: [{ field: "sourceType", op: "is", value: "custom-source" }] },
  }));
  await settle();
  const choice = screen.getByLabelText("custom-source") as HTMLInputElement;
  expect(choice.checked).toBe(true);
  fireEvent.click(choice);
  expect((screen.getByText("Create") as HTMLButtonElement).disabled).toBe(true);
});
