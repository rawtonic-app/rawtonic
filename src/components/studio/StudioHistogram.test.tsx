import "../../i18n";
import { afterEach, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import StudioHistogram from "./StudioHistogram";
import { useEditorStore } from "../../store/useEditorStore";

afterEach(() => {
  cleanup();
  useEditorStore.setState({
    softProof: false,
    softProofSpace: "srgb",
    softProofWarning: false,
    histogram: null,
    clipHighlights: false,
    clipShadows: false,
  });
});

it("soft-proofs in a chosen colour space with an optional gamut warning", () => {
  render(<StudioHistogram />);
  expect(screen.queryByLabelText("Proof color space")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Proof" }));
  expect(useEditorStore.getState().softProof).toBe(true);
  const space = screen.getByLabelText("Proof color space") as HTMLSelectElement;
  expect(space.value).toBe("srgb");
  // Enabling Proof inserts its controls below the caption, so its toggle
  // stays under the pointer for the next click. The header names no control,
  // so it is found by its class.
  const header = document.querySelector<HTMLElement>(".studio-clip-corners");
  expect(header).not.toBeNull();
  expect(header!.nextElementSibling).toBe(space.parentElement);
  expect(space.parentElement?.nextElementSibling?.className).toBe("studio-histogram-plot");
  fireEvent.change(space, { target: { value: "adobeRgb" } });
  expect(useEditorStore.getState().softProofSpace).toBe("adobeRgb");
  fireEvent.click(screen.getByRole("button", { name: "Gamut warning" }));
  expect(useEditorStore.getState().softProofWarning).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Proof" }));
  expect(useEditorStore.getState().softProof).toBe(false);
  expect(screen.queryByLabelText("Proof color space")).toBeNull();
});

/** The backend's histogram: if `lit`, a photo that clips `shadows` /
 * `highlights`, as the engine counts it. */
const clippedHistogram = (shadows: boolean, highlights: boolean) => {
  const bins = [0, 0, 0, 0, 5, 2, 0, 0, 0, 0];
  return {
    red: bins,
    green: [...bins],
    blue: [...bins],
    clippedShadows: shadows,
    clippedHighlights: highlights,
  };
};

it("switches each clipping warning on its own with the histogram corners", () => {
  useEditorStore.setState({
    histogram: null,
    clipHighlights: false,
    clipShadows: false,
    softProof: false,
  });
  render(<StudioHistogram />);
  // No text button any more.
  expect(screen.queryByRole("button", { name: "Clipping" })).toBeNull();
  const shadows = screen.getByRole("button", { name: "Shadow clipping" });
  const highlights = screen.getByRole("button", { name: "Highlight clipping" });
  expect(shadows.getAttribute("aria-pressed")).toBe("false");
  expect(highlights.getAttribute("aria-pressed")).toBe("false");
  expect(shadows.getAttribute("title")).toContain("J");
  expect(shadows.getAttribute("title")).toContain("blue in the photo");
  expect(highlights.getAttribute("title")).toContain("red in the photo");
  expect(highlights.getAttribute("title")).toContain("J");
  fireEvent.click(shadows);
  expect(useEditorStore.getState().clipShadows).toBe(true);
  expect(useEditorStore.getState().clipHighlights).toBe(false);
  expect(shadows.getAttribute("aria-pressed")).toBe("true");
  expect(highlights.getAttribute("aria-pressed")).toBe("false");
  fireEvent.click(highlights);
  expect(useEditorStore.getState().clipHighlights).toBe(true);
  expect(useEditorStore.getState().clipShadows).toBe(true);
  fireEvent.click(shadows);
  expect(useEditorStore.getState().clipShadows).toBe(false);
  expect(useEditorStore.getState().clipHighlights).toBe(true);
});

it("lights the corners from the histogram even with the warnings off", () => {
  useEditorStore.setState({
    histogram: clippedHistogram(false, true) as never,
    clipHighlights: false,
    clipShadows: false,
  });
  render(<StudioHistogram />);
  const shadows = screen.getByRole("button", { name: "Shadow clipping" });
  const highlights = screen.getByRole("button", { name: "Highlight clipping, clipped" });
  expect(shadows.hasAttribute("data-clipping")).toBe(false);
  expect(highlights.hasAttribute("data-clipping")).toBe(true);
  expect(highlights.getAttribute("aria-pressed")).toBe("false");
  expect(highlights.querySelector(".studio-clip-corner-mark")).not.toBeNull();
  cleanup();
  useEditorStore.setState({ histogram: clippedHistogram(true, false) as never });
  render(<StudioHistogram />);
  expect(
    screen.getByRole("button", { name: "Shadow clipping, clipped" }).hasAttribute("data-clipping"),
  ).toBe(true);
  expect(
    screen.getByRole("button", { name: "Highlight clipping" }).hasAttribute("data-clipping"),
  ).toBe(false);
  useEditorStore.setState({ histogram: null, clipHighlights: false, clipShadows: false });
});

it("follows the backend's clipping flags, not the bins' ends", () => {
  // Bins that fill both ends (as a smoothed histogram with values near an end
  // does) but no pixel at either end: neither corner lights.
  const nearEnds = {
    red: [0.4, 0.6, 1, 1, 0.6, 0.2, 0, 0, 0.2, 0.7],
    green: [0.5, 0.3, 0, 0, 0, 0, 0, 0, 0, 0.9],
    blue: [1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    clippedShadows: false,
    clippedHighlights: false,
  };
  useEditorStore.setState({ histogram: nearEnds as never });
  render(<StudioHistogram />);
  expect(
    screen.getByRole("button", { name: "Shadow clipping" }).hasAttribute("data-clipping"),
  ).toBe(false);
  expect(
    screen.getByRole("button", { name: "Highlight clipping" }).hasAttribute("data-clipping"),
  ).toBe(false);
  // Empty ends, yet the backend counts a clipped pixel: the side lights.
  cleanup();
  useEditorStore.setState({
    histogram: {
      red: [0, 0, 5],
      green: [0, 0, 5],
      blue: [0, 0, 5],
      clippedHighlights: true,
    } as never,
  });
  render(<StudioHistogram />);
  expect(
    screen.getByRole("button", { name: "Highlight clipping, clipped" }).hasAttribute(
      "data-clipping",
    ),
  ).toBe(true);
  expect(
    screen.getByRole("button", { name: "Shadow clipping" }).hasAttribute("data-clipping"),
  ).toBe(false);
});

it("keeps the lit state when a warning switches for the same histogram", () => {
  useEditorStore.setState({
    histogram: clippedHistogram(true, true) as never,
    clipHighlights: false,
    clipShadows: false,
  });
  render(<StudioHistogram />);
  const lit = () =>
    [
      screen.getByRole("button", { name: /^Shadow clipping/ }),
      screen.getByRole("button", { name: /^Highlight clipping/ }),
    ].map((corner) => [corner.hasAttribute("data-clipping"), corner.getAttribute("aria-pressed")]);
  const before = lit();
  expect(before).toEqual([
    [true, "false"],
    [true, "false"],
  ]);
  fireEvent.click(screen.getByRole("button", { name: /^Shadow clipping/ }));
  expect(useEditorStore.getState()).toMatchObject({ clipShadows: true, clipHighlights: false });
  // The warning's state changed; the clipping verdict did not.
  expect(lit()).toEqual([
    [true, "true"],
    [true, "false"],
  ]);
  fireEvent.click(screen.getByRole("button", { name: /^Highlight clipping/ }));
  expect(useEditorStore.getState()).toMatchObject({ clipShadows: true, clipHighlights: true });
  expect(lit()).toEqual([
    [true, "true"],
    [true, "true"],
  ]);
});

it("keeps the corner buttons off the plot's own image role", () => {
  useEditorStore.setState({ histogram: null, softProof: true });
  render(<StudioHistogram />);
  // The photo histogram section names the corners' home; the triangles carry
  // no second group label of their own.
  const region = screen.getByRole("region", { name: "Photo histogram" });
  expect(
    within(region).getAllByRole("button", { name: /clipping/ }),
  ).toHaveLength(2);
  expect(screen.queryByRole("group", { name: "Photo histogram" })).toBeNull();
  useEditorStore.setState({ softProof: false });
});

it("keeps fixed-size transparent control icons above the plot (studio.css applied)", async () => {
  // Vitest hands CSS imports (also `?raw`) over empty, so read the file: the
  // tests run on Node from the repository root, but the frontend tsconfig has
  // no Node types.
  // @ts-expect-error node:fs is untyped in the frontend tsconfig
  const { readFileSync } = await import("node:fs");
  const style = document.createElement("style");
  style.textContent = readFileSync(
    "src/components/studio/studio.css",
    "utf8",
  ) as string;
  document.head.appendChild(style);
  try {
    const { container } = render(
      <div className="studio-workspace">
        <StudioHistogram />
      </div>,
    );
    const plot = container.querySelector(
      ".studio-histogram-plot > svg",
    ) as SVGElement;
    const group = container.querySelector(".studio-clip-corners")!;
    const corners = group.querySelectorAll("button.studio-clip-corner > svg");
    expect(group.nextElementSibling).toBe(plot.parentElement);
    expect(plot.parentElement?.contains(group)).toBe(false);
    expect(corners).toHaveLength(2);
    // Only the graph owns a background and adaptive height. Icons never stretch.
    expect(getComputedStyle(plot).backgroundColor).not.toMatch(
      /transparent|rgba\(0, 0, 0, 0\)/,
    );
    expect(getComputedStyle(plot).height).toBe("82px");
    const shadow = group.querySelector(".studio-clip-shadows")!;
    const mark = shadow.querySelector("polygon")!;
    // Assert the visual state contract with the real stylesheet, including
    // the studio's general pressed-button styles.
    for (const [state, fill, stroke] of [
      ["", "rgba(0, 0, 0, 0)", "rgb(150, 154, 159)"],
      ["is-on", "rgba(0, 0, 0, 0)", "rgb(238, 240, 243)"],
      ["is-lit", "rgb(0, 0, 0)", "rgb(238, 240, 243)"],
      ["is-on is-lit", "rgb(0, 0, 0)", "rgb(238, 240, 243)"],
    ]) {
      shadow.setAttribute(
        "class",
        `studio-clip-corner studio-clip-shadows ${state}`,
      );
      expect(getComputedStyle(mark).fill).toBe(fill);
      expect(getComputedStyle(mark).stroke).toBe(stroke);
    }

    const highlight = group.querySelector(".studio-clip-highlights")!;
    const highlightMark = highlight.querySelector("polygon")!;
    for (const [state, fill] of [
      ["", "rgba(0, 0, 0, 0)"],
      ["is-on", "rgba(0, 0, 0, 0)"],
      ["is-lit", "rgb(255, 255, 255)"],
      ["is-on is-lit", "rgb(255, 255, 255)"],
    ]) {
      highlight.setAttribute("class", `studio-clip-corner studio-clip-highlights ${state}`);
      expect(getComputedStyle(highlightMark).fill).toBe(fill);
    }
    // The warning's on state is the box, independent of the glyph's fill.
    for (const [state, background, frame] of [
      ["", "rgba(0, 0, 0, 0)", false],
      ["is-lit", "rgba(0, 0, 0, 0)", false],
      ["is-on", "rgba(255, 255, 255, 0.12)", true],
      ["is-on is-lit", "rgba(255, 255, 255, 0.12)", true],
    ] as const) {
      shadow.setAttribute("class", `studio-clip-corner studio-clip-shadows ${state}`);
      expect(getComputedStyle(shadow).backgroundColor).toBe(background);
      expect(getComputedStyle(shadow).boxShadow.includes("inset")).toBe(frame);
    }
    // Glyphs sit centred in their boxes; the boxes reach past the plot edge.
    expect(getComputedStyle(shadow).justifyContent).toBe("center");
    expect(getComputedStyle(highlight).justifyContent).toBe("center");
    expect(getComputedStyle(shadow).marginLeft).toBe("-4.5px");
    expect(getComputedStyle(highlight).marginRight).toBe("-4.5px");
    for (const layer of corners) {
      expect(getComputedStyle(layer).backgroundColor).toMatch(
        /transparent|rgba\(0, 0, 0, 0\)/,
      );
      expect(getComputedStyle(layer).height).toBe("11px");
      expect(getComputedStyle(layer).width).toBe("11px");
      expect(getComputedStyle(layer.parentElement!).height).toBe("20px");
      expect(getComputedStyle(layer.parentElement!).width).toBe("20px");
    }
  } finally {
    style.remove();
  }
});

it("expresses independent warning and clipping states as control classes", () => {
  useEditorStore.setState({
    histogram: clippedHistogram(false, true) as never,
    clipShadows: true,
    clipHighlights: false,
  });
  render(<StudioHistogram />);
  const shadows = screen.getByRole("button", { name: "Shadow clipping" });
  const highlights = screen.getByRole("button", {
    name: "Highlight clipping, clipped",
  });
  expect(shadows.tagName).toBe("BUTTON");
  expect(shadows.classList.contains("is-on")).toBe(true);
  expect(shadows.classList.contains("is-lit")).toBe(false);
  expect(highlights.classList.contains("is-on")).toBe(false);
  expect(highlights.classList.contains("is-lit")).toBe(true);
  fireEvent.click(highlights);
  expect(highlights.classList.contains("is-on")).toBe(true);
  expect(highlights.classList.contains("is-lit")).toBe(true);
  fireEvent.click(shadows);
  expect(shadows.classList.contains("is-on")).toBe(false);
  expect(shadows.classList.contains("is-lit")).toBe(false);
});

it("reserves native activation keys while leaving J to the app shortcut", () => {
  const keys: string[] = [];
  render(<div onKeyDown={(event) => keys.push(event.key)}><StudioHistogram /></div>);
  const shadows = screen.getByRole("button", { name: "Shadow clipping" });
  for (const key of [" ", "Enter", "j"]) fireEvent.keyDown(shadows, { key });
  expect(keys).toEqual(["j"]);
});

it("writes the capture settings under the histogram with their units, as the viewer bar does", () => {
  // The backend gives the focal length as a bare number ("12"); the
  // histogram line printed it as "12", the bar under the photo "12 mm".
  useEditorStore.setState({
    selectedImage: {
      path: "/scratch/a.orf",
      isRaw: true,
      width: 5184,
      height: 3888,
      exif: { ISO: "1600", FNumber: "f/7.1", ExposureTime: "1/60 s", FocalLength: "12" },
    } as never,
  });
  try {
    render(<StudioHistogram />);
    const line = document.querySelector<HTMLElement>(".studio-exif-line")!;
    const parts = [...line.querySelectorAll("span")].map((s) => s.textContent);
    expect(parts).toEqual(["ISO 1600", "f/7.1", "1/60 s", "12 mm"]);
  } finally {
    useEditorStore.setState({ selectedImage: null });
  }
});
