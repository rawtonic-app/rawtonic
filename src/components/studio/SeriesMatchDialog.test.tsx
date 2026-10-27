import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { SeriesMatchDialogHost, proposalValues, useSeriesMatchDialog, type MatchProposal } from "./SeriesMatchDialog";
import { registerRecipeWriter, type RecipeValues } from "../../utils/appUndo";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => undefined) }));

const proposal = (path: string, extra: Partial<MatchProposal>): MatchProposal => ({
  path,
  status: "match",
  reasons: [],
  exposure: null,
  whiteBalance: null,
  confidence: 0.9,
  exposureAnchor: "scene",
  whiteBalanceAnchor: "scene",
  currentExposure: 0,
  currentWhiteBalance: { temperature: 5500, tint: 5 },
  ...extra,
});

let analyses: { references: string[]; options: unknown; useFaces: boolean }[] = [];
let writes: RecipeValues[][] = [];

beforeEach(() => {
  analyses = [];
  writes = [];
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  registerRecipeWriter(async (values) => {
    writes.push(values);
    return values.map(({ path }) => ({ path, values: {} }));
  });
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === "analyze_series_match") {
      const a = args as { references: string[]; options: unknown; useFaces: boolean };
      analyses.push(a);
      return {
        faceModels: "missing",
        elapsedMs: 1200,
        cancelled: false,
        proposals: [
          proposal("/s/a.raf", { status: "reference" }),
          proposal("/s/b.raf", { exposure: 0.8, whiteBalance: { temperature: 3300, tint: 8 }, exposureAnchor: "skinAndScene" }),
          proposal("/s/c.raf", { status: "review", exposure: 1.4, confidence: 0.3, reasons: ["littleInCommon"] }),
          proposal("/s/d.raf", { status: "keep", reasons: ["bracket"] }),
        ],
      };
    }
    return undefined;
  });
});
afterEach(() => {
  cleanup();
  useSeriesMatchDialog.getState().close();
});

const open = async () => {
  act(() =>
    useSeriesMatchDialog.getState().open({ paths: ["/s/a.raf", "/s/b.raf", "/s/c.raf", "/s/d.raf"], references: ["/s/a.raf"] }),
  );
  render(<SeriesMatchDialogHost />);
  await screen.findByText(/1 to match · 1 to check · 1 kept/);
};

it("writes only the changes of the ticked photos: matches ticked, review and kept not", async () => {
  await open();
  expect(analyses[0]).toMatchObject({ references: ["/s/a.raf"], options: { exposure: true, whiteBalance: true }, useFaces: true });
  expect(screen.getByText("Exposure bracket – kept")).toBeTruthy();
  expect(screen.getByText(/Little in common/)).toBeTruthy();
  expect(screen.getByText("5500 K → 3300 K, tint +3")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Apply to 1 photo" }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0]).toEqual([
    { path: "/s/b.raf", values: { exposure: 0.8, whiteBalance: "custom", wbTemperature: 3300, wbTint: 8 } },
  ]);
});

it("a photo for review is applied once ticked", async () => {
  await open();
  fireEvent.click(screen.getByRole("checkbox", { name: "Apply to c.raf" }));
  fireEvent.click(screen.getByRole("button", { name: "Apply to 2 photos" }));
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(writes[0].map((w) => w.path)).toEqual(["/s/b.raf", "/s/c.raf"]);
});

it("another reference or option analyses again; faces are downloaded only on request", async () => {
  await open();
  fireEvent.click(screen.getAllByRole("button", { name: "Use as reference" })[2]);
  await waitFor(() => expect(analyses).toHaveLength(2));
  expect(analyses[1].references).toEqual(["/s/a.raf", "/s/c.raf"]);
  fireEvent.click(screen.getByRole("checkbox", { name: "White balance" }));
  await waitFor(() => expect(analyses).toHaveLength(3));
  expect(analyses[2].options).toEqual({ exposure: true, whiteBalance: false });
  expect(analyses.every((a) => !(a as { downloadFaceModels?: boolean }).downloadFaceModels)).toBe(true);
  fireEvent.click(await screen.findByRole("button", { name: "Download face models" }));
  await waitFor(() => expect(analyses).toHaveLength(4));
  expect((analyses[3] as { downloadFaceModels?: boolean }).downloadFaceModels).toBe(true);
});

it("the last reference cannot be taken away", async () => {
  await open();
  fireEvent.click(screen.getAllByRole("button", { name: "Use as reference" })[0]);
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(analyses).toHaveLength(1);
});

it("proposal values write exposure and a custom white balance only when they change", () => {
  expect(proposalValues(proposal("/x", { exposure: 0.333 }))).toEqual({ exposure: 0.33 });
  expect(proposalValues(proposal("/x", { whiteBalance: { temperature: 4000, tint: -3 } }))).toEqual({
    whiteBalance: "custom",
    wbTemperature: 4000,
    wbTint: -3,
  });
  expect(proposalValues(proposal("/x", {}))).toEqual({});
});

it("when nothing could be written the dialog stays open without a success notice", async () => {
  registerRecipeWriter(async (values) => {
    writes.push(values);
    return [];
  });
  await open();
  fireEvent.click(screen.getByRole("button", { name: "Apply to 1 photo" }));
  await waitFor(() => expect(writes).toHaveLength(1));
  await waitFor(() => expect(screen.getByRole("button", { name: "Apply to 1 photo" }).hasAttribute("disabled")).toBe(false));
  expect(screen.getByRole("dialog")).toBeTruthy();
});
