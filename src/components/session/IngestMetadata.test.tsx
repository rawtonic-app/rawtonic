import "../../i18n";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import IngestMetadata, { type ImportMetadata } from "./IngestMetadata";
import { useSettingsStore } from "../../store/useSettingsStore";
import type { AppSettings } from "../ui/AppProperties";

import { invoke } from "@tauri-apps/api/core";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => {}) }));
const save = vi.mocked(invoke);
beforeEach(() => {
  save.mockClear();
  useSettingsStore.setState({ appSettings: {} as AppSettings });
});
afterEach(cleanup);
function Form() {
  const [value, setValue] = useState<ImportMetadata | null>(null);
  return <><IngestMetadata value={value} onChange={setValue} /><output data-testid="value">{JSON.stringify(value)}</output></>;
}
it("starts at None and creates, edits, selects and deletes saved presets", async () => {
  const user = userEvent.setup();
  render(<Form />);
  expect((screen.getByLabelText("Metadata preset") as HTMLSelectElement).value).toBe("");
  expect(screen.getByTestId("value").textContent).toBe("null");
  await user.selectOptions(screen.getByLabelText("Metadata preset"), "new");
  expect((screen.getByLabelText("Include creator contact details") as HTMLInputElement).checked).toBe(false);
  expect(screen.queryByLabelText("Email")).toBeNull();
  await user.type(screen.getByLabelText("Preset name"), "My rights");
  await user.type(screen.getByLabelText("Creator"), "Photographer");
  await user.click(screen.getByLabelText("Copyright notice"));
  await user.paste("Copyright {year}");
  await user.type(screen.getByLabelText("Keywords (comma separated)"), "harbour, sea, harbour");
  await user.click(screen.getByRole("button", { name: "Save preset" }));
  await waitFor(() => expect(save).toHaveBeenCalledOnce());
  expect(useSettingsStore.getState().appSettings?.importMetadataPresets).toEqual([expect.objectContaining({ name: "My rights", creator: "Photographer", includeCreatorContact: false, overwrite: [], keywords: ["harbour", "sea"] })]);
  await user.click(screen.getByLabelText("Overwrite Creator"));
  await user.click(screen.getByLabelText("Include creator contact details"));
  await user.type(screen.getByLabelText("Email"), "creator@example.org");
  await user.click(screen.getByRole("button", { name: "Save preset" }));
  expect(useSettingsStore.getState().appSettings?.importMetadataPresets).toHaveLength(1);
  await user.selectOptions(screen.getByLabelText("Metadata preset"), "");
  expect(screen.getByTestId("value").textContent).toBe("null");
  const preset = useSettingsStore.getState().appSettings!.importMetadataPresets![0];
  await user.selectOptions(screen.getByLabelText("Metadata preset"), preset.id);
  expect((screen.getByLabelText("Creator") as HTMLInputElement).value).toBe("Photographer");
  expect((screen.getByLabelText("Overwrite Creator") as HTMLInputElement).checked).toBe(true);
  await user.click(screen.getByRole("button", { name: "Delete preset" }));
  expect(useSettingsStore.getState().appSettings?.importMetadataPresets).toEqual([]);
  expect(screen.getByTestId("value").textContent).toBe("null");
});

it("reports a settings-write failure and leaves saved presets unchanged", async () => {
  const user = userEvent.setup();
  render(<Form />);
  await user.selectOptions(screen.getByLabelText("Metadata preset"), "new");
  await user.type(screen.getByLabelText("Preset name"), "Rights");
  save.mockRejectedValueOnce(new Error("Disk full"));
  await user.click(screen.getByRole("button", { name: "Save preset" }));
  expect((await screen.findByRole("alert")).textContent).toContain("Disk full");
  expect(useSettingsStore.getState().appSettings?.importMetadataPresets).toBeUndefined();
});

it("keeps a saved preset in its picker position when edited", async () => {
  const user = userEvent.setup();
  const preset = { creator: "", copyright: "", description: "", iptc: {}, keywords: [], overwrite: [], includeCreatorContact: false };
  useSettingsStore.setState({ appSettings: { ...useSettingsStore.getState().appSettings!, importMetadataPresets: [
    { ...preset, id: "first", name: "First" }, { ...preset, id: "second", name: "Second" },
  ] } });
  render(<Form />);
  await user.selectOptions(screen.getByLabelText("Metadata preset"), "first");
  await user.type(screen.getByLabelText("Creator"), "Creator");
  await user.click(screen.getByRole("button", { name: "Save preset" }));
  expect(useSettingsStore.getState().appSettings?.importMetadataPresets?.map((p) => p.id)).toEqual(["first", "second"]);
});
