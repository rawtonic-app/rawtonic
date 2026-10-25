import { type Option, OPTION_SEPARATOR } from "../components/ui/AppProperties";
import {
  appCommandEnabled,
  registeredAppCommands,
  runAppCommand,
} from "./appCommands";
import {
  buildMenuSpec,
  MENU_ID_PREFIX,
  MENU_LAYOUT,
  type MenuNode,
  type Translate,
} from "./appMenu";
import { KEYBIND_DEFINITIONS, shortcutHint } from "./keyboardUtils";

export interface PaletteCommand {
  id: string;
  label: string;
  enabled: boolean;
  shortcut?: string | null;
}

export function paletteCommands(
  t: Translate,
  keybinds?: Record<string, string[]> | null,
  platform = "macos",
  enabled = appCommandEnabled,
): PaletteCommand[] {
  const labels = new Map<string, string>();
  const walk = (nodes: MenuNode[]) =>
    nodes.forEach((node) => {
      if (node.kind === "item")
        labels.set(node.id.slice(MENU_ID_PREFIX.length), node.label);
      if (node.kind === "submenu") walk(node.items);
    });
  walk(buildMenuSpec(t, keybinds, enabled).spec.menus);
  for (const [id, command] of registeredAppCommands()) {
    if (!labels.has(id)) {
      const def = KEYBIND_DEFINITIONS.find((def) => def.action === id);
      labels.set(
        id,
        command.menu?.label ??
          t(
            def
              ? [`appMenu.items.${id}`, def.description]
              : `appMenu.items.${id}`,
            { defaultValue: id },
          ),
      );
    }
  }
  return [...labels].map(([id, label]) => ({
    id,
    label,
    enabled: enabled(id),
    shortcut: shortcutHint(id, keybinds, platform),
  }));
}

const normalize = (text: string) =>
  text.normalize("NFD").replace(/\p{M}/gu, "").toLocaleLowerCase();
/** Subsequence matching rewards contiguous letters and word starts; exact labels win. */
export function rankCommands(
  commands: PaletteCommand[],
  query: string,
  recent: string[],
): PaletteCommand[] {
  const needle = normalize(query.trim());
  const score = (label: string) => {
    const haystack = normalize(label);
    if (!needle) return 0;
    if (haystack === needle) return 10000;
    let position = -1;
    let value = 0;
    for (const letter of needle) {
      const next = haystack.indexOf(letter, position + 1);
      if (next < 0) return -1;
      value += next === position + 1 ? 20 : 1;
      if (next === 0 || /\s|[›:/_-]/u.test(haystack[next - 1])) value += 10;
      position = next;
    }
    return value - haystack.length / 100;
  };
  const recency = (id: string) =>
    recent.includes(id) ? recent.length - recent.indexOf(id) : 0;
  return commands
    .map((command, index) => ({ command, index, score: score(command.label) }))
    .filter((row) => row.score >= 0)
    .sort(
      (a, b) =>
        b.score - a.score ||
        recency(b.command.id) - recency(a.command.id) ||
        a.index - b.index,
    )
    .map((row) => row.command);
}

/** The same tree as the native menu; app-only commands join Edit/Help. */
export function overflowCommands(
  platform: string,
  t: Translate,
  keybinds?: Record<string, string[]> | null,
  enabled = appCommandEnabled,
): Option[] {
  if (platform !== "linux" && platform !== "windows") return [];
  const built = buildMenuSpec(t, keybinds, enabled).spec.menus;
  const convert = (nodes: MenuNode[]): Option[] =>
    nodes
      .flatMap((node): Option[] => {
        if (node.kind === "native") return [];
        if (node.kind === "separator") return [{ type: OPTION_SEPARATOR }];
        if (node.kind === "submenu")
          return [{ label: node.label, submenu: convert(node.items) }];
        const command = node.id.slice(MENU_ID_PREFIX.length);
        return [
          {
            label: node.label,
            disabled: !node.enabled,
            checked: node.checked,
            onClick: () => {
              if (enabled(command)) runAppCommand(command);
            },
          },
        ];
      })
      .filter(
        (option, index, all) =>
          option.type !== OPTION_SEPARATOR ||
          (index > 0 &&
            index < all.length - 1 &&
            all[index - 1].type !== OPTION_SEPARATOR),
      );
  return MENU_LAYOUT.flatMap((top, index) => {
    const node = built[index];
    if (
      !["file", "edit", "photo", "view", "help"].includes(top.menu) ||
      node.kind !== "submenu"
    )
      return [];
    const extra =
      top.menu === "edit"
        ? ["open_settings"]
        : top.menu === "help"
          ? ["about", "check_updates"]
          : [];
    const app = built[0];
    const extras =
      app.kind === "submenu"
        ? app.items.filter(
            (n) =>
              n.kind === "item" &&
              extra.includes(n.id.slice(MENU_ID_PREFIX.length)),
          )
        : [];
    return [
      {
        label: node.label,
        submenu: convert([
          ...node.items,
          ...(extras.length
            ? [{ kind: "separator" } as MenuNode, ...extras]
            : []),
        ]),
      },
    ];
  });
}
