import { useEffect, useId, useRef, useState } from "react";
import { createPortal, flushSync } from "react-dom";
import { useTranslation } from "react-i18next";
import { Search, X } from "lucide-react";
import { useModalFocus } from "../../hooks/useModalFocus";
import { menuCommandEnabled } from "../../hooks/useAppMenu";
import {
  onAppCommandsChanged,
  registerAppCommands,
  runAppCommand,
} from "../../utils/appCommands";
import {
  paletteCommands,
  rankCommands,
  type PaletteCommand,
} from "../../utils/commandPalette";
import { type Translate } from "../../utils/appMenu";
import { useSettingsStore } from "../../store/useSettingsStore";
import { isAnyModalOpen, useUIStore } from "../../store/useUIStore";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useMultiViewStore } from "../../store/useMultiViewStore";
import { useUpdateStore } from "../../store/useUpdateStore";

const paletteCommandEnabled = (command: string) =>
  menuCommandEnabled(command, true);

const RECENT_KEY = "rawtonic.recentCommands";
function readRecent(): string[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
    return Array.isArray(value)
      ? value.filter((id): id is string => typeof id === "string").slice(0, 12)
      : [];
  } catch {
    return [];
  }
}

export default function CommandPalette() {
  const [open, setOpen] = useState(false);
  useEffect(
    () =>
      registerAppCommands({
        command_palette: {
          enabled: () => menuCommandEnabledForPalette(),
          run: () => setOpen(true),
        },
      }),
    [],
  );
  return open ? <PaletteDialog close={() => setOpen(false)} /> : null;
}

function menuCommandEnabledForPalette() {
  // Do not open another chooser over Preferences or an existing dialog.
  return (
    !useUIStore.getState().isSettingsOpen &&
    !document.querySelector('dialog[open], [aria-modal="true"]')
  );
}

function PaletteDialog({ close }: { close(): void }) {
  const { t } = useTranslation();
  const keybinds = useSettingsStore((state) => state.appSettings?.keybinds);
  const platform = useSettingsStore((state) => state.osPlatform);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [recent] = useState(readRecent);
  const [, refresh] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const id = useId();
  useModalFocus(root, true);
  useEffect(() => {
    const update = () => refresh((value) => value + 1);
    const stops = [
      onAppCommandsChanged(update),
      useUIStore.subscribe((state) => {
        // Native Help stays available over dialogs; give its destination focus.
        if (
          state.isSettingsOpen ||
          state.isProblemReportOpen ||
          isAnyModalOpen(state)
        )
          close();
        else update();
      }),
      ...[
        useEditorStore,
        useLibraryStore,
        useMultiViewStore,
        useUpdateStore,
      ].map((store) => store.subscribe(update)),
    ];
    return () => stops.forEach((stop) => stop());
  }, [close]);
  const commands = rankCommands(
    paletteCommands(
      t as unknown as Translate,
      keybinds,
      platform,
      paletteCommandEnabled,
    ),
    query,
    recent,
  );
  const selected = Math.min(active, Math.max(0, commands.length - 1));
  useEffect(() => {
    document
      .getElementById(`${id}-option-${selected}`)
      ?.scrollIntoView?.({ block: "nearest" });
  }, [id, selected, query]);
  const execute = (command: PaletteCommand | undefined) => {
    if (!command?.enabled || !paletteCommandEnabled(command.id)) return;
    // Remove the chooser and restore focus before a handler opens its dialog.
    flushSync(close);
    if (menuCommandEnabled(command.id) && runAppCommand(command.id)) {
      try {
        localStorage.setItem(
          RECENT_KEY,
          JSON.stringify(
            [command.id, ...recent.filter((item) => item !== command.id)].slice(
              0,
              12,
            ),
          ),
        );
      } catch {
        /* Storage may be disabled. */
      }
    }
  };
  return createPortal(
    <div
      className="fixed inset-0 z-200 flex justify-center items-start pt-[min(15vh,120px)] px-4 bg-black/50"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <div
        ref={root}
        role="dialog"
        aria-modal="true"
        data-command-palette
        aria-labelledby={`${id}-title`}
        className="w-full max-w-xl rounded-xl border border-border-color bg-surface text-text-primary shadow-2xl overflow-hidden"
        onKeyDown={(event) => {
          event.stopPropagation();
          if (event.nativeEvent.isComposing) return;
          if (event.key === "Escape") {
            event.preventDefault();
            close();
          }
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            setActive(
              commands.length
                ? (selected +
                    (event.key === "ArrowDown" ? 1 : commands.length - 1)) %
                    commands.length
                : 0,
            );
          }
          if (
            event.key === "Enter" &&
            !(event.target instanceof HTMLButtonElement)
          ) {
            event.preventDefault();
            execute(commands[selected]);
          }
        }}
      >
        <div className="flex items-center gap-3 px-4 pt-3">
          <h2 id={`${id}-title`} className="text-sm font-semibold grow">
            {t("appMenu.items.command_palette")}
          </h2>
          <button
            type="button"
            onClick={close}
            aria-label={t("commandPalette.close")}
            className="p-2 rounded hover:bg-bg-primary focus-visible:outline-2 focus-visible:outline-accent"
          >
            <X size={16} />
          </button>
        </div>
        <div className="flex items-center gap-3 px-4 pb-3 border-b border-border-color">
          <Search
            size={18}
            aria-hidden="true"
            className="text-text-secondary shrink-0"
          />
          <input
            role="combobox"
            data-autofocus
            aria-label={t("commandPalette.search")}
            aria-expanded="true"
            aria-controls={`${id}-list`}
            aria-activedescendant={
              commands.length ? `${id}-option-${selected}` : undefined
            }
            aria-autocomplete="list"
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setActive(0);
            }}
            placeholder={t("commandPalette.search")}
            className="w-full bg-transparent py-2 text-sm outline-none"
          />
        </div>
        <div
          role="listbox"
          id={`${id}-list`}
          aria-label={t("appMenu.items.command_palette")}
          className="max-h-[min(55vh,440px)] overflow-y-auto p-2"
        >
          {commands.map((command, index) => (
            <div
              key={command.id}
              id={`${id}-option-${index}`}
              role="option"
              aria-selected={index === selected}
              aria-disabled={!command.enabled}
              onMouseDown={(event) => event.preventDefault()}
              onMouseMove={() => setActive(index)}
              onClick={() => execute(command)}
              className={`flex items-center gap-4 px-3 py-2.5 rounded text-sm ${index === selected ? "bg-bg-primary ring-1 ring-border-color" : ""} ${command.enabled ? "cursor-pointer" : "opacity-40 cursor-default"}`}
            >
              <span className="grow">{command.label}
                {command.id === "match_light" && !command.enabled && (
                  <span className="block text-xs text-text-secondary">{t("commandPalette.selectTwoPhotos")}</span>
                )}
              </span>
              {command.shortcut && (
                <kbd className="shrink-0 text-xs text-text-secondary">
                  {command.shortcut}
                </kbd>
              )}
            </div>
          ))}
          {!commands.length && (
            <p role="status" className="p-4 text-sm text-text-secondary">
              {t("commandPalette.empty")}
            </p>
          )}
        </div>
        <p className="px-4 py-2.5 border-t border-border-color text-xs text-text-secondary">
          {t(query ? "commandPalette.keys" : "commandPalette.recent")}
        </p>
      </div>
    </div>,
    document.body,
  );
}
