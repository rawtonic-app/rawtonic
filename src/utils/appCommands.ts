/**
 * The app's commands by name, for callers other than their keys: the macOS
 * menu bar (useAppMenu). Each command is registered by the code that owns
 * it, with the handler its key runs (useKeyboardShortcuts registers the
 * shortcut table's actions), so a menu item and its key never differ.
 */
export interface AppCommand {
  run(): void;
  /** Whether running it does something now (the menu greys it out else). */
  enabled?(): boolean;
  /** A state the menu shows with a check mark. */
  checked?(): boolean;
}

const commands = new Map<string, AppCommand>();
const listeners = new Set<() => void>();

const notify = () => listeners.forEach((listener) => listener());

/** Registers commands; returns their removal. A later registration of the
 * same name replaces an earlier one until it is removed. */
export function registerAppCommands(entries: Record<string, AppCommand>): () => void {
  for (const [name, command] of Object.entries(entries)) commands.set(name, command);
  notify();
  return () => {
    for (const [name, command] of Object.entries(entries)) {
      if (commands.get(name) === command) commands.delete(name);
    }
    notify();
  };
}

export function appCommand(name: string): AppCommand | undefined {
  return commands.get(name);
}

export function appCommandEnabled(name: string): boolean {
  const command = commands.get(name);
  return !!command && (command.enabled?.() ?? true);
}

/** Runs a command when it is enabled; returns whether it ran. */
export function runAppCommand(name: string): boolean {
  const command = commands.get(name);
  if (!command || !(command.enabled?.() ?? true)) return false;
  command.run();
  return true;
}

/** Called after commands are added or removed. */
export function onAppCommandsChanged(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** For owners whose commands' availability changed outside the stores
 * (component state): the menu bar refreshes its state. */
export function notifyAppCommandsChanged(): void {
  notify();
}
