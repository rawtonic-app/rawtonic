import "../../i18n";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import SessionAlbumSubMenu from "./SessionAlbumSubMenu";
import i18n from "i18next";
import { SESSION_ALBUMS_CHANGED, type SessionAlbum } from "./StudioAlbums";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../../activity/notify", () => ({ notify: { error: vi.fn(), success: vi.fn() } }));
const mockInvoke = vi.mocked(invoke);
afterEach(cleanup);

it("adds the targeted photographs to a session album and tells the Albums tool", async () => {
  const albums: SessionAlbum[] = [
    { id: "g", name: "Family", parent: null, count: 0, kind: "group" },
    { id: "a", name: "Paul", parent: "g", count: 4 },
  ];
  mockInvoke.mockImplementation(async (command) =>
    command === "session_albums" ? albums : undefined,
  );
  const changed = vi.fn();
  window.addEventListener(SESSION_ALBUMS_CHANGED, changed);
  const hide = vi.fn();
  render(
    <SessionAlbumSubMenu
      directory="/job"
      paths={["/job/Capture/a.raf", "/job/Capture/b.raf"]}
      hideContextMenu={hide}
    />,
  );
  // Groups are headings, not targets.
  expect(await screen.findByText("Family")).toBeTruthy();
  expect(screen.queryByRole("menuitem", { name: /Family/ })).toBeNull();
  fireEvent.click(screen.getByRole("menuitem", { name: /Paul/ }));
  expect(hide).toHaveBeenCalled();
  await vi.waitFor(() => expect(changed).toHaveBeenCalledTimes(1));
  expect(mockInvoke).toHaveBeenCalledWith("session_album_add", {
    directory: "/job",
    id: "a",
    paths: ["/job/Capture/a.raf", "/job/Capture/b.raf"],
  });
  window.removeEventListener(SESSION_ALBUMS_CHANGED, changed);
});

it("offers no smart album as a destination", async () => {
  const albums: SessionAlbum[] = [
    { id: "s", name: "Five stars", parent: null, count: 9, kind: "smart" },
    { id: "a", name: "Paul", parent: null, count: 4 },
  ];
  mockInvoke.mockImplementation(async (command) => (command === "session_albums" ? albums : undefined));
  render(<SessionAlbumSubMenu directory="/job" paths={["/job/Capture/a.raf"]} hideContextMenu={vi.fn()} />);
  expect(await screen.findByRole("menuitem", { name: /Paul/ })).toBeTruthy();
  expect(screen.queryByText("Five stars")).toBeNull();
});

it("says there is no album to add to when there are only smart albums", async () => {
  mockInvoke.mockImplementation(async (command) =>
    command === "session_albums" ? [{ id: "s", name: "Five stars", parent: null, count: 9, kind: "smart" }] : undefined,
  );
  render(<SessionAlbumSubMenu directory="/job" paths={["/job/Capture/a.raf"]} hideContextMenu={vi.fn()} />);
  expect(await screen.findByText(i18n.t("studio.library.albumMenu.empty"))).toBeTruthy();
});
