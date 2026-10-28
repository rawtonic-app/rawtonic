import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import IngestPreviewGrid, { type IngestCandidate } from "./IngestPreviewGrid";
import { createLargePreviewCache, type LargePreview } from "./IngestLoupe";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const revoked: string[] = [];
beforeEach(() => {
  revoked.length = 0;
  URL.revokeObjectURL = vi.fn((url: string) => void revoked.push(url));
  window.localStorage.clear();
});
afterEach(cleanup);

const files = (count: number): IngestCandidate[] =>
  Array.from({ length: count }, (_, i) => ({
    path: `/Volumes/CARD/DCIM/100MSDCF/DSC0${String(i).padStart(4, "0")}.ARW`,
    size: 25_000_000,
    duplicate: false,
  }));

const largePreview = async (path: string): Promise<LargePreview> => ({
  url: `blob:large${path}`,
  info: { iso: "ISO 400", exposure: "1/250 s", aperture: "f/2.8", focalLength: "35 mm" },
});

function Harness({
  list,
  loadPreview = largePreview,
  onCancel,
}: {
  list: IngestCandidate[];
  loadPreview?: (path: string) => Promise<LargePreview>;
  onCancel?: () => void;
}) {
  const [selected, setSelected] = useState(() => new Set(list.map((file) => file.path)));
  return (
    <dialog open onCancel={onCancel}>
      <IngestPreviewGrid
        files={list}
        selected={selected}
        loadThumbnail={async (path) => `blob:thumb${path}`}
        loadPreview={loadPreview}
        onSelectedChange={setSelected}
      />
    </dialog>
  );
}

const shownName = () => document.querySelector(".session-ingest-loupe-title strong")?.textContent;
const loupe = () => screen.queryByRole("region", { name: "Large view" });

describe("Import dialog large view", () => {
  it("opens on double-click and on Enter, shows the large preview and capture info, and closes back to the grid", async () => {
    const user = userEvent.setup();
    render(<Harness list={files(5)} />);
    await user.dblClick(screen.getByRole("checkbox", { name: "DSC00002.ARW" }));
    expect(loupe()).toBeTruthy();
    expect(shownName()).toBe("DSC00002.ARW");
    expect(screen.getByText("3 / 5")).toBeTruthy();
    await waitFor(() =>
      expect(screen.getByRole("img", { name: "DSC00002.ARW" }).getAttribute("src")).toBe(
        "blob:large/Volumes/CARD/DCIM/100MSDCF/DSC00002.ARW",
      ),
    );
    expect(document.querySelector(".session-ingest-loupe-title span")?.textContent).toContain(
      "ISO 400 · 1/250 s · f/2.8 · 35 mm · 25.0 MB",
    );
    // A double-click toggles a tile twice: its checkbox is as it was.
    expect(screen.getByRole("checkbox", { name: "DSC00002.ARW" }).getAttribute("aria-checked")).toBe("true");

    await user.keyboard(" ");
    expect(loupe()).toBeNull();
    await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("DSC00002.ARW"));

    await user.keyboard("{Enter}");
    expect(shownName()).toBe("DSC00002.ARW");
    await user.dblClick(document.querySelector(".session-ingest-loupe-image")!);
    expect(loupe()).toBeNull();
  });

  it("steps through the photos with the arrow keys, stopping at the ends", async () => {
    const user = userEvent.setup();
    render(<Harness list={files(3)} />);
    await user.dblClick(screen.getByRole("checkbox", { name: "DSC00001.ARW" }));
    await user.keyboard("{ArrowRight}");
    expect(shownName()).toBe("DSC00002.ARW");
    await user.keyboard("{ArrowRight}");
    expect(shownName()).toBe("DSC00002.ARW");
    await user.keyboard("{ArrowLeft}{ArrowLeft}");
    expect(shownName()).toBe("DSC00000.ARW");
    await user.keyboard("{ArrowUp}");
    expect(shownName()).toBe("DSC00000.ARW");
    await user.keyboard("{End}");
    expect(shownName()).toBe("DSC00002.ARW");
    await user.click(screen.getByRole("button", { name: "Previous photo" }));
    expect(shownName()).toBe("DSC00001.ARW");
    // Back in the grid the photo last shown has the focus.
    await user.keyboard("{Escape}");
    await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("DSC00001.ARW"));
  });

  it("moves the focus through the grid with the arrow keys and opens the focused photo with Enter", async () => {
    const user = userEvent.setup();
    render(<Harness list={files(8)} />);
    screen.getByRole("checkbox", { name: "DSC00000.ARW" }).focus();
    await user.keyboard("{ArrowRight}{ArrowRight}");
    expect(document.activeElement?.getAttribute("aria-label")).toBe("DSC00002.ARW");
    await user.keyboard("{Enter}");
    expect(shownName()).toBe("DSC00002.ARW");
    // Enter opened the view; it did not uncheck the tile.
    expect(screen.getByRole("checkbox", { name: "DSC00002.ARW" }).getAttribute("aria-checked")).toBe("true");
  });

  it("takes Escape for itself while open, so the dialog stays; a second Escape reaches the dialog", async () => {
    const user = userEvent.setup();
    const onCancel = vi.fn();
    render(<Harness list={files(3)} onCancel={onCancel} />);
    await user.dblClick(screen.getByRole("checkbox", { name: "DSC00000.ARW" }));
    const dialog = document.querySelector("dialog")!;
    const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => void document.activeElement!.dispatchEvent(escape));
    expect(escape.defaultPrevented).toBe(true);
    expect(loupe()).toBeNull();
    // The cancel a webview may still send for that key is not the dialog's.
    const cancel = new Event("cancel", { cancelable: true });
    act(() => void dialog.dispatchEvent(cancel));
    expect(cancel.defaultPrevented).toBe(true);
    expect(onCancel).not.toHaveBeenCalled();
    expect(document.querySelector("dialog")?.open).toBe(true);

    await act(() => new Promise((resolve) => setTimeout(resolve, 300)));
    act(() => void dialog.dispatchEvent(new Event("cancel", { cancelable: true })));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("toggles the shown photo's import checkbox", async () => {
    const user = userEvent.setup();
    render(<Harness list={files(3)} />);
    await user.dblClick(screen.getByRole("checkbox", { name: "DSC00001.ARW" }));
    const check = screen.getByRole("checkbox", { name: "Import" });
    expect(check.getAttribute("aria-checked")).toBe("true");
    await user.click(check);
    expect(check.getAttribute("aria-checked")).toBe("false");
    expect(screen.getByRole("status").textContent).toContain("2 of 3 photos selected");
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("checkbox", { name: "Import" }).getAttribute("aria-checked")).toBe("true");
    await user.keyboard("{Escape}");
    expect(screen.getByRole("checkbox", { name: "DSC00001.ARW" }).getAttribute("aria-checked")).toBe("false");
  });

  it("shows the grid preview until the large one arrives", async () => {
    const user = userEvent.setup();
    let finish: (preview: LargePreview) => void = () => {};
    const slow = () => new Promise<LargePreview>((resolve) => (finish = resolve));
    render(<Harness list={files(1)} loadPreview={slow} />);
    await waitFor(() => expect(document.querySelectorAll(".session-ingest-thumb img")).toHaveLength(1));
    await user.dblClick(screen.getByRole("checkbox", { name: "DSC00000.ARW" }));
    const image = () => screen.getByRole("img", { name: "DSC00000.ARW" });
    expect(image().getAttribute("src")).toBe("blob:thumb/Volumes/CARD/DCIM/100MSDCF/DSC00000.ARW");
    expect(image().className).toBe("is-placeholder");
    await act(async () => finish({ url: "blob:large", info: null }));
    expect(image().getAttribute("src")).toBe("blob:large");
  });

  it("switches the thumbnail size and keeps it", async () => {
    render(<Harness list={files(2)} />);
    const slider = screen.getByRole("slider", { name: "Thumbnail size" });
    const grid = () => screen.getByRole("group", { name: "Photos to import" });
    expect(grid().style.getPropertyValue("--ingest-tile")).toBe("112px");
    fireEvent.change(slider, { target: { value: "2" } });
    expect(grid().style.getPropertyValue("--ingest-tile")).toBe("168px");
    cleanup();
    render(<Harness list={files(2)} />);
    expect(grid().style.getPropertyValue("--ingest-tile")).toBe("168px");
  });
});

describe("large preview cache", () => {
  it("loads the shown photo first, keeps its neighbours, drops what is no longer wanted and discards late arrivals", async () => {
    const pending = new Map<string, () => void>();
    const load = vi.fn(
      (path: string) =>
        new Promise<LargePreview>((resolve) =>
          pending.set(path, () => resolve({ url: `blob:${path}`, info: null })),
        ),
    );
    const loaded = vi.fn();
    const cache = createLargePreviewCache(load, loaded, 2);
    cache.want(["a", "b", "c", "d"]);
    expect(load.mock.calls.map((call) => call[0])).toEqual(["a", "b"]);

    // The view moved on before "c" and "d" started: they are never loaded.
    cache.want(["x", "a"]);
    await act(async () => pending.get("a")!());
    expect(cache.get("a")?.url).toBe("blob:a");
    expect(load.mock.calls.map((call) => call[0])).toEqual(["a", "b", "x"]);
    // "b" arrives after it stopped being wanted: discarded, its URL revoked.
    await act(async () => pending.get("b")!());
    expect(cache.get("b")).toBeUndefined();
    expect(revoked).toEqual(["blob:b"]);
    await act(async () => pending.get("x")!());
    expect(loaded.mock.calls.map((call) => call[0])).toEqual(["a", "x"]);

    cache.want(["x"]);
    expect(revoked).toEqual(["blob:b", "blob:a"]);
    cache.clear();
    expect(revoked).toEqual(["blob:b", "blob:a", "blob:x"]);
    expect(cache.get("x")).toBeUndefined();
  });
});
