import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import StudioFilters from "./StudioFilters";
import StudioBrowser from "./StudioBrowser";
import { useLibraryStore } from "../../store/useLibraryStore";
import { RawStatus, EditedStatus, type ImageFile } from "../ui/AppProperties";

beforeEach(() => {
  useLibraryStore.setState({
    sessionDirectory: "/job",
    imageFlags: {},
    filterCriteria: {
      flag: "all",
      rating: 0,
      colors: [],
      rawStatus: RawStatus.All,
    },
    searchCriteria: { text: "", tags: [], mode: "OR" },
  });
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("applies flag and color filters and clears every hidden constraint", async () => {
  const user = userEvent.setup();
  render(
    <StudioFilters counts={{ all: 3, picked: 1, unflagged: 1, rejected: 1 }} />,
  );
  const picked = screen.getByRole("button", { name: "Picked 1" });
  await user.click(picked);
  expect(picked.getAttribute("aria-pressed")).toBe("true");
  expect(useLibraryStore.getState().filterCriteria.flag).toBe("picked");
  await user.selectOptions(
    screen.getByLabelText("Filter by color label"),
    "red",
  );
  expect(useLibraryStore.getState().filterCriteria.colors).toEqual(["red"]);
  useLibraryStore
    .getState()
    .setSearchCriteria({ text: "portrait", tags: ["iso:100"] });
  await user.click(screen.getByRole("button", { name: "Clear filters" }));
  expect(useLibraryStore.getState().filterCriteria).toEqual({
    flag: "all",
    rating: 0,
    colors: [],
    rawStatus: RawStatus.All,
    editedStatus: EditedStatus.All,
  });
  expect(useLibraryStore.getState().searchCriteria).toEqual({
    text: "",
    tags: [],
    mode: "OR",
  });
});

it("shows persisted flags and color labels in both browser modes, including virtual copies", () => {
  const file: ImageFile = {
    path: "/job/photo.arw?vc=1",
    rating: 4,
    tags: ["color:blue"],
    modified: 0,
    is_edited: false,
    is_raw: true,
    is_virtual_copy: true,
    is_cloud_placeholder: false,
    exif: null,
    group_id: null,
  };
  useLibraryStore.setState({ imageFlags: { "/job/photo.arw": -1 } });
  const props = {
    files: [file],
    onSelect: vi.fn(),
    onOpen: vi.fn(),
    onRate: vi.fn(),
    onContextMenu: vi.fn(),
    requestThumbnails: vi.fn(),
  };
  const { rerender } = render(<StudioBrowser {...props} />);
  expect(screen.getByRole("img", { name: "Rejected" })).toBeTruthy();
  expect(screen.getByRole("img", { name: "Blue label" })).toBeTruthy();
  expect(screen.getByRole("option").className).toContain("is-rejected");
  useLibraryStore.setState({ imageFlags: { "/job/photo.arw": 1 } });
  rerender(<StudioBrowser {...props} film />);
  expect(screen.getByRole("img", { name: "Picked" })).toBeTruthy();
  expect(screen.queryByRole("img", { name: "Rejected" })).toBeNull();
});

it("names a virtual copy by its file and marks it, without the internal copy id", () => {
  const file = (path: string): ImageFile => ({
    path,
    rating: 0,
    tags: [],
    modified: 0,
    is_edited: false,
    is_raw: true,
    is_virtual_copy: path.includes("?vc="),
    is_cloud_placeholder: false,
    exif: null,
    group_id: null,
  });
  render(
    <StudioBrowser
      files={[file("/job/DSCF0135.RAF"), file("/job/DSCF0135.RAF?vc=9b8208")]}
      onSelect={vi.fn()}
      onOpen={vi.fn()}
      onRate={vi.fn()}
      onContextMenu={vi.fn()}
      requestThumbnails={vi.fn()}
    />,
  );
  const names = screen.getAllByRole("option").map((tile) => tile.getAttribute("aria-label"));
  expect(names).toEqual(["DSCF0135.RAF", "DSCF0135.RAF (virtual copy)"]);
  expect(screen.getByText("· Virtual copy", { exact: false })).toBeTruthy();
  expect(document.body.textContent).not.toContain("?vc=");
});

it("shows a virtual copy's file type without the internal copy id", () => {
  render(
    <StudioBrowser
      files={[{
        path: "/job/chart.png?vc=9b8208",
        rating: 0,
        tags: [],
        modified: 0,
        is_edited: false,
        is_raw: false,
        is_virtual_copy: true,
        is_cloud_placeholder: false,
        exif: null,
        group_id: null,
      }]}
      onSelect={vi.fn()}
      onOpen={vi.fn()}
      onRate={vi.fn()}
      onContextMenu={vi.fn()}
      requestThumbnails={vi.fn()}
    />,
  );
  expect(document.querySelector(".studio-photo-type")?.textContent).toBe("PNG");
});
