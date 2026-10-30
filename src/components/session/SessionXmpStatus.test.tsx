import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "react-toastify";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useUIStore } from "../../store/useUIStore";
import { undo, useCatalogUndoStore } from "../../utils/appUndo";
import SessionXmpStatus, {
  openPendingXmpView,
  PENDING_XMP_VIEW_ID,
  SHOW_PENDING_XMP_EVENT,
  type XmpReviewEntry,
  type XmpRetry,
} from "./SessionXmpStatus";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

afterEach(() => cleanup());

/** Answers each command with its own value (a function: called with the args). */
function answer(values: Record<string, unknown>) {
  vi.mocked(invoke).mockImplementation(async (command: string, args?: unknown) => {
    const value = values[command];
    return typeof value === "function" ? (value as (args: unknown) => unknown)(args) : value;
  });
}

const changed = (path: string): XmpReviewEntry => ({ path, problem: { kind: "changed_elsewhere" }, volume: null });

function reasonTexts() {
  const list = document.querySelector(".session-xmp-reasons") as HTMLElement | null;
  return list ? within(list).getAllByRole("listitem").map((item) => item.textContent) : [];
}
beforeEach(() => {
  vi.clearAllMocks();
  useLibraryStore.setState({ sessionDirectory: "/job" });
});

describe("SessionXmpStatus", () => {
  it("shows nothing when every XMP is up to date", () => {
    const { container } = render(<SessionXmpStatus directory="/job" count={0} onChanged={vi.fn()} />);
    expect(container.textContent).toBe("");
  });

  it("explains a real pending state and writes the sidecars again on request", async () => {
    const user = userEvent.setup();
    const onChanged = vi.fn();
    vi.mocked(invoke).mockResolvedValue({ written: 2, unsaved: [] });
    render(<SessionXmpStatus directory="/job" count={2} onChanged={onChanged} />);
    await user.click(screen.getByRole("button", { name: "2 XMP not updated" }));
    const panel = screen.getByRole("dialog", { name: "XMP files not updated" });
    expect(panel.textContent).toContain("The XMP files of 2 photos are not up to date");
    expect(panel.textContent).toContain("The photos themselves are never changed.");
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(invoke).toHaveBeenCalledWith("session_write_pending_xmp", { directory: "/job" });
    expect(onChanged).toHaveBeenCalled();
    expect(toast.success).toHaveBeenCalledWith("XMP files updated for 2 photos");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("says why sidecars still could not be written", async () => {
    const user = userEvent.setup();
    const unsaved = [
      { path: "/job/a.ARW", problem: { kind: "changed_elsewhere" } },
      { path: "/job/b.ARW", problem: { kind: "shared_name" } },
      { path: "/job/b.JPG", problem: { kind: "shared_name" } },
      { path: "/job/c.ARW", problem: { kind: "failed", detail: "Read-only file system" } },
    ] as const;
    answer({
      session_write_pending_xmp: { written: 0, unsaved },
      // A failed write is still pending in the list; Try again said why.
      session_xmp_review: unsaved.map(({ path, problem }) => ({ path, problem: problem.kind === "failed" ? null : problem, volume: null })),
    });
    render(<SessionXmpStatus directory="/job" count={4} onChanged={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "4 XMP not updated" }));
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(reasonTexts()).toContain("1 XMP file could not be written: Read-only file system"));
    expect(reasonTexts()).toEqual([
      "1 XMP file was changed by another program. Rawtonic doesn't overwrite it; the library keeps your version.",
      "2 photos have the same name as another one (RAW and JPEG), so both would write the same XMP file.",
      "1 XMP file could not be written: Read-only file system",
    ]);
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("asks the app to show the photographs, and lists them as a view", async () => {
    const user = userEvent.setup();
    const shown = vi.fn();
    window.addEventListener(SHOW_PENDING_XMP_EVENT, shown);
    render(<SessionXmpStatus directory="/job" count={1} onChanged={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "1 XMP not updated" }));
    await user.click(screen.getByRole("button", { name: "Show photos" }));
    window.removeEventListener(SHOW_PENDING_XMP_EVENT, shown);
    expect(shown).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("dialog")).toBeNull();

    vi.mocked(invoke).mockResolvedValue(["/job/a.ARW"]);
    const select = vi.fn();
    await openPendingXmpView("/job", "XMP not updated", select);
    expect(invoke).toHaveBeenCalledWith("session_pending_xmp", { directory: "/job" });
    expect(select).toHaveBeenCalledWith(PENDING_XMP_VIEW_ID, "XMP not updated", ["/job/a.ARW"], false);
  });

  it("lists the photographs again after Try again when they are shown", async () => {
    const user = userEvent.setup();
    const shown = vi.fn();
    window.addEventListener(SHOW_PENDING_XMP_EVENT, shown);
    useLibraryStore.setState({ activeAlbumId: PENDING_XMP_VIEW_ID });
    answer({
      session_write_pending_xmp: { written: 1, unsaved: [{ path: "/job/b.ARW", problem: { kind: "offline" } }] },
      session_xmp_review: [{ path: "/job/b.ARW", problem: { kind: "offline" }, volume: "Card" }],
    });
    render(<SessionXmpStatus directory="/job" count={2} onChanged={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "2 XMP not updated" }));
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(shown).toHaveBeenCalled());
    window.removeEventListener(SHOW_PENDING_XMP_EVENT, shown);
    useLibraryStore.setState({ activeAlbumId: null });
    expect(shown).toHaveBeenCalledTimes(1);
    expect((shown.mock.calls[0][0] as CustomEvent).detail).toEqual({ refresh: true });
  });

  it("does not show one library's photographs after another one opened", async () => {
    useLibraryStore.setState({ sessionDirectory: "/a" });
    let answer: (paths: string[]) => void = () => {};
    vi.mocked(invoke).mockReturnValue(new Promise((resolve) => { answer = resolve as typeof answer; }));
    const select = vi.fn();
    const opening = openPendingXmpView("/a", "XMP not updated", select);
    useLibraryStore.setState({ sessionDirectory: "/b" });
    answer(["/a/x.ARW"]);
    await opening;
    useLibraryStore.setState({ sessionDirectory: null });
    expect(select).not.toHaveBeenCalled();
  });

  it("closes on Escape without passing the key on (a crop behind it stays)", async () => {
    const user = userEvent.setup();
    const behind = vi.fn();
    window.addEventListener("keydown", behind);
    render(<SessionXmpStatus directory="/job" count={1} onChanged={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "1 XMP not updated" }));
    await user.keyboard("{Escape}");
    window.removeEventListener("keydown", behind);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(behind).not.toHaveBeenCalled();
  });

  it("ignores a retry that finishes after another library opened", async () => {
    const user = userEvent.setup();
    useLibraryStore.setState({ sessionDirectory: "/a" });
    let finish: (retry: XmpRetry) => void = () => {};
    vi.mocked(invoke).mockReturnValue(new Promise((resolve) => { finish = resolve as typeof finish; }));
    const onChanged = vi.fn();
    render(<SessionXmpStatus directory="/a" count={1} onChanged={onChanged} />);
    await user.click(screen.getByRole("button", { name: "1 XMP not updated" }));
    await user.click(screen.getByRole("button", { name: "Try again" }));
    useLibraryStore.setState({ sessionDirectory: "/b" });
    await act(async () => finish({ written: 1, unsaved: [] }));
    useLibraryStore.setState({ sessionDirectory: null });
    expect(onChanged).not.toHaveBeenCalled();
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("says why each XMP is behind as soon as it opens", async () => {
    const user = userEvent.setup();
    answer({
      session_xmp_review: [
        changed("/job/a.ARW"),
        { path: "/job/b.ARW", problem: { kind: "offline" }, volume: "Card" },
        { path: "/job/c.ARW", problem: null, volume: null },
      ],
    });
    render(<SessionXmpStatus directory="/job" count={3} onChanged={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "3 XMP not updated" }));
    await waitFor(() => expect(reasonTexts()).toHaveLength(3));
    expect(reasonTexts()).toEqual([
      "1 XMP file was changed by another program. Rawtonic doesn't overwrite it; the library keeps your version.",
      "1 photo is offline. Its XMP file is updated when the drive is connected again.",
      "1 XMP file can be written again.",
    ]);
    expect(invoke).toHaveBeenCalledWith("session_xmp_review", { directory: "/job" });
  });

  it("keeps the library's metadata for a changed sidecar through the photo's write queue", async () => {
    const user = userEvent.setup();
    let left = [changed("/job/a.ARW"), changed("/job/b.ARW")];
    const onChanged = vi.fn();
    answer({
      session_xmp_review: () => left,
      session_xmp_resolve: ({ paths }: { paths: string[] }) => {
        left = left.filter((entry) => !paths.includes(entry.path));
        return { done: paths, offline: [], conflicts: [], failed: [], undo: [] };
      },
    });
    render(<SessionXmpStatus directory="/job" count={2} onChanged={onChanged} />);
    await user.click(screen.getByRole("button", { name: "2 XMP not updated" }));
    await user.click(await screen.findByRole("button", { name: "Keep library for a.ARW" }));
    expect(invoke).toHaveBeenCalledWith("session_xmp_resolve", { directory: "/job", paths: ["/job/a.ARW"], keep: "library" });
    await waitFor(() => expect(screen.queryByRole("button", { name: "Keep library for a.ARW" })).toBeNull());
    expect(screen.getByRole("button", { name: "Keep library for b.ARW" })).toBeTruthy();
    expect(onChanged).toHaveBeenCalled();
    expect(toast.success).toHaveBeenCalledWith("XMP conflict resolved for 1 photo");
    expect(useCatalogUndoStore.getState().past).toHaveLength(0);
  });

  it("reads the XMP into the library as one step of Undo", async () => {
    const user = userEvent.setup();
    useUIStore.setState({ activeView: "library" });
    useCatalogUndoStore.setState({ past: [], future: [] });
    let left = [changed("/job/a.ARW"), changed("/job/b.ARW")];
    const receipts = [{ asset: "a" }, { asset: "b" }];
    answer({
      session_xmp_review: () => left,
      session_xmp_resolve: ({ paths }: { paths: string[] }) => {
        left = [];
        return { done: paths, offline: [], conflicts: [], failed: [], undo: receipts };
      },
      session_xmp_undo: () => {
        left = [changed("/job/a.ARW"), changed("/job/b.ARW")];
        return { done: ["/job/a.ARW", "/job/b.ARW"], offline: [], conflicts: [], failed: [], undo: [] };
      },
    });
    render(<SessionXmpStatus directory="/job" count={2} onChanged={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "2 XMP not updated" }));
    await user.click(await screen.findByRole("button", { name: "Use XMP for all" }));
    expect(invoke).toHaveBeenCalledWith("session_xmp_resolve", {
      directory: "/job",
      paths: ["/job/a.ARW", "/job/b.ARW"],
      keep: "xmp",
    });
    await waitFor(() => expect(useCatalogUndoStore.getState().past).toHaveLength(1));
    await act(async () => undo());
    expect(invoke).toHaveBeenCalledWith("session_xmp_undo", {
      directory: "/job",
      steps: receipts,
      paths: ["/job/a.ARW", "/job/b.ARW"],
    });
    expect(useCatalogUndoStore.getState().future).toHaveLength(1);
    useCatalogUndoStore.setState({ past: [], future: [] });
  });

  it("lists thousands of conflicts without putting every row in the page", async () => {
    const user = userEvent.setup();
    const many = Array.from({ length: 3000 }, (_, i) => changed(`/job/IMG_${String(i).padStart(4, "0")}.ARW`));
    answer({
      session_xmp_review: many,
      session_xmp_resolve: ({ paths }: { paths: string[] }) => ({ done: paths, offline: [], conflicts: [], failed: [], undo: [] }),
    });
    render(<SessionXmpStatus directory="/job" count={3000} onChanged={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "3000 XMP not updated" }));
    const list = await screen.findByRole("list", { name: "3000 XMP files changed by another program" });
    expect(within(list).getAllByRole("listitem").length).toBeLessThan(20);
    expect(within(list).queryByText("IMG_2500.ARW")).toBeNull();
    fireEvent.scroll(list.parentElement!, { target: { scrollTop: 2500 * 30 } });
    expect(within(list).getByText("IMG_2500.ARW")).toBeTruthy();
    expect(within(list).getAllByRole("listitem").length).toBeLessThan(20);
    await user.click(screen.getByRole("button", { name: "Keep library for all" }));
    const call = vi.mocked(invoke).mock.calls.find(([command]) => command === "session_xmp_resolve");
    expect((call?.[1] as { paths: string[] }).paths).toHaveLength(3000);
  });
});
