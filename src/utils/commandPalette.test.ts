import { afterEach, expect, it, vi } from "vitest";
import {
  paletteCommands,
  rankCommands,
  overflowCommands,
} from "./commandPalette";
import { registerAppCommands } from "./appCommands";
import { KEYBIND_DEFINITIONS, buildComboMap } from "./keyboardUtils";

let stop = () => {};
afterEach(() => stop());
const t = (key: string | string[]) => (Array.isArray(key) ? key[0] : key);
it("searches fuzzy subsequences, prefers exact labels, then recent commands", () => {
  const commands = ["Export", "Export with Recipe", "Preferences"].map(
    (label) => ({ id: label, label, enabled: true }),
  );
  expect(rankCommands(commands, "xpr", [])[0].id).toBe("Export");
  expect(rankCommands(commands, "export", ["Export with Recipe"])[0].id).toBe(
    "Export",
  );
  expect(rankCommands(commands, "", ["Preferences"])[0].id).toBe("Preferences");
  expect(rankCommands(commands, "missing", [])).toEqual([]);
});
it("includes all registered commands and live labels with registry enabled state", () => {
  stop = registerAppCommands({
    "recipe:test": {
      run: vi.fn(),
      enabled: () => false,
      menu: { group: "recipes", label: "Small JPEG" },
    },
    custom: { run: vi.fn() },
  });
  expect(paletteCommands(t)).toContainEqual(
    expect.objectContaining({
      id: "recipe:test",
      label: "Small JPEG",
      enabled: false,
    }),
  );
  expect(paletteCommands(t).find((c) => c.id === "custom")).toBeTruthy();
});
it("generates five overflow submenus from the shared table only on Windows/Linux", () => {
  expect(overflowCommands("macos", t)).toEqual([]);
  for (const platform of ["linux", "windows"]) {
    const menus = overflowCommands(platform, t);
    expect(menus.map((m) => m.label)).toEqual(
      ["file", "edit", "photo", "view", "help"].map(
        (m) => `appMenu.menus.${m}`,
      ),
    );
    expect(
      menus[0].submenu?.some((o) => o.label === "appMenu.items.open_session"),
    ).toBe(true);
  }
});
it("offers a rebindable, unambiguous palette shortcut without displacing capture", () => {
  expect(buildComboMap({}).get("ctrl+shift+KeyP")).toEqual(["command_palette"]);
  expect(buildComboMap({}).get("ctrl+KeyK")).toEqual(["tether_capture"]);
  expect(
    KEYBIND_DEFINITIONS.find((d) => d.action === "command_palette"),
  ).toBeTruthy();
  expect(
    buildComboMap({ command_palette: ["ctrl", "Slash"] }).get("ctrl+Slash"),
  ).toEqual(["command_palette"]);
});
