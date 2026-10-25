import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { AppWindow, Check, FolderOpen } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { toast } from 'react-toastify';
import { Invokes, OPTION_HEADER, OPTION_SEPARATOR, type Option } from '../ui/AppProperties';
import type { ExportPreset } from '../ui/ExportImportProperties';
import { useSettingsStore } from '../../store/useSettingsStore';
import {
  chosenRecipe,
  editRecipes,
  runEditWith,
  type AppChoice,
  type AppChoices,
  type EditRecipe,
  type OpenWithPrefs,
} from '../../utils/editWith';

/** Asks for any application ("Other…"). */
async function pickApplication(title: string): Promise<string | null> {
  const chosen = await openDialog({
    title,
    defaultPath: '/Applications',
    multiple: false,
    directory: false,
    filters: [{ name: 'Applications', extensions: ['app'] }],
  });
  return typeof chosen === 'string' ? chosen : null;
}

const errorText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : String(e));

/** Opens the originals of `paths` in `program`. */
async function openPhotosWith(program: string, paths: string[], t: TFunction) {
  try {
    await invoke(Invokes.OpenWithOpen, { program, paths });
  } catch (e) {
    toast.error(t('contextMenus.openWith.failed', { error: errorText(e) }));
  }
}

/** Renders `paths` with `recipe` and opens the renders in `program`. */
async function editPhotosWith(
  paths: string[],
  recipe: EditRecipe,
  program: string,
  t: TFunction,
  onRendered?: () => void | Promise<void>,
) {
  const name = program.split('/').pop()?.replace(/\.app$/, '') ?? program;
  const progress = toast.info(t('contextMenus.editWith.rendering', { app: name }), { autoClose: false });
  try {
    const result = await runEditWith(paths, recipe, program);
    toast.dismiss(progress);
    await onRendered?.();
    if (result.libraryChanged) toast.error(t('contextMenus.editWith.libraryChanged'));
    else if (result.failed > 0)
      toast.error(t('contextMenus.editWith.someFailed', { failed: result.failed, total: paths.length }));
    if (result.problems.length) toast.warn(t('contextMenus.editWith.notLinked', { error: result.problems.join('\n') }));
  } catch (e) {
    toast.dismiss(progress);
    const error = errorText(e);
    // `open_with::GENERATED_AREAS`: photographs with generated areas.
    const generated = error.match(/^generated-areas: (.*)$/s);
    if (generated) toast.error(t('contextMenus.editWith.generatedAreas', { names: generated[1] }));
    else toast.error(t('contextMenus.editWith.failed', { error }));
  }
}

const recipeLabel = (recipe: EditRecipe, t: TFunction) =>
  recipe.builtIn === 'adobeRgb'
    ? t('contextMenus.editWith.tiffAdobe')
    : recipe.builtIn === 'proPhoto'
      ? t('contextMenus.editWith.tiffProPhoto')
      : (recipe.name ?? recipe.id);

/** The programs as plain menu entries (the menu bar's Open With / Edit With
 * show them in the window: its own menus cannot list programs). */
function programOptions(choices: AppChoices, t: TFunction, onPick: (path: string) => void): Option[] {
  if (!choices.supported) return [{ label: t('contextMenus.openWith.unsupported'), disabled: true }];
  const entry = (app: AppChoice): Option => ({
    icon: AppWindow,
    label: app.isDefault ? `${app.name} · ${t('contextMenus.openWith.default')}` : app.name,
    onClick: () => onPick(app.path),
  });
  const pinned = choices.apps.filter((app) => app.isDefault || app.isRecent);
  const others = choices.apps.filter((app) => !app.isDefault && !app.isRecent);
  return [
    ...(choices.apps.length ? [] : [{ label: t('contextMenus.openWith.none'), disabled: true }]),
    ...pinned.map(entry),
    ...(pinned.length && others.length ? [{ type: OPTION_SEPARATOR }] : []),
    ...others.map(entry),
    { type: OPTION_SEPARATOR },
    {
      icon: FolderOpen,
      label: t('contextMenus.openWith.other'),
      onClick: () =>
        void pickApplication(t('contextMenus.openWith.chooseTitle'))
          .catch(() => null)
          .then((path) => path && onPick(path)),
    },
  ];
}

/** Open With as menu entries for `paths`. */
export async function openWithOptions(paths: string[], t: TFunction): Promise<Option[]> {
  const choices = await invoke<AppChoices>(Invokes.OpenWithApps, { paths });
  return [
    { type: OPTION_HEADER, label: t('contextMenus.openWith.label') },
    ...programOptions(choices, t, (program) => void openPhotosWith(program, paths, t)),
  ];
}

/** Edit With as menu entries for `paths`: the recipe (remembered), then the
 * programs for its format. */
export async function editWithOptions(
  paths: string[],
  t: TFunction,
  presets: ExportPreset[] | undefined,
  onRendered?: () => void | Promise<void>,
): Promise<Option[]> {
  const recipes = editRecipes(presets);
  const prefs = await invoke<OpenWithPrefs>(Invokes.OpenWithPrefs).catch(() => null);
  const recipe = chosenRecipe(recipes, prefs?.editRecipe);
  const choices = await invoke<AppChoices>(Invokes.EditWithApps, { extension: recipe.outputFormat });
  return [
    { type: OPTION_HEADER, label: t('contextMenus.editWith.label') },
    {
      label: `${t('contextMenus.editWith.recipe')}: ${recipeLabel(recipe, t)}`,
      submenu: recipes.map((r) => ({
        label: recipeLabel(r, t),
        checked: r.id === recipe.id,
        onClick: () => void invoke(Invokes.EditWithSetRecipe, { recipe: r.id }).catch(() => {}),
      })),
    },
    { type: OPTION_SEPARATOR },
    ...programOptions(choices, t, (program) => void editPhotosWith(paths, recipe, program, t, onRendered)),
  ];
}

/** The programs, the default and recent ones first, then "Other…". */
function AppRows({ choices, onPick }: { choices: AppChoices | null; onPick(path: string): void }) {
  const { t } = useTranslation();
  if (choices === null) return <div className="rawtonic-menu-heading">{t('contextMenus.openWith.reading')}</div>;
  if (!choices.supported)
    return <div className="rawtonic-menu-heading">{t('contextMenus.openWith.unsupported')}</div>;
  const pinned = choices.apps.filter((app) => app.isDefault || app.isRecent);
  const others = choices.apps.filter((app) => !app.isDefault && !app.isRecent);
  const row = (app: AppChoice) => (
    <button
      key={app.path}
      type="button"
      role="menuitem"
      className="rawtonic-menu-item"
      title={app.path}
      onClick={() => onPick(app.path)}
    >
      <span className="flex items-center gap-2.5 min-w-0">
        {app.icon ? (
          <img src={app.icon} alt="" width={16} height={16} className="shrink-0" />
        ) : (
          <AppWindow size={14} className="shrink-0" />
        )}
        <span className="truncate">{app.name}</span>
      </span>
      {app.isDefault && <small className="rawtonic-menu-count">{t('contextMenus.openWith.default')}</small>}
    </button>
  );
  return (
    <>
      {choices.apps.length === 0 && <div className="rawtonic-menu-heading">{t('contextMenus.openWith.none')}</div>}
      {pinned.map(row)}
      {pinned.length > 0 && others.length > 0 && <div className="rawtonic-menu-separator" />}
      {others.map(row)}
      <div className="rawtonic-menu-separator" />
      <button
        type="button"
        role="menuitem"
        className="rawtonic-menu-item"
        onClick={async () => {
          const path = await pickApplication(t('contextMenus.openWith.chooseTitle')).catch(() => null);
          if (path) onPick(path);
        }}
      >
        <span className="flex items-center gap-2.5 min-w-0">
          <FolderOpen size={14} className="shrink-0" />
          <span className="truncate">{t('contextMenus.openWith.other')}</span>
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
  onRendered,
}: {
  paths: string[];
  hideContextMenu(): void;
  onRendered?(): void | Promise<void>;
}) {
  const { t } = useTranslation();
  const presets = useSettingsStore((state) => state.appSettings?.exportPresets);
  const recipes = editRecipes(presets);
  const [recipeId, setRecipeId] = useState<string | null | undefined>(undefined);
  useEffect(() => {
    let current = true;
    invoke<OpenWithPrefs>(Invokes.OpenWithPrefs)
      .then((prefs) => current && setRecipeId(prefs?.editRecipe ?? null))
      .catch(() => current && setRecipeId(null));
    return () => {
      current = false;
    };
  }, []);
  const recipe = chosenRecipe(recipes, recipeId);
  const choices = useChoices(
    Invokes.EditWithApps,
    recipeId === undefined ? null : { extension: recipe.outputFormat },
  );

  const choose = (id: string) => {
    setRecipeId(id);
    invoke(Invokes.EditWithSetRecipe, { recipe: id }).catch(() => {});
  };

  const pick = (program: string) => {
    hideContextMenu();
    void editPhotosWith(paths, recipe, program, t, onRendered);
  };

  return (
    <div className="rawtonic-menu-list min-w-60" role="menu" aria-label={t('contextMenus.editWith.label')}>
      <div className="rawtonic-menu-heading rawtonic-menu-group">{t('contextMenus.editWith.recipe')}</div>
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
      <AppRows choices={choices} onPick={pick} />
    </div>
  );
}
