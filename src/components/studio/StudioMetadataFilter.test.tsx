import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import StudioMetadataFilter, { facetLabel } from "./StudioMetadataFilter";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { LibraryViewMode, RawStatus } from "../ui/AppProperties";
import { catalogChanged } from "../../utils/catalogWrites";
import i18n from "i18next";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  vi.useFakeTimers();
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue({
    camera: [
      { value: "ILCE-7RM4", count: 120 },
      { value: "X-T4", count: 30 },
      { value: "", count: 2 },
    ],
    captureDate: [{ value: "2026-10", count: 152 }],
    fileType: [{ value: "raw", count: 150 }, { value: "jpeg", count: 2 }],
    aperture: [{ value: "2.8", count: 40 }],
  });
  useLibraryStore.setState({
    sessionDirectory: "/job",
    currentFolderPath: "/job/Capture",
    activeAlbumId: null,
    searchCriteria: { text: "bride", tags: [], mode: "OR" },
    filterCriteria: { colors: [], rating: 4, rawStatus: RawStatus.All },
  });
  useSettingsStore.setState({ appSettings: { libraryViewMode: LibraryViewMode.Recursive } as never });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const counted = async () => {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(200);
  });
};

it("lists each field's values with counts within the shown folder, search and culling filters", async () => {
  render(<StudioMetadataFilter />);
  await counted();
  const [command, args] = mockInvoke.mock.calls[0] as [string, { directory: string; requests: { field: string; rule: unknown }[] }];
  expect(command).toBe("session_facets");
  expect(args.directory).toBe("/job");
  const camera = args.requests.find((r) => r.field === "camera");
  expect(camera?.rule).toEqual({
    combine: "all",
    rules: [
      { field: "folder", op: "contains", value: "/job/Capture" },
      { field: "text", op: "contains", value: "bride" },
      { field: "rating", op: "ge", value: 4 },
    ],
  });
  const column = screen.getByRole("group", { name: "Camera" });
  expect(within(column).getByText("ILCE-7RM4")).toBeTruthy();
  expect(within(column).getByText("120")).toBeTruthy();
  expect(within(column).getByText("Unknown camera")).toBeTruthy();
  expect(within(screen.getByRole("group", { name: "Aperture" })).getByText("f/2.8")).toBeTruthy();
  expect(within(screen.getByRole("group", { name: "File type" })).getByText("JPEG")).toBeTruthy();
});

it("chooses values, counts each list without its own choice and follows catalog writes", async () => {
  render(<StudioMetadataFilter />);
  await counted();
  fireEvent.click(within(screen.getByRole("group", { name: "Camera" })).getByText("X-T4"));
  expect(useLibraryStore.getState().filterCriteria.metadata).toEqual({ camera: ["X-T4"] });
  await counted();
  const requests = (mockInvoke.mock.calls.at(-1)?.[1] as { requests: { field: string; rule: { rules: unknown[] } }[] })
    .requests;
  // The camera list leaves out the chosen camera; the others include it.
  expect(JSON.stringify(requests.find((r) => r.field === "camera")?.rule)).not.toContain("X-T4");
  expect(JSON.stringify(requests.find((r) => r.field === "lens")?.rule)).toContain("X-T4");
  const calls = mockInvoke.mock.calls.length;
  act(() => catalogChanged());
  await counted();
  expect(mockInvoke.mock.calls.length).toBe(calls + 1);
  // "All" takes the choice back.
  fireEvent.click(within(screen.getByRole("group", { name: "Camera" })).getAllByRole("button")[0]);
  expect(useLibraryStore.getState().filterCriteria.metadata).toEqual({ camera: [] });
});

it("shows nothing outside a library", () => {
  useLibraryStore.setState({ sessionDirectory: null });
  const { container } = render(<StudioMetadataFilter />);
  expect(container.innerHTML).toBe("");
});

it("names months and photographic values", () => {
  const t = i18n.t.bind(i18n);
  expect(facetLabel("captureDate", "2026-02", t, "en")).toBe("February 2026");
  expect(facetLabel("iso", "3200", t)).toBe("ISO 3200");
  expect(facetLabel("focalLength", "85", t)).toBe("85 mm");
  expect(facetLabel("keyword", "", t)).toBe("No keyword");
});

it("counts within the RAW / not RAW filter too", async () => {
  useLibraryStore.setState({
    filterCriteria: { colors: [], rating: 0, rawStatus: RawStatus.RawOnly },
    searchCriteria: { text: "", tags: [], mode: "OR" },
  });
  render(<StudioMetadataFilter />);
  await counted();
  const args = mockInvoke.mock.calls.at(-1)?.[1] as { requests: { field: string; rule: unknown }[] };
  expect(args.requests.find((r) => r.field === "camera")?.rule).toEqual({
    combine: "all",
    rules: [
      { field: "folder", op: "contains", value: "/job/Capture" },
      { field: "fileType", op: "is", value: ["raw", "dng"] },
    ],
  });
});
