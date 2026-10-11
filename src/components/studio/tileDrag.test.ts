import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { PointerEvent as ReactPointerEvent } from "react";
import { TILE_DROP_EVENT, startTileDrag, type TileDropDetail } from "./tileDrag";
import { useLibraryStore } from "../../store/useLibraryStore";

const pointer = (type: string, x: number, y: number) => {
  const event = new MouseEvent(type, { clientX: x, clientY: y, bubbles: true });
  Object.defineProperty(event, "pointerId", { value: 1 });
  return event;
};
let album: HTMLButtonElement;
let tile: HTMLDivElement;

beforeEach(() => {
  album = document.createElement("button");
  album.dataset.albumDrop = "picks";
  tile = document.createElement("div");
  document.body.append(album, tile);
  // jsdom has no layout: the album is "under" the pointer right of x = 100.
  document.elementFromPoint = vi.fn((x: number) => (x > 100 ? album : tile));
  useLibraryStore.setState({ multiSelectedPaths: ["/a.arw", "/b.arw"] });
});
afterEach(() => {
  album.remove();
  tile.remove();
});

const press = (path: string, extra: Partial<ReactPointerEvent<HTMLElement>> = {}) =>
  startTileDrag(
    { button: 0, pointerId: 1, clientX: 10, clientY: 10, currentTarget: tile, target: tile, ...extra } as unknown as ReactPointerEvent<HTMLElement>,
    path,
  );

it("drops the selection on an album and swallows the click that ends the drag", () => {
  const dropped = vi.fn();
  album.addEventListener(TILE_DROP_EVENT, (e) => dropped((e as CustomEvent<TileDropDetail>).detail));
  const clicked = vi.fn();
  tile.addEventListener("click", clicked);
  press("/a.arw");
  document.dispatchEvent(pointer("pointermove", 60, 10));
  expect(document.querySelector(".studio-drag-ghost span")?.textContent).toBe("2 photographs");
  expect(tile.classList.contains("is-drag-source")).toBe(true);
  document.dispatchEvent(pointer("pointermove", 150, 10));
  expect(album.classList.contains("is-drop-target")).toBe(true);
  document.dispatchEvent(pointer("pointerup", 150, 10));
  tile.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  expect(dropped).toHaveBeenCalledWith({ target: "picks", paths: ["/a.arw", "/b.arw"] });
  expect(clicked).not.toHaveBeenCalled();
  expect(document.querySelector(".studio-drag-ghost")).toBeNull();
  expect(album.classList.contains("is-drop-target")).toBe(false);
});

it("drags only the pressed photo when it is not selected, and leaves short presses as clicks", () => {
  const dropped = vi.fn();
  album.addEventListener(TILE_DROP_EVENT, (e) => dropped((e as CustomEvent<TileDropDetail>).detail));
  press("/c.arw");
  document.dispatchEvent(pointer("pointermove", 150, 10));
  document.dispatchEvent(pointer("pointerup", 150, 10));
  // The click that ends a drag (swallowed).
  tile.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  expect(dropped).toHaveBeenLastCalledWith({ target: "picks", paths: ["/c.arw"] });

  const clicked = vi.fn();
  tile.addEventListener("click", clicked);
  press("/c.arw");
  document.dispatchEvent(pointer("pointermove", 12, 11));
  document.dispatchEvent(pointer("pointerup", 12, 11));
  tile.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  expect(clicked).toHaveBeenCalledTimes(1);
  expect(dropped).toHaveBeenCalledTimes(1);
});

it("ignores modified presses and presses on the stars", () => {
  const stars = document.createElement("div");
  stars.className = "studio-photo-rating";
  tile.append(stars);
  press("/a.arw", { metaKey: true });
  press("/a.arw", { target: stars });
  document.dispatchEvent(pointer("pointermove", 150, 10));
  expect(document.querySelector(".studio-drag-ghost")).toBeNull();
});
