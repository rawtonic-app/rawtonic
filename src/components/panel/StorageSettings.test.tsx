import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import StorageSettings from "./StorageSettings";
import { type AppSettings, Invokes } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
const entries = [
  {
    category: "thumbnails",
    bytes: 1234,
    files: 2,
    path: "/cache/thumbnails",
    clearable: true,
  },
  {
    category: "models",
    bytes: 2048,
    files: 3,
    path: "/data/models",
    clearable: true,
  },
  {
    category: "systemModels",
    bytes: null,
    files: 0,
    path: "/system/models",
    clearable: false,
    error: true,
  },
];
beforeEach(() => {
  vi.mocked(invoke).mockImplementation((command) =>
    Promise.resolve(command === Invokes.StorageUsage ? entries : undefined),
  );
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("shows sizes and owned locations; system caches have no removal action", async () => {
  render(
    <StorageSettings
      appSettings={{} as AppSettings}
      onSettingsChange={vi.fn()}
      onLibraryRefresh={vi.fn()}
    />,
  );
  await screen.findByText("/cache/thumbnails");
  expect(screen.getByText("/system/models")).toBeTruthy();
  expect(screen.getByText("storage.unavailable")).toBeTruthy();
  expect(screen.getAllByRole("button", { name: "storage.clear" })).toHaveLength(
    1,
  );
});

it("cancels a pending scan when leaving the page", async () => {
  vi.mocked(invoke).mockImplementation((command) =>
    command === Invokes.StorageUsage
      ? new Promise(() => {})
      : Promise.resolve(),
  );
  const view = render(
    <StorageSettings
      appSettings={{} as AppSettings}
      onSettingsChange={vi.fn()}
      onLibraryRefresh={vi.fn()}
    />,
  );
  const scan = vi
    .mocked(invoke)
    .mock.calls.find(([command]) => command === Invokes.StorageUsage);
  view.unmount();
  expect(invoke).toHaveBeenCalledWith(Invokes.StorageCancelScan, scan?.[1]);
});

it("asks before scheduling model removal and explains the next launch", async () => {
  render(
    <StorageSettings
      appSettings={{} as AppSettings}
      onSettingsChange={vi.fn()}
      onLibraryRefresh={vi.fn()}
    />,
  );
  fireEvent.click(
    await screen.findByRole("button", { name: "storage.removeModels" }),
  );
  expect(screen.getByText("storage.modelsConfirm")).toBeTruthy();
  expect(invoke).not.toHaveBeenCalledWith(
    Invokes.StorageClear,
    expect.anything(),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "storage.confirmRemove" }),
  );
  await screen.findByText("storage.modelsPending");
  expect(invoke).toHaveBeenCalledWith(Invokes.StorageClear, {
    category: "models",
    confirmed: true,
  });
});

it("shows a clear failure and retains the inventory", async () => {
  vi.mocked(invoke).mockImplementation((command) =>
    command === Invokes.StorageClear
      ? Promise.reject(new Error("refused"))
      : Promise.resolve(command === Invokes.StorageUsage ? entries : undefined),
  );
  render(
    <StorageSettings
      appSettings={{} as AppSettings}
      onSettingsChange={vi.fn()}
      onLibraryRefresh={vi.fn()}
    />,
  );
  fireEvent.click(await screen.findByRole("button", { name: "storage.clear" }));
  await screen.findByRole("alert");
  expect(screen.getByText("/cache/thumbnails")).toBeTruthy();
});

it("applies a saved limit immediately and reports a persistence failure", async () => {
  const save = vi
    .fn()
    .mockRejectedValueOnce(new Error("disk full"))
    .mockResolvedValue(undefined);
  render(
    <StorageSettings
      appSettings={{} as AppSettings}
      onSettingsChange={save}
      onLibraryRefresh={vi.fn()}
    />,
  );
  await act(async () => {});
  fireEvent.change(screen.getByLabelText("storage.thumbnailLimit"), {
    target: { value: "5" },
  });
  await screen.findByRole("alert");
  expect(invoke).not.toHaveBeenCalledWith(Invokes.StorageApplyLimits);
  fireEvent.change(screen.getByLabelText("storage.thumbnailLimit"), {
    target: { value: "10" },
  });
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith(Invokes.StorageApplyLimits),
  );
  expect(save).toHaveBeenLastCalledWith({ thumbnailCacheLimitGb: 10 });
});

it("restores a scheduled model removal after returning to Storage", async () => {
  vi.mocked(invoke).mockImplementation((command) =>
    Promise.resolve(
      command === Invokes.StorageUsage
        ? entries.map((entry) => ({
            ...entry,
            pendingRemoval: entry.category === "models",
          }))
        : undefined,
    ),
  );
  render(
    <StorageSettings
      appSettings={{} as AppSettings}
      onSettingsChange={vi.fn()}
      onLibraryRefresh={vi.fn()}
    />,
  );
  await screen.findByText("storage.modelsPending");
  expect(
    (
      screen.getByRole("button", {
        name: "storage.removeModels",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
});
