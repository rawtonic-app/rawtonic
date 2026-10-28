import "../../i18n";
import { afterEach, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import StudioHistogram from "./StudioHistogram";
import { useEditorStore } from "../../store/useEditorStore";

afterEach(() => {
  cleanup();
  useEditorStore.setState({
    softProof: false,
    softProofSpace: "srgb",
    softProofWarning: false,
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
