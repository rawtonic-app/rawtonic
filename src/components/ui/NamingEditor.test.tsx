import "../../i18n";
import { useState } from "react";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";
import NamingEditor from "./NamingEditor";
import type { NamingKind, NamingMode } from "../../utils/namingTemplates";

let current = "";
function Editor({
  initial = "{orig_base}",
  mode = "export",
  kind = "file",
}: {
  initial?: string;
  mode?: NamingMode;
  kind?: NamingKind;
}) {
  const [value, set] = useState(initial);
  const [sequenceStart, setSequenceStart] = useState(1);
  current = value;
  return (
    <NamingEditor
      mode={mode}
      kind={kind}
      label="Filename template"
      value={value}
      onChange={set}
      original="photo.RAF"
      job="Acme"
      sequenceStart={sequenceStart}
      onSequenceStartChange={kind === "file" ? setSequenceStart : undefined}
    />
  );
}
beforeEach(() => localStorage.clear());
afterEach(cleanup);
const field = () => screen.getByRole("group", { name: "Naming blocks" });
const chipNames = () =>
  within(field())
    .queryAllByRole("button")
    .filter((b) => b.classList.contains("naming-chip-label"))
    .map((b) => b.textContent);
const texts = () => within(field()).getAllByRole("textbox") as HTMLInputElement[];

it("shows tokens as chips with editable free text between them", () => {
  render(<Editor initial="Shoot_{date}-{seq:3}.x" />);
  expect(chipNames()).toEqual(["Date", "Counter"]);
  expect(texts().map((t) => t.value)).toEqual(["Shoot_", "-", ".x"]);
  fireEvent.change(texts()[1], { target: { value: " · " } });
  expect(current).toBe("Shoot_{date} · {seq:3}.x");
  expect(screen.getByText("Shoot_2026-10-11 · 001.x.jpg")).toBeTruthy();
});

it("adds a token by double-click or Enter at the text cursor, not by a single click", () => {
  render(<Editor initial="ab" />);
  fireEvent.click(screen.getByText("Edit naming format…"));
  const text = texts()[0];
  fireEvent.focus(text);
  text.setSelectionRange(1, 1);
  fireEvent.select(text);
  const year = screen.getByRole("button", { name: "Year" });
  fireEvent.click(year);
  expect(current).toBe("ab");
  fireEvent.doubleClick(year);
  expect(current).toBe("a{yyyy}b");
  fireEvent.keyDown(screen.getByRole("button", { name: "Lens" }), { key: "Enter" });
  expect(current).toContain("{lens}");
});

it("removes and moves chips from the keyboard", () => {
  render(<Editor initial="{yyyy}_{orig_base}" />);
  const [year] = within(field()).getAllByRole("button").filter((b) => b.classList.contains("naming-chip-label"));
  fireEvent.keyDown(year, { key: "ArrowRight", altKey: true });
  expect(current).toBe("{orig_base}_{yyyy}");
  // Backspace at the start of the text after a chip removes the chip.
  const after = texts()[2];
  after.setSelectionRange(0, 0);
  fireEvent.keyDown(after, { key: "Backspace" });
  expect(current).toBe("{orig_base}_");
  fireEvent.click(screen.getByRole("button", { name: "Remove Photo name" }));
  expect(current).toBe("_");
});

it("chooses counter digits and date formats on their chips", () => {
  render(<Editor initial="{date}_{seq}" />);
  fireEvent.change(within(field()).getByLabelText("Date format"), { target: { value: "dd.mm.yyyy" } });
  expect(current).toBe("{date:dd.mm.yyyy}_{seq}");
  fireEvent.change(within(field()).getByLabelText("Counter digits"), { target: { value: "4" } });
  expect(current).toBe("{date:dd.mm.yyyy}_{seq:4}");
  expect(screen.getByText("11.10.2026_0001.jpg")).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Counter starts at"), { target: { value: "42" } });
  expect(screen.getByText("11.10.2026_0042.jpg")).toBeTruthy();
});

it("shows the backend's name for the selected photo when it has one", () => {
  render(
    <NamingEditor
      mode="ingest"
      label="Filename template"
      value="{camera}_{seq:4}"
      onChange={() => {}}
      preview={{ name: "Nikon D750_0001.NEF", dateSource: "original", sidecar: false }}
    />,
  );
  expect(screen.getByText("Nikon D750_0001.NEF")).toBeTruthy();
  expect(screen.getByText("Date from the capture time (EXIF)")).toBeTruthy();
});

it("saves, renames and deletes presets, shared by import, rename and export", () => {
  const view = render(<Editor />);
  fireEvent.click(screen.getByText("Edit naming format…"));
  fireEvent.change(screen.getByLabelText("Filename template"), { target: { value: "Web_{orig_base}" } });
  fireEvent.change(screen.getByLabelText("Preset name"), { target: { value: "Website" } });
  fireEvent.click(screen.getByRole("button", { name: "Save preset" }));
  view.unmount();
  // The rename dialog offers it too.
  const rename = render(<Editor mode="rename" />);
  fireEvent.change(screen.getByLabelText("Naming preset"), { target: { value: "saved:0" } });
  expect(current).toBe("Web_{orig_base}");
  fireEvent.change(screen.getByLabelText("Preset name"), { target: { value: "Web delivery" } });
  fireEvent.click(screen.getByRole("button", { name: "Rename preset" }));
  expect(screen.getByRole("option", { name: "Web delivery" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Delete preset" }));
  expect(screen.queryByRole("option", { name: "Web delivery" })).toBeNull();
  rename.unmount();
});

it("hides a preset whose tokens a mode cannot fill", () => {
  localStorage.setItem("rawtonic.naming.presets", JSON.stringify([{ name: "Per format", template: "{orig_base}_{format}" }]));
  render(<Editor mode="ingest" />);
  expect(screen.queryByRole("option", { name: "Per format" })).toBeNull();
});

it("shows invalid tokens inline and prevents form submission", () => {
  render(<Editor />);
  const input = screen.getByLabelText("Filename template") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "{unknown}" } });
  expect(input.checkValidity()).toBe(false);
  expect(screen.getByRole("alert").textContent).toContain("available tokens");
  fireEvent.change(input, { target: { value: "{orig_base}" } });
  expect(input.checkValidity()).toBe(true);
});

it("edits a folder template with folder levels and no presets", () => {
  render(<Editor kind="folder" initial="{job}/{date}/{format}" />);
  expect(screen.queryByLabelText("Naming preset")).toBeNull();
  expect(screen.getByText("Acme/2026-10-11/JPEG")).toBeTruthy();
  fireEvent.click(screen.getByText("Folder tokens…"));
  fireEvent.doubleClick(screen.getByRole("button", { name: "Folder level (/)" }));
  expect(current).toBe("{job}/{date}/{format}/");
});
