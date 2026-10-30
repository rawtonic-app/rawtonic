import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "react-toastify";
import { useLibraryStore } from "../../store/useLibraryStore";
import SessionXmpStatus, { openPendingXmpView, PENDING_XMP_VIEW_ID, SHOW_PENDING_XMP_EVENT, type XmpRetry } from "./SessionXmpStatus";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

afterEach(() => cleanup());
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
    vi.mocked(invoke).mockResolvedValue({
      written: 0,
      unsaved: [
        { path: "/job/a.ARW", problem: { kind: "changed_elsewhere" } },
        { path: "/job/b.ARW", problem: { kind: "shared_name" } },
        { path: "/job/b.JPG", problem: { kind: "shared_name" } },
        { path: "/job/c.ARW", problem: { kind: "failed", detail: "Read-only file system" } },
      ],
    });
    render(<SessionXmpStatus directory="/job" count={4} onChanged={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "4 XMP not updated" }));
    await user.click(screen.getByRole("button", { name: "Try again" }));
    const items = (await screen.findAllByRole("listitem")).map((item) => item.textContent);
    expect(items).toEqual([
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
    vi.mocked(invoke).mockResolvedValue({ written: 1, unsaved: [{ path: "/job/b.ARW", problem: { kind: "offline" } }] });
    render(<SessionXmpStatus directory="/job" count={2} onChanged={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "2 XMP not updated" }));
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByRole("listitem");
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
});
