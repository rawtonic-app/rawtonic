import { Fragment, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { openDialog } from '../../utils/fileDialog';
import { AppWindow, Check, FolderOpen, Settings2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { notify } from '../../activity/notify';
import { Invokes, OPTION_HEADER, OPTION_SEPARATOR, type Option } from '../ui/AppProperties';
import type { ExportPreset } from '../ui/ExportImportProperties';
import { errorCode, errorMessage } from '../../shared/ipc/errors';
import { refreshStacks } from '../../store/useStackStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import {
  chosenRecipe,
  editRecipes,
  photoWritesWritten,
  programGroups,
  programName,
  programRecipe,
  runEditWith,
  type AppChoice,
  type AppChoices,
  type EditRecipe,
  type EditWithRun,
  type OpenWithPrefs
} from '../../utils/editWith';
import { useLibraryStore } from '../../store/useLibraryStore';
import { recipeLabel, recipeSummary, showEditWithSettings } from './EditWithSettingsDialog';

/** Asks for any application ("Other…"): an application bundle on macOS, a
 * program file on Windows, a `.desktop` file or executable on Linux. */
async function pickApplication(title: string): Promise<string | null> {
  const platform = useSettingsStore.getState().osPlatform;
  if (platform === 'windows') return (await invoke<AppChoice | null>(Invokes.OpenWithPick, { title }))?.path ?? null;
  // Linux: a `.desktop` file or an executable (no extension to filter by).
  const linux = platform === 'linux';
  const chosen = await openDialog({
    title,
    defaultPath: linux ? '/usr/share/applications' : '/Applications',
    multiple: false,
    directory: false,
    filters: linux ? undefined : [{ name: 'Applications', extensions: ['app'] }]
  });
  return typeof chosen === 'string' ? chosen : null;
}

/** "Other…": on Windows the program file picker, the last resort after the
 * programs Windows itself offers. */
const otherLabel = (t: TFunction) =>
  ['windows', 'linux'].includes(useSettingsStore.getState().osPlatform)
    ? t('contextMenus.openWith.browse')
    : t('contextMenus.openWith.other');

/** "Browse for a program…": the picked program, or null when the user
 * cancelled; a picker that failed says why instead of doing nothing. */
async function browseForProgram(t: TFunction): Promise<string | null> {
  try {
    return await pickApplication(t('contextMenus.openWith.chooseTitle'));
  } catch (e) {
    notify.error(t('contextMenus.openWith.pickFailed', { error: errorMessage(e) }));
    return null;
  }
}

async function displayName(program: string): Promise<string> {
  const choice = await invoke<AppChoice | null>(Invokes.OpenWithDescribe, { program }).catch(() => null);
  return choice?.name || programName(program);
}

/** Opens the originals of `paths` in `program`, once their pending edits
 * and metadata (and XMP sidecars) are written. */
async function openPhotosWith(program: string, paths: string[], t: TFunction) {
  try {
    await photoWritesWritten();
    await invoke(Invokes.OpenWithOpen, { program, paths });
  } catch (e) {
    notify.error(t('contextMenus.openWith.failed', { error: errorMessage(e) }));
  }
}

/** Renders `paths` with `recipe` and opens the renders in `program` (shown
 * as `name`). */
async function editPhotosWith(
  paths: string[],
  recipe: EditRecipe,
  program: string,
  t: TFunction,
  onRendered?: () => void | Promise<void>,
  name = programName(program)
) {
  const progress = notify.info(t('contextMenus.editWith.rendering', { app: name }), {
    autoClose: false
  });
  try {
    const result = await runEditWith(paths, recipe, program);
    notify.dismiss(progress);
    await onRendered?.();
    if (result.failed > 0) offerTryAgain(result, paths.length, recipe, program, t, onRendered, name);
    if (result.cancelled > 0)
      notify.info(t('contextMenus.editWith.cancelled', { cancelled: result.cancelled, rendered: result.rendered }));
    if (result.problems.length)
      notify.warn(t('contextMenus.editWith.notLinked', { error: result.problems.join('\n') }));
  } catch (e) {
    notify.dismiss(progress);
    // `open_with::GENERATED_AREAS`: photographs with generated areas, named.
    if (errorCode(e) === 'generated_areas') {
      const { names } = e as { names?: string[] };
      notify.error(t('contextMenus.editWith.generatedAreas', { names: (names ?? []).join(', ') }));
    } else notify.error(t('contextMenus.editWith.failed', { error: errorMessage(e) }));
  }
}

/** The notice for photographs an Edit With could not render, with Try
 * again: it renders only those photographs (the finished renders were
 * handed off and are not made a second time), and only while the library
 * the click was made in is open again. In another one a photograph may have
 * other edits, or none (a session's own folder elsewhere belongs to it only
 * while it is open): then it says which library to open and stays. */
function offerTryAgain(
  result: EditWithRun,
  total: number,
  recipe: EditRecipe,
  program: string,
  t: TFunction,
  onRendered: (() => void | Promise<void>) | undefined,
  name: string
) {
  notify.error(t('contextMenus.editWith.someFailed', { failed: result.failed, total }), {
    // Its own entry: another batch failing as often keeps its own Try again.
    toastId: `edit-with-failed:${program}:${result.failedPaths.join('\n')}`,
    source: 'export',
    paths: result.failedPaths,
    actions: [
      {
        label: t('activity.tryAgain'),
        settles: true,
        run: () => {
          if (useLibraryStore.getState().sessionDirectory !== result.library) {
            notify.warn(
              result.library
                ? t('contextMenus.editWith.openLibraryFirst', { library: libraryName(result.library) })
                : t('contextMenus.editWith.closeLibraryFirst')
            );
            offerTryAgain(result, total, recipe, program, t, onRendered, name);
            return;
          }
          console.info(`Edit With in ${program}: Try again for ${result.failedPaths.join(', ')}`);
          return editPhotosWith(result.failedPaths, recipe, program, t, onRendered, name);
        }
      }
    ]
  });
}

/** A library's name as the top bar shows it: its folder. */
const libraryName = (directory: string) => directory.split(/[\\/]/).filter(Boolean).pop() ?? directory;

/** An Edit With the last quit cut short (`export_queue::EditWithRun`). */
interface InterruptedEditWith {
  program: string;
  photos: string[];
  completed?: { source: string; target: string; library?: string | null }[];
}

/** At start-up: each Edit With a quit or crash cut short waits under Needs
 * attention with Try again, which runs Edit With again on its photographs
 * with its program and the recipe chosen now. Finished renders are handed
 * off as they are; only unfinished photographs are rendered again. */
export async function offerInterruptedEditWith(t: TFunction) {
  const runs = await invoke<InterruptedEditWith[]>(Invokes.EditWithInterrupted).catch(() => null);
  for (const run of Array.isArray(runs) ? runs : []) {
    notify.warn(t('contextMenus.editWith.interrupted', { app: await displayName(run.program) }), {
      toastId: `edit-with-interrupted:${run.program}:${run.photos.join('\n')}`,
      source: 'export',
      paths: run.photos,
      actions: [
        {
          label: t('activity.tryAgain'),
          settles: true,
          run: async () => {
            if (run.completed?.length) {
              try {
                const result = await invoke<{ problems: string[] }>(Invokes.EditWithFinish, {
                  items: run.completed,
                  program: run.program
                });
                if (result.problems.length)
                  notify.warn(t('contextMenus.editWith.notLinked', { error: result.problems.join('\n') }));
              } catch (error) {
                notify.error(t('contextMenus.editWith.failed', { error: errorMessage(error) }));
              }
              await refreshStacks();
            }
            if (!run.photos.length) return;
            const recipes = editRecipes(useSettingsStore.getState().appSettings?.exportPresets);
            const prefs = await invoke<OpenWithPrefs>(Invokes.OpenWithPrefs).catch(() => null);
            await editPhotosWith(
              run.photos,
              programRecipe(recipes, prefs, run.program),
              run.program,
              t,
              undefined,
              await displayName(run.program)
            );
          }
        }
      ]
    });
  }
}

/** The programs as plain menu entries (the menu bar's Open With / Edit With
 * show them in the window: its own menus cannot list programs). */
function programOptions(
  choices: AppChoices,
  t: TFunction,
  onPick: (path: string, name: string) => void,
  note: (app: AppChoice) => string | null = () => null
): Option[] {
  if (!choices.supported) return [{ label: t('contextMenus.openWith.unsupported'), disabled: true }];
  const entry = (app: AppChoice): Option => ({
    icon: AppWindow,
    label: [app.name, app.isDefault ? t('contextMenus.openWith.default') : null, note(app)].filter(Boolean).join(' · '),
    onClick: () => onPick(app.path, app.name)
  });
  return [
    ...(choices.apps.length ? [] : [{ label: t('contextMenus.openWith.none'), disabled: true }]),
    ...programGroups(choices.apps).flatMap((group, i) => [
      ...(i > 0 ? [{ type: OPTION_SEPARATOR }] : []),
      ...group.map(entry)
    ]),
    { type: OPTION_SEPARATOR },
    {
      icon: FolderOpen,
      label: otherLabel(t),
      onClick: () => void browseForProgram(t).then(async (path) => path && onPick(path, await displayName(path)))
    }
  ];
}

/** The programs Edit With lists: the ones for the default recipe's format,
 * then each program with settings of its own that this list misses (its own
 * format may be one the default's list does not cover); a program that is
 * gone is left out. */
async function editChoices(extension: string, prefs: OpenWithPrefs | null | undefined): Promise<AppChoices> {
  const choices = await invoke<AppChoices>(Invokes.EditWithApps, { extension });
  if (!choices.supported) return choices;
  const listed = new Set(choices.apps.map((app) => app.path));
  const missing = Object.keys(prefs?.programSettings ?? {}).filter((program) => !listed.has(program));
  const found = await Promise.all(
    missing.map(async (program) => {
      const app = await invoke<AppChoice | null>(Invokes.OpenWithDescribe, { program }).catch(() => null);
      return app ? { ...app, path: program, isDefault: false, isRecent: false, recommended: true } : null;
    })
  );
  return { ...choices, apps: [...choices.apps, ...found.filter((app) => app !== null)] };
}

/** Open With as menu entries for `paths`. */
export async function openWithOptions(paths: string[], t: TFunction): Promise<Option[]> {
  const choices = await invoke<AppChoices>(Invokes.OpenWithApps, { paths });
  return [
    { type: OPTION_HEADER, label: t('contextMenus.openWith.label') },
    ...programOptions(choices, t, (program) => void openPhotosWith(program, paths, t))
  ];
}

/** Edit With as menu entries for `paths`: the recipe (remembered), then the
 * programs for its format. */
export async function editWithOptions(
  paths: string[],
  t: TFunction,
  presets: ExportPreset[] | undefined,
  onRendered?: () => void | Promise<void>
): Promise<Option[]> {
  const recipes = editRecipes(presets);
  const prefs = await invoke<OpenWithPrefs>(Invokes.OpenWithPrefs).catch(() => null);
  const recipe = chosenRecipe(recipes, prefs?.editRecipe);
  const choices = await editChoices(recipe.outputFormat, prefs);
  const programs = choices.supported ? choices.apps : [];
  return [
    { type: OPTION_HEADER, label: t('contextMenus.editWith.label') },
    {
      label: `${t('contextMenus.editWith.defaultRecipe')}: ${recipeLabel(recipe, t)}`,
      submenu: recipes.map((r) => ({
        label: recipeLabel(r, t),
        checked: r.id === recipe.id,
        onClick: () => void invoke(Invokes.EditWithSetRecipe, { recipe: r.id }).catch(() => {})
      }))
    },
    ...(programs.length
      ? [
          {
            label: t('contextMenus.editWith.programSettings'),
            submenu: programs.map((app) => ({
              icon: Settings2,
              label: t('contextMenus.editWith.settingsFor', { app: app.name }),
              onClick: () => void showEditWithSettings(app.path, app.name, t)
            }))
          }
        ]
      : []),
    { type: OPTION_SEPARATOR },
    ...programOptions(
      choices,
      t,
      (program, name) =>
        void editPhotosWith(paths, programRecipe(recipes, prefs, program), program, t, onRendered, name),
      (app) => ownSummary(recipes, prefs, app.path, t)
    )
  ];
}

/** A program's own Edit With settings in a few words (null: it uses the
 * default recipe). */
function ownSummary(
  recipes: EditRecipe[],
  prefs: OpenWithPrefs | null | undefined,
  program: string,
  t: TFunction
): string | null {
  const recipe = programRecipe(recipes, prefs, program);
  return recipe.program ? recipeSummary(recipe, t) : null;
}

/** Edit With's per-program parts of a program row: its own settings in a
 * few words, and "Settings for <program>…". */
interface RowSettings {
  note(app: AppChoice): string | null;
  open(app: AppChoice): void;
}

/** The programs, the default and recent ones first, then "Other…". */
function AppRows({
  choices,
  onPick,
  settings
}: {
  choices: AppChoices | null;
  onPick(path: string, name: string): void;
  settings?: RowSettings;
}) {
  const { t } = useTranslation();
  if (choices === null) return <div className="rawtonic-menu-heading">{t('contextMenus.openWith.reading')}</div>;
  if (!choices.supported) return <div className="rawtonic-menu-heading">{t('contextMenus.openWith.unsupported')}</div>;
  const row = (app: AppChoice) => {
    const note = settings?.note(app);
    const button = (
      <button
        key={app.path}
        type="button"
        role="menuitem"
        className="rawtonic-menu-item"
        title={app.path}
        onClick={() => onPick(app.path, app.name)}
      >
        <span className="flex items-center gap-2.5 min-w-0">
          {app.icon ? (
            <img src={app.icon} alt="" width={16} height={16} className="shrink-0" />
          ) : (
            <AppWindow size={14} className="shrink-0" />
          )}
          <span className="truncate">{app.name}</span>
        </span>
        {(app.isDefault || note) && (
          <small className="rawtonic-menu-count">
            {[app.isDefault ? t('contextMenus.openWith.default') : null, note].filter(Boolean).join(' · ')}
          </small>
        )}
      </button>
    );
    if (!settings) return button;
    const label = t('contextMenus.editWith.settingsFor', { app: app.name });
    return (
      <div key={app.path} className="rawtonic-menu-row">
        {button}
        <button
          type="button"
          role="menuitem"
          className="rawtonic-menu-item is-icon"
          aria-label={label}
          title={label}
          onClick={() => settings.open(app)}
        >
          <Settings2 size={14} aria-hidden="true" />
        </button>
      </div>
    );
  };
  return (
    <>
      {choices.apps.length === 0 && <div className="rawtonic-menu-heading">{t('contextMenus.openWith.none')}</div>}
      {programGroups(choices.apps).map((group, i) => (
        <Fragment key={i}>
          {i > 0 && <div className="rawtonic-menu-separator" />}
          {group.map(row)}
        </Fragment>
      ))}
      <div className="rawtonic-menu-separator" />
      <button
        type="button"
        role="menuitem"
        className="rawtonic-menu-item"
        onClick={async () => {
          const path = await browseForProgram(t);
          if (path) onPick(path, await displayName(path));
        }}
      >
        <span className="flex items-center gap-2.5 min-w-0">
          <FolderOpen size={14} className="shrink-0" />
          <span className="truncate">{otherLabel(t)}</span>
        </span>
      </button>
    </>
  );
}

function useChoices(command: Invokes, args: Record<string, unknown> | null) {
  const [choices, setChoices] = useState<AppChoices | null>(null);
  const key = JSON.stringify(args);
  useEffect(() => {
    if (args === null) return;
    let current = true;
    setChoices(null);
    invoke<AppChoices>(command, args)
      .then((list) => current && setChoices(list))
      .catch(() => current && setChoices({ supported: true, apps: [] }));
    return () => {
      current = false;
    };
    // `key` stands for `args`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [command, key]);
  return choices;
}

/** Open With: the originals of the photographs in another program; nothing
 * is written or moved. */
export function OpenWithSubMenu({ paths, hideContextMenu }: { paths: string[]; hideContextMenu(): void }) {
  const { t } = useTranslation();
  const choices = useChoices(Invokes.OpenWithApps, { paths });
  const pick = (program: string) => {
    hideContextMenu();
    void openPhotosWith(program, paths, t);
  };
  return (
    <div className="rawtonic-menu-list min-w-56" role="menu" aria-label={t('contextMenus.openWith.label')}>
      <AppRows choices={choices} onPick={pick} />
    </div>
  );
}

/** Edit With: renders of the photographs with the chosen recipe, beside
 * their originals and stacked with them, opened in another program. */
export function EditWithSubMenu({
  paths,
  hideContextMenu,
  onRendered
}: {
  paths: string[];
  hideContextMenu(): void;
  onRendered?(): void | Promise<void>;
}) {
  const { t } = useTranslation();
  const presets = useSettingsStore((state) => state.appSettings?.exportPresets);
  const recipes = editRecipes(presets);
  // undefined while reading; null when they cannot be read.
  const [prefs, setPrefs] = useState<OpenWithPrefs | null | undefined>(undefined);
  useEffect(() => {
    let current = true;
    invoke<OpenWithPrefs>(Invokes.OpenWithPrefs)
      .then((read) => current && setPrefs(read ?? null))
      .catch(() => current && setPrefs(null));
    return () => {
      current = false;
    };
  }, []);
  const recipe = chosenRecipe(recipes, prefs?.editRecipe);
  const [choices, setChoices] = useState<AppChoices | null>(null);
  const extension = recipe.outputFormat;
  useEffect(() => {
    if (prefs === undefined) return;
    let current = true;
    setChoices(null);
    editChoices(extension, prefs)
      .then((list) => current && setChoices(list))
      .catch(() => current && setChoices({ supported: true, apps: [] }));
    return () => {
      current = false;
    };
  }, [extension, prefs]);

  const choose = (id: string) => {
    setPrefs((read) => ({ recentApps: [], ...read, editRecipe: id }));
    invoke(Invokes.EditWithSetRecipe, { recipe: id }).catch(() => {});
  };

  const pick = (program: string, name: string) => {
    hideContextMenu();
    void editPhotosWith(paths, programRecipe(recipes, prefs, program), program, t, onRendered, name);
  };

  const settings: RowSettings = {
    note: (app) => ownSummary(recipes, prefs, app.path, t),
    open: (app) => {
      hideContextMenu();
      void showEditWithSettings(app.path, app.name, t);
    }
  };

  return (
    <div className="rawtonic-menu-list min-w-60" role="menu" aria-label={t('contextMenus.editWith.label')}>
      <div className="rawtonic-menu-heading rawtonic-menu-group">{t('contextMenus.editWith.defaultRecipe')}</div>
      {recipes.map((r) => (
        <button
          key={r.id}
          type="button"
          role="menuitemradio"
          aria-checked={r.id === recipe.id}
          className="rawtonic-menu-item"
          onClick={() => choose(r.id)}
        >
          <span className="flex items-center gap-2.5 min-w-0">
            {r.id === recipe.id ? (
              <Check size={14} className="shrink-0" aria-hidden="true" />
            ) : (
              <span className="w-3.5 shrink-0" aria-hidden="true" />
            )}
            <span className="truncate">{recipeLabel(r, t)}</span>
          </span>
        </button>
      ))}
      <div className="rawtonic-menu-separator" />
      <div className="rawtonic-menu-heading rawtonic-menu-group">{t('contextMenus.editWith.openIn')}</div>
      <AppRows choices={choices} onPick={pick} settings={settings} />
    </div>
  );
}
