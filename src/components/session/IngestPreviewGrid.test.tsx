import "../../i18n";
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import IngestPreviewGrid, { createThumbnailQueue, type IngestCandidate } from "./IngestPreviewGrid";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(cleanup);

const files = (count: number, duplicates: number[] = []): IngestCandidate[] =>
  Array.from({ length: count }, (_, i) => ({
    path: `/Volumes/CARD/DCIM/100MSDCF/DSC0${String(i).padStart(4, "0")}.ARW`,
    size: 25_000_000,
    duplicate: duplicates.includes(i),
  }));

function Harness({
  list,
  load,
  onChange,
}: {
  list: IngestCandidate[];
  load: (path: string) => Promise<string | null>;
  onChange?: (selected: Set<string>) => void;
}) {
  const [selected, setSelected] = useState(
    () => new Set(list.filter((file) => !file.duplicate).map((file) => file.path)),
  );
  return (
    <IngestPreviewGrid
      files={list}
      selected={selected}
      loadThumbnail={load}
      onSelectedChange={(next) => {
        setSelected(next);
        onChange?.(next);
      }}
    />
  );
}

describe("Ingest preview grid", () => {
  it("shows each photograph with a checkbox, the count and size of the selection, and marks duplicates unchecked", async () => {
    const load = vi.fn(async (path: string) => `blob:${path}`);
    render(<Harness list={files(3, [1])} load={load} />);
    const tiles = screen.getAllByRole("checkbox");
    expect(tiles).toHaveLength(3);
    expect(tiles.map((tile) => tile.getAttribute("aria-checked"))).toEqual(["true", "false", "true"]);
    expect(screen.getByText("In library")).toBeTruthy();
    expect(screen.getByRole("status").textContent).toContain("2 of 3 photographs selected · 50.0 MB · 1 already in the library");
    await waitFor(() => expect(document.querySelectorAll("img")).toHaveLength(3));
    expect(document.querySelector("img")?.getAttribute("src")).toMatch(/^blob:/);
  });

  it("selects all, none, single photographs and Shift-click ranges; only checked ones count", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<Harness list={files(5, [4])} load={async () => null} onChange={onChange} />);
    await user.click(screen.getByRole("button", { name: "Select none" }));
    expect(screen.getByRole("status").textContent).toContain("0 of 5 photographs selected");
    await user.click(screen.getByRole("checkbox", { name: "DSC00001.ARW" }));
    await user.keyboard("{Shift>}");
    await user.click(screen.getByRole("checkbox", { name: "DSC00003.ARW" }));
    await user.keyboard("{/Shift}");
    expect(screen.getAllByRole("checkbox").map((tile) => tile.getAttribute("aria-checked"))).toEqual([
      "false",
      "true",
      "true",
      "true",
      "false",
    ]);
    expect(screen.getByRole("status").textContent).toContain("3 of 5 photographs selected · 75.0 MB");
    await user.click(screen.getByRole("checkbox", { name: "DSC00002.ARW" }));
    expect(screen.getByRole("checkbox", { name: "DSC00002.ARW" }).getAttribute("aria-checked")).toBe("false");
    await user.click(screen.getByRole("button", { name: "Select all" }));
    expect(screen.getByRole("status").textContent).toContain("5 of 5 photographs selected · 125 MB");
    expect([...onChange.mock.calls.at(-1)![0]]).toHaveLength(5);
  });

  it("draws only the visible rows of hundreds of photographs and makes previews only for them", async () => {
    const load = vi.fn(async () => null);
    render(<Harness list={files(600)} load={load} />);
    const drawn = screen.getAllByRole("checkbox").length;
    expect(drawn).toBeGreaterThan(0);
    expect(drawn).toBeLessThan(60);
    await waitFor(() => expect(load).toHaveBeenCalledTimes(drawn));
    expect(screen.getByRole("status").textContent).toContain("600 of 600 photographs selected · 15.0 GB");
  });
});

describe("thumbnail queue", () => {
  it("makes a few previews at a time and drops requests whose tile left before their turn", async () => {
    const resolvers: (() => void)[] = [];
    const load = vi.fn(
      (path: string) =>
        new Promise<string | null>((resolve) => resolvers.push(() => resolve(`blob:${path}`))),
    );
    const queue = createThumbnailQueue(load, 2);
    const done: (string | null)[] = [];
    for (const path of ["a", "b", "c", "d"]) void queue.request(path).then((url) => done.push(url));
    expect(load).toHaveBeenCalledTimes(2);
    queue.release("c");
    await act(async () => resolvers.shift()!());
    expect(load).toHaveBeenCalledTimes(3);
    expect(load.mock.calls.map((call) => call[0])).toEqual(["a", "b", "d"]);
    await act(async () => {
      resolvers.shift()!();
      resolvers.shift()!();
    });
    expect(done).toEqual(["blob:a", "blob:b", "blob:d"]);
  });
});
