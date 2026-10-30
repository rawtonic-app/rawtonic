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
  expect(shadows.querySelector("title")?.textContent).toContain("J");
  expect(highlights.querySelector("title")?.textContent).toContain("J");
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
  const group = screen.getByRole("group", { name: "Clipping warnings" });
  expect(within(group).getAllByRole("button")).toHaveLength(2);
  useEditorStore.setState({ softProof: false });
});
