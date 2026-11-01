import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import { LoupeFaces, PhotoEyesBadge } from "./StudioEyes";
import StudioFilters from "./StudioFilters";
import { useEyesStore, type EyeFace, type PhotoEyes } from "../../store/useEyesStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { Invokes, RawStatus, type AppSettings } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockInvoke = vi.mocked(invoke);

const face = (
  index: number,
  state: "open" | "closed" | "uncertain",
  overrideState: "open" | null = null,
  focus: "sharp" | "soft" | "uncertain" | null = "sharp",
): EyeFace => ({
  face: index,
  bbox: [0.1 * index, 0.2, 0.1, 0.1] as [number, number, number, number],
  state,
  ear: 0.2,
  accessories: 0,
  overrideState,
  focus,
  focusSigma: focus ? 1.2 : null,
  focusOverride: null,
});

let eyes: PhotoEyes;
const settings = (eyeCheck: boolean) =>
  useSettingsStore.setState({ appSettings: { eyeCheck } as AppSettings, handleSettingsChange: vi.fn() });

beforeEach(() => {
  eyes = { faces: [face(0, "open"), face(1, "closed"), face(2, "uncertain")], error: null };
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command, args) => {
    if (command === Invokes.PhotoEyes) return eyes;
    if (command === Invokes.FaceEyeCrops) return (args as { boxes: unknown[] }).boxes.map(() => "data:image/jpeg;base64,");
    if (command === Invokes.SetFaceEyes) {
      const { face: index, state } = args as { face: number; state: "open" | null };
      eyes = { ...eyes, faces: eyes.faces!.map((f) => (f.face === index ? { ...f, overrideState: state } : f)) };
      return null;
    }
    if (command === Invokes.SetFaceFocus) {
      const { face: index, focus } = args as { face: number; focus: "sharp" | "soft" | null };
      eyes = { ...eyes, faces: eyes.faces!.map((f) => (f.face === index ? { ...f, focusOverride: focus } : f)) };
      return null;
    }
    return null;
  });
  useLibraryStore.setState({
    sessionDirectory: "/job",
    imageFlags: {},
    filterCriteria: { flag: "all", rating: 0, colors: [], rawStatus: RawStatus.All },
    searchCriteria: { text: "", tags: [], mode: "OR" },
  });
  useEyesStore.setState({ summaries: {}, generation: 0 });
  settings(true);
});
afterEach(cleanup);

it("badges photographs with closed eyes or faces to check, never open ones", () => {
  useEyesStore.setState({
    summaries: {
      "/job/a.arw": { faces: 14, closed: 2, notVisible: 1, uncertain: 3, soft: 4 },
      "/job/b.arw": { faces: 3, closed: 0, notVisible: 0, uncertain: 1, soft: 0 },
      "/job/c.arw": { faces: 4, closed: 0, notVisible: 1, uncertain: 0, soft: 0 },
      "/job/d.arw": { faces: 5, closed: 0, notVisible: 0, uncertain: 1, soft: 2 },
    },
  });
  render(
    <>
      <PhotoEyesBadge path="/job/a.arw" />
      <PhotoEyesBadge path="/job/b.arw" />
      <PhotoEyesBadge path="/job/c.arw" />
      <PhotoEyesBadge path="/job/d.arw" />
      <PhotoEyesBadge path="/job/unread.arw" />
    </>,
  );
  const closed = screen.getByRole("img", {
    name: "Faces: 14 · eyes closed: 2 · eyes soft: 4 · to check: 3 · eyes not visible: 1",
  });
  expect(closed.className).toContain("is-closed");
  expect(closed.textContent).toBe("2/14");
  const check = screen.getByRole("img", { name: "Faces: 3 · to check: 1" });
  expect(check.className).toContain("is-uncertain");
  // Soft eyes come before faces to check.
  const soft = screen.getByRole("img", { name: "Faces: 5 · eyes soft: 2 · to check: 1" });
  expect(soft.className).toContain("is-soft");
  expect(soft.textContent).toBe("2/5");
  expect(screen.getAllByRole("img")).toHaveLength(3);
});

it("offers Check eyes until it is on, then filters closed eyes and those to check", async () => {
  const user = userEvent.setup();
  settings(false);
  const { rerender } = render(<StudioFilters counts={{ all: 3, picked: 0, unflagged: 3, rejected: 0 }} />);
  const select = screen.getByLabelText("Filter by eyes");
  expect([...select.querySelectorAll("option")].map((o) => o.textContent)).toEqual(["All eyes", "Check eyes…"]);
  await user.selectOptions(select, "start");
  await waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith(Invokes.StartEyeCheck, { directory: "/job", download: true, resume: true }),
  );
  expect(useSettingsStore.getState().handleSettingsChange).toHaveBeenCalledWith({ eyeCheck: true });
  settings(true);
  rerender(<StudioFilters counts={{ all: 3, picked: 0, unflagged: 3, rejected: 0 }} />);
  expect([...screen.getByLabelText("Filter by eyes").querySelectorAll("option")].map((o) => o.textContent)).toEqual([
    "All eyes",
    "Eyes closed",
    "Eyes soft",
    "Eyes to check",
  ]);
  await user.selectOptions(screen.getByLabelText("Filter by eyes"), "soft");
  expect(useLibraryStore.getState().filterCriteria.eyes).toBe("soft");
  await user.selectOptions(screen.getByLabelText("Filter by eyes"), "closed");
  expect(useLibraryStore.getState().filterCriteria.eyes).toBe("closed");
  await user.click(screen.getByRole("button", { name: "Clear filters" }));
  expect(useLibraryStore.getState().filterCriteria.eyes).toBe("all");
});

it("shows the Loupe's faces and lets the photographer's call win, or give the suggestion back", async () => {
  const user = userEvent.setup();
  const { rerender } = render(<LoupeFaces path="/job/a.arw" />);
  expect(await screen.findByText("Faces: 3 · eyes closed: 1 · to check: 1")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Face 2: Eyes closed or partly closed" }));
  await user.click(screen.getByRole("menuitemradio", { name: "Eyes open" }));
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.SetFaceEyes, {
    directory: "/job",
    path: "/job/a.arw",
    face: 1,
    state: "open",
  });
  // The store's change reads the photograph again.
  rerender(<LoupeFaces path="/job/a.arw" />);
  expect(await screen.findByText("Faces: 3 · to check: 1")).toBeTruthy();
  const own = screen.getByRole("button", { name: "Face 2: Eyes open" });
  expect(own.className).toContain("is-own");
  await user.click(own);
  await user.click(screen.getByRole("menuitemradio", { name: "Use the suggestion (Eyes closed or partly closed)" }));
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.SetFaceEyes, {
    directory: "/job",
    path: "/job/a.arw",
    face: 1,
    state: null,
  });
});

it("shows nothing while eyes are not checked or a photograph has no faces", async () => {
  settings(false);
  const { container, rerender } = render(<LoupeFaces path="/job/a.arw" />);
  expect(container.textContent).toBe("");
  settings(true);
  eyes = { faces: [], error: null };
  rerender(<LoupeFaces path="/job/b.arw" />);
  await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith(Invokes.PhotoEyes, { directory: "/job", path: "/job/b.arw" }));
  expect(container.textContent).toBe("");
});

it("shows each face's eye focus beside its eyes and lets the photographer's focus call win", async () => {
  const user = userEvent.setup();
  eyes = {
    faces: [face(0, "open", null, "soft"), face(1, "open", null, "uncertain"), face(2, "open", null, null)],
    error: null,
  };
  const { rerender } = render(<LoupeFaces path="/job/a.arw" />);
  expect(await screen.findByText("Faces: 3 · eyes soft: 1")).toBeTruthy();
  const soft = screen.getByRole("button", { name: "Face 1 eye focus: Eyes soft (check focus)" });
  expect(soft.className).toContain("is-soft");
  expect(
    screen.getByRole("button", { name: "Face 2 eye focus: Eye focus not judged (face small or uncertain)" }),
  ).toBeTruthy();
  // A face read before eye focus has no focus call to show.
  expect(screen.queryByRole("button", { name: /^Face 3 eye focus/ })).toBeNull();
  await user.click(soft);
  // Never "uncertain" as a call.
  expect(screen.getAllByRole("menuitemradio").map((b) => b.textContent)).toEqual([
    "Eyes sharp",
    "Eyes soft (check focus)",
    "Use the suggestion (Eyes soft (check focus))",
  ]);
  await user.click(screen.getByRole("menuitemradio", { name: "Eyes sharp" }));
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.SetFaceFocus, {
    directory: "/job",
    path: "/job/a.arw",
    face: 0,
    focus: "sharp",
  });
  rerender(<LoupeFaces path="/job/a.arw" />);
  expect(await screen.findByText("Faces: 3")).toBeTruthy();
  const own = screen.getByRole("button", { name: "Face 1 eye focus: Eyes sharp" });
  expect(own.className).toContain("is-own");
  expect(own.title).toBe("Eyes sharp (your call; suggested: Eyes soft (check focus))");
  // The eyes call keeps its own menu.
  await user.click(screen.getByRole("button", { name: "Face 1: Eyes open" }));
  expect(screen.getByRole("menu", { name: "Your call for this face" })).toBeTruthy();
  expect(screen.queryByRole("menu", { name: "Your eye-focus call for this face" })).toBeNull();
});
