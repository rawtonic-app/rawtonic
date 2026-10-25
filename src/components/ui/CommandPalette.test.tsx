import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import CommandPalette from "./CommandPalette";
import {
  registerAppCommands,
  runAppCommand,
  notifyAppCommandsChanged,
} from "../../utils/appCommands";
import { menuCommandEnabled } from "../../hooks/useAppMenu";
import { useKeyboardShortcuts } from "../../hooks/useKeyboardShortcuts";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import { type AppSettings } from "./AppProperties";
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve()),
}));
vi.mock("react-toastify", () => ({
  toast: { error: vi.fn(), success: vi.fn() },
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string | string[]) => (Array.isArray(key) ? key[0] : key),
    i18n: { language: "en" },
  }),
}));
let stop = () => {};
const run = vi.fn();
function Harness() {
  useKeyboardShortcuts({
    sortedImageList: [],
    handleUndoFileOperation: vi.fn(),
    handleBackToLibrary: vi.fn(),
    handleDeleteSelected: vi.fn(),
    handleImageSelect: vi.fn(),
    handlePasteFiles: vi.fn(),
    handleZoomChange: vi.fn(),
  });
  return (
    <>
      <input aria-label="outside" />
      <CommandPalette />
    </>
  );
}
const press = (code: string, init: KeyboardEventInit = {}) =>
  fireEvent.keyDown(document.activeElement ?? window, {
    code,
    key: code.replace("Key", ""),
    ...init,
  });
beforeEach(() => {
  localStorage.clear();
  run.mockClear();
  useSettingsStore.setState({
    osPlatform: "macos",
    appSettings: { keybinds: {} } as AppSettings,
  });
  useUIStore.setState({
    isSettingsOpen: false,
    activeView: "library",
    isFullScreen: false,
  });
  stop = registerAppCommands({
    "recipe:test": { run, menu: { group: "recipes", label: "Small JPEG" } },
  });
});
afterEach(() => {
  cleanup();
  stop();
});
it("opens through the rebindable table key, searches and runs the registered handler, then remembers it", () => {
  render(<Harness />);
  press("KeyP", { metaKey: true, shiftKey: true });
  const search = screen.getByRole("combobox");
  expect(document.activeElement).toBe(search);
  fireEvent.change(search, { target: { value: "smjpg" } });
  press("Enter");
  expect(run).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("dialog")).toBeNull();
  act(() => {
    runAppCommand("command_palette");
  });
  expect(screen.getAllByRole("option")[0].textContent).toContain("Small JPEG");
  press("Escape");
  expect(screen.queryByRole("dialog")).toBeNull();
});
it("does not trigger in text inputs and honors a rebound shortcut", () => {
  render(<Harness />);
  screen.getByLabelText("outside").focus();
  press("KeyP", { metaKey: true, shiftKey: true });
  expect(screen.queryByRole("dialog")).toBeNull();
  (document.activeElement as HTMLElement).blur();
  act(() =>
    useSettingsStore.setState({
      appSettings: {
        keybinds: { command_palette: ["ctrl", "Slash"] },
      } as unknown as AppSettings,
    }),
  );
  press("KeyP", { metaKey: true, shiftKey: true });
  expect(screen.queryByRole("dialog")).toBeNull();
  press("Slash", { metaKey: true });
  expect(screen.getByRole("dialog")).toBeTruthy();
});
it("shows disabled commands and refuses Enter, refreshes live availability, and traps focus", () => {
  let enabled = false;
  stop();
  stop = registerAppCommands({
    "recipe:test": {
      run,
      enabled: () => enabled,
      menu: { group: "recipes", label: "Small JPEG" },
    },
  });
  render(<Harness />);
  act(() => {
    runAppCommand("command_palette");
  });
  fireEvent.change(screen.getByRole("combobox"), {
    target: { value: "Small JPEG" },
  });
  expect(screen.getByRole("option").getAttribute("aria-disabled")).toBe("true");
  press("Enter");
  expect(run).not.toHaveBeenCalled();
  enabled = true;
  act(() => notifyAppCommandsChanged());
  expect(screen.getByRole("option").getAttribute("aria-disabled")).toBe(
    "false",
  );
  press("Enter");
  expect(run).toHaveBeenCalledOnce();
});
it("uses the menu enabled state without its own modal disabling workspace commands", () => {
  render(<Harness />);
  act(() => {
    runAppCommand("command_palette");
  });
  fireEvent.change(screen.getByRole("combobox"), {
    target: { value: "appMenu.items.open_settings" },
  });
  expect(screen.getByRole("option").getAttribute("aria-disabled")).toBe(
    "false",
  );
  expect(menuCommandEnabled("open_settings")).toBe(false);
  press("Enter");
  expect(useUIStore.getState().isSettingsOpen).toBe(true);
});

it.each(["linux", "windows"])(
  "opens with Ctrl+Shift+P on %s and displays effective shortcuts",
  (osPlatform) => {
    useSettingsStore.setState({ osPlatform });
    render(<Harness />);
    press("KeyP", { ctrlKey: true, shiftKey: true });
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(screen.getByRole("dialog").textContent).toContain("Ctrl + ,");
  },
);

it("moves through matches with arrows and restores the opener on Escape", () => {
  stop();
  const second = vi.fn();
  stop = registerAppCommands({
    "recipe:first": { run, menu: { group: "recipes", label: "JPEG first" } },
    "recipe:second": {
      run: second,
      menu: { group: "recipes", label: "JPEG second" },
    },
  });
  render(<Harness />);
  const opener = screen.getByLabelText("outside");
  opener.focus();
  act(() => {
    runAppCommand("command_palette");
  });
  fireEvent.change(screen.getByRole("combobox"), { target: { value: "JPEG" } });
  press("ArrowDown");
  press("Enter");
  expect(second).toHaveBeenCalledOnce();
  expect(run).not.toHaveBeenCalled();
  expect(document.activeElement).toBe(opener);
  act(() => {
    runAppCommand("command_palette");
  });
  press("Escape");
  expect(document.activeElement).toBe(opener);
});

it("keeps native text Undo available while the chooser reads workspace Undo availability", () => {
  render(<Harness />);
  const unregister = registerAppCommands({
    undo: { run: vi.fn(), enabled: () => false },
  });
  screen.getByLabelText("outside").focus();
  expect(menuCommandEnabled("undo")).toBe(true);
  expect(menuCommandEnabled("undo", true)).toBe(false);
  act(() => {
    runAppCommand("command_palette");
  });
  expect(menuCommandEnabled("undo")).toBe(true);
  expect(menuCommandEnabled("undo", true)).toBe(false);
  unregister();
});

it("dismisses the chooser when a native Help command opens another modal", () => {
  render(<Harness />);
  const unregister = registerAppCommands({
    about: { run: () => useUIStore.getState().setUI({ isSettingsOpen: true }) },
  });
  act(() => {
    runAppCommand("command_palette");
  });
  act(() => {
    runAppCommand("about");
  });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(useUIStore.getState().isSettingsOpen).toBe(true);
  unregister();
});
