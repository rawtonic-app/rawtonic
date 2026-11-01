import "../../i18n";
import { afterEach, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import StudioScopes, { scopeMode, scopePixels, waveformY } from "./StudioScopes";
import { useEditorStore } from "../../store/useEditorStore";
import type { WaveformData } from "../ui/AppProperties";

afterEach(() => {
  cleanup();
  useEditorStore.setState({ waveform: null, activeWaveformChannel: "luma" });
});

const data: WaveformData = {
  width: 256,
  height: 256,
  luma: btoa(String.fromCharCode(...new Uint8Array(256 * 4).fill(7)).repeat(256)),
  rgb: "",
  parade: "",
  vectorscope: "",
  vectorscopeTargets: [
    { name: "R", percent: 100, x: 104.9, y: 28 },
    { name: "R", percent: 75, x: 110.8, y: 53 },
  ],
  skinLineDegrees: 125.5,
};

it("graticule lines sit on the codes they name: 100 % on code 255, 0 % on code 0", () => {
  // Row r holds code 255 - r; its centre is r + 0.5.
  expect(waveformY(100)).toBe(0.5);
  expect(waveformY(0)).toBe(255.5);
  expect(waveformY(50)).toBe(128);
});

it("decodes a computed scope and nothing for a mode that was not computed", () => {
  const luma = scopePixels(data, "luma");
  expect(luma?.length).toBe(256 * 256 * 4);
  expect(luma?.[0]).toBe(7);
  expect(scopePixels(data, "rgb")).toBeNull();
  expect(scopePixels(null, "luma")).toBeNull();
  // A truncated image is never drawn.
  expect(scopePixels({ ...data, luma: btoa("short") }, "luma")).toBeNull();
  expect(scopeMode("parade")).toBe("parade");
  expect(scopeMode("bogus")).toBe("luma");
});

it("switches modes and draws the backend's vectorscope targets and skin line", () => {
  useEditorStore.setState({ waveform: data });
  const { container } = render(<StudioScopes />);
  expect(screen.getByRole("img", { name: "Luma waveform of the shown photo" })).toBeTruthy();
  expect(container.querySelectorAll("[data-percent]").length).toBe(5);
  fireEvent.click(screen.getByRole("button", { name: "Vector" }));
  expect(useEditorStore.getState().activeWaveformChannel).toBe("vectorscope");
  // Not computed yet: the plot says so instead of showing a stale mode.
  expect(screen.getByRole("img", { name: "Scopes available after opening a photo" })).toBeTruthy();
  const red = container.querySelector('[data-target="R100"]')!;
  expect(Number(red.getAttribute("x")) + Number(red.getAttribute("width")) / 2).toBeCloseTo(104.9);
  expect(container.querySelector('[data-target="R75"]')).not.toBeNull();
  const skin = container.querySelector(".studio-scope-skin")!;
  const angle = Math.atan2(
    128 - Number(skin.getAttribute("y2")),
    Number(skin.getAttribute("x2")) - 128,
  );
  expect((angle * 180) / Math.PI).toBeCloseTo(125.5);
});

it("keeps Space and Enter on a mode button away from the global shortcuts", () => {
  // useKeyboardShortcuts listens on window: Space there zooms the photo and
  // prevents the button's own activation.
  const reached: string[] = [];
  const shortcuts = (event: KeyboardEvent) => reached.push(event.key);
  window.addEventListener("keydown", shortcuts);
  try {
    render(<StudioScopes />);
    const vector = screen.getByRole("button", { name: "Vector" });
    for (const key of [" ", "Enter", "j"]) {
      expect(fireEvent.keyDown(vector, { key })).toBe(true);
    }
    expect(reached).toEqual(["j"]);
  } finally {
    window.removeEventListener("keydown", shortcuts);
  }
});
