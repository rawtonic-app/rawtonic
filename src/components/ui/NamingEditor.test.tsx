import "../../i18n";
import { useState } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";
import NamingEditor from "./NamingEditor";
function Editor() {
  const [value, set] = useState("{original_filename}");
  const [sequenceStart, setSequenceStart] = useState(1);
  return (
    <NamingEditor
      mode="export"
      label="Filename template"
      value={value}
      onChange={set}
      original="photo.RAF"
      sequenceStart={sequenceStart}
      onSequenceStartChange={setSequenceStart}
    />
  );
}
beforeEach(() => localStorage.clear());
afterEach(cleanup);
it("inserts at the cursor and lets keyboard users reorder and remove blocks", () => {
  render(<Editor />);
  fireEvent.click(screen.getByText("Edit naming format…"));
  const input = screen.getByLabelText("Filename template") as HTMLInputElement;
  input.setSelectionRange(0, 0);
  fireEvent.click(screen.getByRole("button", { name: "Year" }));
  expect(input.value).toBe("{YYYY}{original_filename}");
  const parts = screen.getByLabelText("Naming blocks");
  fireEvent.click(parts.querySelector("button")!);
  fireEvent.click(screen.getByRole("button", { name: "Move block right" }));
  expect(input.value).toBe("{original_filename}{YYYY}");
  fireEvent.click(screen.getByRole("button", { name: "Remove block" }));
  expect(input.value).toBe("{original_filename}");
});
it("saves naming presets and restores them in a later dialog", () => {
  const view = render(<Editor />);
  fireEvent.click(screen.getByText("Edit naming format…"));
  fireEvent.change(screen.getByLabelText("Filename template"), {
    target: { value: "Web_{original_filename}" },
  });
  fireEvent.change(screen.getByLabelText("Preset name"), {
    target: { value: "Website" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Save preset" }));
  view.unmount();
  render(<Editor />);
  fireEvent.change(screen.getByLabelText("Naming preset"), {
    target: { value: "saved:0" },
  });
  expect(
    (screen.getByLabelText("Filename template") as HTMLInputElement).value,
  ).toBe("Web_{original_filename}");
  expect(screen.getByText("Web_photo.jpg")).toBeTruthy();
});

it("shows invalid tokens inline and prevents form submission", () => {
  render(<Editor />);
  const input = screen.getByLabelText("Filename template") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "{unknown}" } });
  expect(input.checkValidity()).toBe(false);
  expect(screen.getByRole("alert").textContent).toContain("available tokens");
  fireEvent.change(input, { target: { value: "{original_filename}" } });
  expect(input.checkValidity()).toBe(true);
});

it("updates the counter preview and restores the saved start", () => {
  const view = render(<Editor />);
  fireEvent.change(screen.getByLabelText("Filename template"), { target: { value: "{sequence}" } });
  fireEvent.change(screen.getByLabelText("Counter starts at"), { target: { value: "42" } });
  expect(screen.getByText("42.jpg")).toBeTruthy();
  fireEvent.click(screen.getByText("Edit naming format…"));
  fireEvent.change(screen.getByLabelText("Preset name"), { target: { value: "Delivery" } });
  fireEvent.click(screen.getByRole("button", { name: "Save preset" }));
  view.unmount();
  render(<Editor />);
  fireEvent.change(screen.getByLabelText("Naming preset"), { target: { value: "saved:0" } });
  expect((screen.getByLabelText("Counter starts at") as HTMLInputElement).value).toBe("42");
  expect(screen.getByText("42.jpg")).toBeTruthy();
});
