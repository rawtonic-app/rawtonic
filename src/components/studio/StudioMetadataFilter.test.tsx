import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import StudioMetadataFilter, { facetLabel, rangeOf } from "./StudioMetadataFilter";
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
  expect((args as { listed?: string }).listed).toBe("/job/Capture");
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

it("chooses a run of values with Shift-click (ISO 800 to 3200), not the photographs without one", async () => {
  mockInvoke.mockResolvedValue({
    iso: [
      { value: "400", count: 3 },
      { value: "800", count: 2 },
      { value: "1600", count: 4 },
      { value: "3200", count: 3 },
      { value: "6400", count: 1 },
      { value: "", count: 1 },
    ],
  });
  render(<StudioMetadataFilter />);
  await counted();
  const iso = within(screen.getByRole("group", { name: "ISO" }));
  fireEvent.click(iso.getByText("ISO 800"));
  fireEvent.click(iso.getByText("ISO 3200"), { shiftKey: true });
  expect(useLibraryStore.getState().filterCriteria.metadata).toEqual({ iso: ["800", "1600", "3200"] });
  // Back down from there: the run joins what is chosen.
  fireEvent.click(iso.getByText("Unknown ISO"), { shiftKey: true });
  expect(useLibraryStore.getState().filterCriteria.metadata?.iso).toEqual(["800", "1600", "3200", "6400", ""]);
});

it("lists capture dates by year, a year chosen in one click, its months on demand", async () => {
  mockInvoke.mockResolvedValue({
    captureDate: [
      { value: "2025-03", count: 2 },
      { value: "2024-11", count: 5 },
      { value: "2024-02", count: 4 },
      { value: "2023-07", count: 1 },
      { value: "", count: 3 },
    ],
  });
  render(<StudioMetadataFilter />);
  await counted();
  const dates = within(screen.getByRole("group", { name: "Capture date" }));
  // Years with the sum of their months; months hidden until asked for.
  expect(dates.getByText("2024").closest("button")!.textContent).toContain("9");
  expect(dates.queryByText("November 2024")).toBeNull();
  fireEvent.click(dates.getByText("2024"));
  expect(useLibraryStore.getState().filterCriteria.metadata).toEqual({ captureDate: ["2024"] });
  fireEvent.click(dates.getByLabelText("Show the months of 2024"));
  expect(dates.getByText("November 2024").closest("button")!.getAttribute("aria-pressed")).toBe("true");
  // A month of the chosen year taken back: the year's other months stay.
  fireEvent.click(dates.getByText("November 2024"));
  expect(useLibraryStore.getState().filterCriteria.metadata).toEqual({ captureDate: ["2024-02"] });
  // Shift-click over years chooses the years between.
  fireEvent.click(dates.getByText("2025"));
  fireEvent.click(dates.getByText("2023"), { shiftKey: true });
  expect(useLibraryStore.getState().filterCriteria.metadata).toEqual({ captureDate: ["2025", "2024", "2023"] });
});

it("takes \"no value\" as either end of a range of years or of months", () => {
  const month = (value: string) => ({ value, count: 1 });
  const rows = [
    { value: "2025", count: 1, months: [month("2025-03")] },
    month("2025-03"),
    { value: "2024", count: 1, months: [month("2024-05")] },
    { value: "2023", count: 1, months: [month("2023-01")] },
    month(""),
  ];
  expect(rangeOf(rows, "2025", "")).toEqual(["2025", "2024", "2023", ""]);
  expect(rangeOf(rows, "", "2024")).toEqual(["2024", "2023", ""]);
  expect(rangeOf(rows, "2025-03", "")).toEqual(["2025-03", ""]);
  expect(rangeOf(rows, "2025", "2025-03")).toBeNull();
});

it("keeps a chosen value no photograph has any more in its place, so a range still follows the order", async () => {
  mockInvoke.mockResolvedValue({ iso: [{ value: "800", count: 2 }, { value: "1600", count: 4 }] });
  render(<StudioMetadataFilter />);
  await counted();
  fireEvent.click(within(screen.getByRole("group", { name: "ISO" })).getByText("ISO 800"));
  // The lists change (another choice, a write) and no photograph has ISO
  // 800 any more: it stays listed at 0, between 400 and 1600.
  mockInvoke.mockResolvedValue({
    iso: ["400", "1600", "3200", "6400"].map((value) => ({ value, count: 1 })),
  });
  act(() => catalogChanged());
  await counted();
  const iso = within(screen.getByRole("group", { name: "ISO" }));
  expect(iso.getAllByText(/^ISO \d+$/).map((label) => label.textContent)).toEqual([
    "ISO 400",
    "ISO 800",
    "ISO 1600",
    "ISO 3200",
    "ISO 6400",
  ]);
  fireEvent.click(iso.getByText("ISO 3200"), { shiftKey: true });
  expect(useLibraryStore.getState().filterCriteria.metadata?.iso).toEqual(["800", "1600", "3200"]);
});

it("keeps a chosen year no photograph has any more among the years, a range across it", async () => {
  mockInvoke.mockResolvedValue({ captureDate: ["2024-05", "2023-02", "2022-01"].map((value) => ({ value, count: 1 })) });
  render(<StudioMetadataFilter />);
  await counted();
  fireEvent.click(within(screen.getByRole("group", { name: "Capture date" })).getByText("2023"));
  mockInvoke.mockResolvedValue({ captureDate: ["2024-05", "2022-01"].map((value) => ({ value, count: 1 })) });
  act(() => catalogChanged());
  await counted();
  const dates = within(screen.getByRole("group", { name: "Capture date" }));
  expect(dates.getAllByText(/^\d{4}$/).map((label) => label.textContent)).toEqual(["2024", "2023", "2022"]);
  fireEvent.click(dates.getByText("2022"), { shiftKey: true });
  expect(useLibraryStore.getState().filterCriteria.metadata?.captureDate).toEqual(["2023", "2022"]);
});

it("passes Trash folder context for counts, but never for an album", async () => {
  useLibraryStore.setState({ currentFolderPath: "/job/Trash" });
  const { rerender } = render(<StudioMetadataFilter />);
  await counted();
  expect(mockInvoke).toHaveBeenLastCalledWith("session_facets", expect.objectContaining({ listed: "/job/Trash" }));
  act(() => useLibraryStore.setState({ activeAlbumId: "session-album:p" }));
  rerender(<StudioMetadataFilter />);
  await counted();
  expect(mockInvoke).toHaveBeenLastCalledWith("session_facets", expect.objectContaining({ listed: null }));
});
