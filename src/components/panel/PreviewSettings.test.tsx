import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import PreviewSettings from "./PreviewSettings";
import { type AppSettings } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

beforeEach(() => {
  vi.mocked(invoke).mockResolvedValue(0);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("keeps preview construction controls and links storage management to its single page", async () => {
  const onOpenStorage = vi.fn();
  render(
    <PreviewSettings
      appSettings={{} as AppSettings}
      onSettingsChange={vi.fn()}
      onOpenStorage={onOpenStorage}
    />,
  );
  await act(async () => {});
  expect(screen.getByText("previews.settings.standardSize")).toBeTruthy();
  expect(screen.getByText("previews.settings.discardFull")).toBeTruthy();
  expect(screen.queryByText("previews.settings.standardLimit")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "storage.manage" }));
  expect(onOpenStorage).toHaveBeenCalledOnce();
});
