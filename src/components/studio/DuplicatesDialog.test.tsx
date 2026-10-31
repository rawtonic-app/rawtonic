import "../../i18n";
import i18n from "i18next";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { DuplicatesDialogHost } from "./DuplicatesDialog";
import { useDuplicatesDialog, type DuplicateMember } from "../../utils/duplicates";
import { useLibraryStore } from "../../store/useLibraryStore";
import { appCommandEnabled, runAppCommand } from "../../utils/appCommands";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const LIBRARY = "/jobs/Wedding";
const member = (path: string, part: Partial<DuplicateMember> = {}): DuplicateMember => ({
  id: path,
  path: `${LIBRARY}/${path}`,
  size: 25_000_000,
  captured: "2026-10-30 12:00:00",
  imported: "2026-10-30 18:00:00",
  rating: 0,
  flag: 0,
  label: "",
  keywords: 0,
  albums: 0,
  copies: 0,
  described: false,
  companion: null,
  compared: "bmff",
  owner: null,
  ownFolder: true,
  edited: false,
  suggested: false,
  ...part,
});
// Oldest import first, as the backend lists them, with its suggestion.
let groups: DuplicateMember[][] = [];
const defaultGroups = () => [
  [
    member("Capture/A.CR3", { suggested: true }),
    member("Capture/A copy.CR3", { suggested: true }),
    member("Selects/A.CR3", { rating: 4, keywords: 2, albums: 1 }),
  ],
  [member("Capture/B.NEF", { compared: "tiff" }), member("Other/B.NEF", { compared: "tiff", suggested: true })],
];

const handlers = new Map<string, (event: { payload: unknown }) => void>();
let starts = 0;
let refusedNext: { path: string; reason: string }[] = [];
const trash = vi.fn(async () => {});
const calls = (name: string) => vi.mocked(invoke).mock.calls.filter(([command]) => command === name);

const finish = (run: number) =>
  act(() =>
    handlers.get("duplicates-finished")!({
      payload: { run, directory: LIBRARY, photos: 5, hashed: 5, unreadable: [], cancelled: false, error: null, seconds: 1 },
    }),
  );

beforeEach(async () => {
  starts = 0;
  groups = defaultGroups();
  refusedNext = [];
  handlers.clear();
  trash.mockClear();
  await i18n.changeLanguage("en");
  useLibraryStore.setState({ sessionDirectory: LIBRARY });
  vi.mocked(listen).mockImplementation(async (event, handler) => {
    handlers.set(event, handler as (event: { payload: unknown }) => void);
    return () => handlers.delete(event);
  });
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === "duplicates_start") return ++starts;
    if (name === "duplicates_groups") return groups;
    if (name === "duplicates_confirm") return refusedNext;
    return undefined;
  });
});

afterEach(() => {
  act(() => useDuplicatesDialog.getState().close());
  cleanup();
  vi.clearAllMocks();
});

async function openReview() {
  render(<DuplicatesDialogHost trash={trash} />);
  act(() => {
    runAppCommand("find_duplicates");
  });
  await waitFor(() => expect(calls("duplicates_start")).toHaveLength(1));
  expect(calls("duplicates_start")[0][1]).toEqual({ directory: LIBRARY });
  await act(async () => {});
  act(() =>
    handlers.get("duplicates-progress")!({ payload: { run: 1, directory: LIBRARY, done: 2, total: 5 } }),
  );
  expect(screen.getByText("Comparing image data: 2 of 5 photos")).toBeTruthy();
  await finish(1);
  await screen.findByText("2 groups of duplicates (5 photos)");
}

const rowOf = (name: string, folder: string) =>
  screen.getAllByRole("listitem").find((li) => li.querySelectorAll("input").length === 1 && li.textContent?.includes(name) && li.textContent.includes(folder))!;
const box = (name: string, folder: string) => within(rowOf(name, folder)).getByRole("checkbox") as HTMLInputElement;

it("reads, then lists the groups with the backend's suggestion pre-marked", async () => {
  await openReview();
  expect(screen.getAllByText("3 photos with identical image data · image data compared")).toHaveLength(1);
  // A: the rated copy with keywords stays, the two plain ones go.
  expect(box("A.CR3", "/Selects").checked).toBe(false);
  expect(box("A.CR3", "/Capture").checked).toBe(true);
  expect(box("A copy.CR3", "/Capture").checked).toBe(true);
  expect(within(rowOf("A.CR3", "/Selects")).getByText(/★★★★ · 2 keywords · 1 album/)).toBeTruthy();
  // B: nothing of its own, so the oldest import stays.
  expect(box("B.NEF", "/Capture").checked).toBe(false);
  expect(box("B.NEF", "/Other").checked).toBe(true);
  expect(screen.getByRole("button", { name: "Move 3 Photos to Trash" })).toBeTruthy();
});

it("trashes exactly the marked photos after checking the files again, as one step", async () => {
  await openReview();
  // The user keeps the plain copy of A after all.
  fireEvent.click(box("A copy.CR3", "/Capture"));
  fireEvent.click(screen.getByRole("button", { name: "Move 2 Photos to Trash" }));
  await waitFor(() => expect(trash).toHaveBeenCalledTimes(1));
  const marked = [`${LIBRARY}/Capture/A.CR3`, `${LIBRARY}/Other/B.NEF`];
  expect(calls("duplicates_confirm")[0][1]).toEqual({
    directory: LIBRARY,
    keep: [`${LIBRARY}/Capture/A copy.CR3`, `${LIBRARY}/Selects/A.CR3`, `${LIBRARY}/Capture/B.NEF`],
    trash: marked,
  });
  expect(trash).toHaveBeenCalledWith(marked, { includeAssociated: false });
  expect(useDuplicatesDialog.getState().open).toBe(false);
});

it("greys another project's photo, which cannot be marked, and names that project", async () => {
  groups = [
    [
      member("Photos/C.jpg", { suggested: false, edited: true }),
      member("Capture/C.jpg", {
        path: "/jobs/Smith/Capture/C.jpg",
        ownFolder: false,
        rating: 5,
        owner: { directory: "/jobs/Smith", name: "Wedding Smith" },
        // Even if a suggestion came marked, the owned photo stays unmarked.
        suggested: true,
      }),
      member("Photos/C copy.jpg", { suggested: true }),
    ],
  ];
  render(<DuplicatesDialogHost trash={trash} />);
  act(() => useDuplicatesDialog.getState().show());
  await waitFor(() => expect(calls("duplicates_start")).toHaveLength(1));
  await act(async () => {});
  await finish(1);
  await screen.findByText("1 group of duplicates (3 photos)");
  const row = rowOf("C.jpg", "/jobs/Smith/Capture");
  expect(row.className).toBe("is-owned");
  expect(within(row).getByText("Belongs to Wedding Smith: remove it there.")).toBeTruthy();
  const owned = within(row).getByRole("checkbox") as HTMLInputElement;
  expect(owned.disabled).toBe(true);
  expect(owned.checked).toBe(false);
  fireEvent.click(owned);
  expect(owned.checked).toBe(false);
  // The library's own copies may all go: the project's photo stays.
  fireEvent.click(box("C.jpg", "/jobs/Wedding/Photos"));
  expect(screen.queryByText("Keep at least one photo of this group.")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Move 2 Photos to Trash" }));
  await waitFor(() => expect(trash).toHaveBeenCalledTimes(1));
  expect(calls("duplicates_confirm")[0][1]).toEqual({
    directory: LIBRARY,
    keep: ["/jobs/Smith/Capture/C.jpg"],
    trash: [`${LIBRARY}/Photos/C.jpg`, `${LIBRARY}/Photos/C copy.jpg`],
  });
});

it("refuses to trash a whole group", async () => {
  await openReview();
  fireEvent.click(box("B.NEF", "/Capture"));
  expect(screen.getByRole("alert").textContent).toBe("Keep at least one photo of this group.");
  expect((screen.getByRole("button", { name: "Move 4 Photos to Trash" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Move 4 Photos to Trash" }));
  expect(trash).not.toHaveBeenCalled();
});

it("trashes nothing when a file changed since it was compared, and compares again", async () => {
  await openReview();
  refusedNext = [{ path: `${LIBRARY}/Other/B.NEF`, reason: "changed" }];
  fireEvent.click(screen.getByRole("button", { name: "Move 3 Photos to Trash" }));
  await screen.findByText("1 photo changed since it was compared, so nothing was moved. Compared again.");
  expect(trash).not.toHaveBeenCalled();
  await waitFor(() => expect(calls("duplicates_start")).toHaveLength(2));
});

it("keeps a photo whose paired JPEG has no kept copy, without comparing again", async () => {
  await openReview();
  refusedNext = [{ path: `${LIBRARY}/Capture/A.CR3`, reason: "companion" }];
  fireEvent.click(screen.getByRole("button", { name: "Move 3 Photos to Trash" }));
  await screen.findByText(
    "1 photo stays: its paired JPEG has no identical copy that stays, so nothing was moved.",
  );
  expect(trash).not.toHaveBeenCalled();
  expect(calls("duplicates_start")).toHaveLength(1);
});

it("says when nothing is duplicated, and Stop stops the reading", async () => {
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === "duplicates_start") return 7;
    if (name === "duplicates_groups") return [];
    return undefined;
  });
  render(<DuplicatesDialogHost trash={trash} />);
  act(() => useDuplicatesDialog.getState().show());
  fireEvent.click(await screen.findByRole("button", { name: "Stop" }));
  expect(calls("duplicates_cancel")[0][1]).toEqual({ run: 7 });
  await finish(7);
  await screen.findByText("No duplicates: no two photos hold identical image data.");
  expect(screen.queryByRole("button", { name: /to Trash/ })).toBeNull();
});

it("takes a pass that ended before its number came back", async () => {
  let release!: (run: number) => void;
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === "duplicates_start") return new Promise<number>((resolve) => (release = resolve));
    if (name === "duplicates_groups") return [];
    return undefined;
  });
  render(<DuplicatesDialogHost trash={trash} />);
  act(() => useDuplicatesDialog.getState().show());
  await waitFor(() => expect(calls("duplicates_start")).toHaveLength(1));
  await finish(3);
  await finish(9);
  await act(async () => release(9));
  await screen.findByText("No duplicates: no two photos hold identical image data.");
});

it("needs a library", async () => {
  useLibraryStore.setState({ sessionDirectory: null });
  render(<DuplicatesDialogHost trash={trash} />);
  expect(appCommandEnabled("find_duplicates")).toBe(false);
  act(() => useDuplicatesDialog.getState().show());
  expect(screen.getByText("Open a project or catalog to find duplicates.")).toBeTruthy();
  expect(calls("duplicates_start")).toHaveLength(0);
});
