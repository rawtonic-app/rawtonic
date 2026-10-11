import { afterEach, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";

import MergeModal from "./MergeModal";
import {
  CLOSED_MERGE_MODAL,
  type MergeModalState,
} from "../../store/useUIStore";

afterEach(cleanup);

const open = (patch: Partial<MergeModalState> = {}): MergeModalState => ({
  ...CLOSED_MERGE_MODAL,
  isOpen: true,
  kind: "hdr",
  sourcePaths: ["/s/a.RAF", "/s/b.RAF", "/s/c.RAF"],
  ...patch,
});

function setup(
  state: MergeModalState,
  onSave = vi.fn(() => Promise.resolve("/s/b-HDR.dng")),
) {
  const onRun = vi.fn();
  const onRefinish = vi.fn(() => Promise.resolve());
  const onOpenFile = vi.fn();
  const onClose = vi.fn();
  const view = render(
    <MergeModal
      state={state}
      thumbnail={null}
      onClose={onClose}
      onOpenFile={onOpenFile}
      onRun={onRun}
      onRefinish={onRefinish}
      onSave={onSave}
    />,
  );
  return { onRun, onRefinish, onOpenFile, onClose, onSave, view };
}

it("starts the preview with the default options when it opens", () => {
  const { onRun } = setup(open({ isProcessing: true }));
  expect(onRun).toHaveBeenCalledWith("hdr", { align: true, deghost: "off" });
});

it("marks the preview stale after an option changes and runs it again", () => {
  const { onRun, view } = setup(open());
  view.rerender(
    <MergeModal
      state={open({
        previewBase64: "data:image/png;base64,AA",
        report: { frames: [] },
      })}
      thumbnail={null}
      onClose={vi.fn()}
      onOpenFile={vi.fn()}
      onRun={onRun}
      onRefinish={vi.fn(() => Promise.resolve())}
      onSave={vi.fn()}
    />,
  );
  const merge = screen.getByRole("button", {
    name: "modals.merge.merge",
  }) as HTMLButtonElement;
  expect(merge.disabled).toBe(false);
  fireEvent.click(
    screen.getByRole("button", { name: "modals.merge.amount.medium" }),
  );
  expect(screen.getByText("modals.merge.stale")).toBeTruthy();
  expect(merge.disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "modals.merge.update" }));
  expect(onRun).toHaveBeenLastCalledWith("hdr", {
    align: true,
    deghost: "medium",
  });
});

it("offers the panorama's projections, boundary warp and crop", () => {
  const { onRun } = setup(open({ kind: "panorama", isProcessing: true }));
  expect(onRun).toHaveBeenCalledWith("panorama", {
    projection: "auto",
    boundaryWarp: 0,
    autoCrop: true,
  });
  for (const p of [
    "auto",
    "spherical",
    "cylindrical",
    "perspective",
    "panini",
  ]) {
    expect(
      screen.getByRole("button", { name: `modals.merge.projection.${p}` }),
    ).toBeTruthy();
  }
});

it("saves the merge and opens the saved file", async () => {
  const { onSave, onOpenFile } = setup(
    open({ previewBase64: "data:image/png;base64,AA" }),
  );
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "modals.merge.merge" }));
  });
  expect(onSave).toHaveBeenCalledWith(true, false);
  expect(screen.getByText("b-HDR.dng")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "modals.merge.open" }));
  expect(onOpenFile).toHaveBeenCalledWith("/s/b-HDR.dng");
});

it("applies a new boundary warp to the stitched panorama without stitching again", () => {
  const { onRun, onRefinish, view } = setup(open({ kind: "panorama" }));
  expect(onRun).toHaveBeenCalledTimes(1);
  view.rerender(
    <MergeModal
      state={open({
        kind: "panorama",
        previewBase64: "data:image/png;base64,AA",
      })}
      thumbnail={null}
      onClose={vi.fn()}
      onOpenFile={vi.fn()}
      onRun={onRun}
      onRefinish={onRefinish}
      onSave={vi.fn()}
    />,
  );
  const slider = view.container.querySelector(
    ".merge-slider input",
  ) as HTMLInputElement;
  fireEvent.change(slider, { target: { value: "40" } });
  fireEvent.click(screen.getByRole("button", { name: "modals.merge.update" }));
  expect(onRefinish).toHaveBeenCalledWith({
    projection: "auto",
    boundaryWarp: 40,
    autoCrop: true,
  });
  expect(onRun).toHaveBeenCalledTimes(1);
  // A new projection needs a new stitch.
  fireEvent.click(
    screen.getByRole("button", { name: "modals.merge.projection.cylindrical" }),
  );
  fireEvent.click(screen.getByRole("button", { name: "modals.merge.update" }));
  expect(onRun).toHaveBeenCalledTimes(2);
});

it("offers HDR and panorama options for an HDR panorama and re-merges when HDR changes", () => {
  const { onRun, onRefinish, view } = setup(open({ kind: "hdrPanorama" }));
  expect(onRun).toHaveBeenCalledWith("hdrPanorama", {
    hdr: { align: true, deghost: "off" },
    panorama: { projection: "auto", boundaryWarp: 0, autoCrop: true },
  });
  view.rerender(
    <MergeModal
      state={open({
        kind: "hdrPanorama",
        previewBase64: "data:image/png;base64,AA",
      })}
      thumbnail={null}
      onClose={vi.fn()}
      onOpenFile={vi.fn()}
      onRun={onRun}
      onRefinish={onRefinish}
      onSave={vi.fn()}
    />,
  );
  fireEvent.change(
    view.container.querySelector(".merge-slider input") as HTMLInputElement,
    {
      target: { value: "30" },
    },
  );
  fireEvent.click(screen.getByRole("button", { name: "modals.merge.update" }));
  expect(onRefinish).toHaveBeenCalledTimes(1);
  fireEvent.click(
    screen.getByRole("button", { name: "modals.merge.amount.high" }),
  );
  fireEvent.click(screen.getByRole("button", { name: "modals.merge.update" }));
  expect(onRun).toHaveBeenCalledTimes(2);
  expect(onRefinish).toHaveBeenCalledTimes(1);
});

it("offers fill edges only for an uncropped panorama and passes it on saving", async () => {
  const onSave = vi.fn(() => Promise.resolve("/s/b-Pano.dng"));
  setup(
    open({ kind: "panorama", previewBase64: "data:image/png;base64,AA" }),
    onSave,
  );
  expect(screen.queryByText("modals.merge.fillEdges")).toBeNull();
  fireEvent.click(screen.getByText("modals.merge.autoCrop"));
  fireEvent.click(screen.getByText("modals.merge.fillEdges"));
  // The crop change needs an updated preview before saving.
  fireEvent.click(screen.getByRole("button", { name: "modals.merge.update" }));
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "modals.merge.merge" }));
  });
  expect(onSave).toHaveBeenCalledWith(true, true);
});
