import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import StudioLayers from "./StudioLayers";
import { useEditorStore } from "../../store/useEditorStore";
import { useProcessStore } from "../../store/useProcessStore";
import { useUIStore } from "../../store/useUIStore";
import { useMaskThumbnailStore } from "../../hooks/useMaskThumbnails";
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER } from "../../utils/adjustments";
import { createSubMask } from "../../utils/maskUtils";
import { Mask } from "../panel/right/Masks";
import { Panel } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
vi.mock("../../context/ContextMenuContext", () => ({ useContextMenu: () => ({ showContextMenu: vi.fn() }) }));
vi.mock("../../hooks/useEditorActions", () => ({ useEditorActions: () => ({ setAdjustments: vi.fn() }) }));
// The rows only read the thumbnails (useMaskThumbnails.test.ts covers updates).
vi.mock("../../hooks/useMaskThumbnails", async (original) => ({
  ...(await original<typeof import("../../hooks/useMaskThumbnails")>()),
  useMaskThumbnailUpdates: () => undefined,
}));

const size = { width: 6000, height: 4000 };
const radial = createSubMask(Mask.Radial, size);
const subject = createSubMask(Mask.AiForeground, size);
const brush = createSubMask(Mask.Brush, size);

/** Thumbnail boxes of the rows, top row first. */
const layerThumbs = () => Array.from(document.querySelectorAll(".studio-layer .studio-mask-thumb.is-layer"));
const componentThumbs = () => Array.from(document.querySelectorAll(".studio-mask-thumb.is-component"));

beforeEach(() => {
  useUIStore.setState({ activePanel: Panel.Masks });
  useProcessStore.setState({ activeAiTasks: {} });
  useEditorStore.setState({
    selectedImage: { path: "/a.cr3", isReady: true, ...size } as never,
    adjustments: {
      ...structuredClone(INITIAL_ADJUSTMENTS),
      masks: [
        { ...structuredClone(INITIAL_MASK_CONTAINER), id: "l1", name: "Vignette", subMasks: [radial] },
        { ...structuredClone(INITIAL_MASK_CONTAINER), id: "l2", name: "Person", subMasks: [subject, brush] },
      ],
    },
    activeMaskContainerId: "l2",
    activeMaskId: subject.id,
  });
  useMaskThumbnailStore.setState({ path: "/a.cr3", layers: {}, components: {} });
});
afterEach(cleanup);

describe("layer list mask thumbnails", () => {
  it("shows each layer's thumbnail and the selected layer's component thumbnails", () => {
    useMaskThumbnailStore.setState({
      layers: { l1: "data:image/png;base64,l1", l2: "data:image/png;base64,l2" },
      components: { [subject.id]: "data:image/png;base64,s", [brush.id]: "data:image/png;base64,b" },
    });
    render(<StudioLayers />);
    // Top row = the layer applied last (l2), then l1; the Background row
    // keeps a plain box for alignment.
    const thumbs = layerThumbs();
    expect(thumbs.map((t) => t.querySelector("img")?.getAttribute("src"))).toEqual([
      "data:image/png;base64,l2",
      "data:image/png;base64,l1",
      undefined,
    ]);
    expect(thumbs[2].classList.contains("is-plain")).toBe(true);
    expect(componentThumbs().map((t) => t.querySelector("img")?.getAttribute("src"))).toEqual([
      "data:image/png;base64,s",
      "data:image/png;base64,b",
    ]);
    // Decorative: the row's button keeps its name.
    expect(thumbs[0].getAttribute("aria-hidden")).toBe("true");
  });

  it("shows the kind's icon until a thumbnail is made, and pulses while an AI mask computes", () => {
    useMaskThumbnailStore.setState({ layers: { l1: "data:image/png;base64,l1" } });
    useProcessStore.setState({ activeAiTasks: { [subject.id]: { id: subject.id, type: "foreground" } } });
    render(<StudioLayers />);
    const [person, vignette] = layerThumbs();
    expect(person.querySelector("img")).toBeNull();
    expect(person.querySelector("svg")).not.toBeNull();
    expect(person.classList.contains("is-busy")).toBe(true);
    expect(vignette.classList.contains("is-busy")).toBe(false);
    const [subjectThumb, brushThumb] = componentThumbs();
    expect(subjectThumb.classList.contains("is-busy")).toBe(true);
    expect(brushThumb.classList.contains("is-busy")).toBe(false);
    expect(brushThumb.querySelector("svg")).not.toBeNull();
  });
});
