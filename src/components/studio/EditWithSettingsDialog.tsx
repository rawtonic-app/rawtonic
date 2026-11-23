import { useEffect, useRef, useState, type KeyboardEvent } from 'react';
import { createPortal } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { create } from 'zustand';
import type { TFunction } from 'i18next';
import { Invokes } from '../ui/AppProperties';
import type { OutputColorSpace } from '../ui/ExportImportProperties';
import { errorMessage } from '../../shared/ipc/errors';
import { useSettingsStore } from '../../store/useSettingsStore';
import {
  COLOR_SPACES,
  COLOR_SPACE_NAMES,
  RESIZE_MODES,
  RESIZE_UNITS,
  defaultResizeValue,
  resizeRange,
  type RecipeDraft,
  type ResizeMode
} from '../../utils/exportRecipes';
import {
  PROGRAM_FORMATS,
  chosenRecipe,
  editRecipes,
  programRecipe,
  programSettings,
  type EditRecipe,
  type OpenWithPrefs
} from '../../utils/editWith';

const FORMAT_NAMES: Record<string, string> = {
  tiff: 'TIFF',
  jpg: 'JPEG',
  png: 'PNG',
  webp: 'WebP',
  jxl: 'JPEG XL',
  avif: 'AVIF'
};
const QUALITY_FORMATS = ['jpg', 'webp', 'jxl', 'avif'];

/** A recipe in a few words, as the Edit With menu shows a program's own
 * settings: format, bit depth (TIFF) and colour space. */
export function recipeSummary(recipe: EditRecipe, t: TFunction): string {
  const { format, depth, colorSpace } = recipe.draft;
  return [
    FORMAT_NAMES[format] ?? format.toUpperCase(),
    ...(format === 'tiff' ? [t('studio.export.file.bits', { bits: depth })] : []),
    COLOR_SPACE_NAMES[colorSpace]
  ].join(' · ');
}

/** A default recipe's name as the Edit With menu lists it. */
export const recipeLabel = (recipe: EditRecipe, t: TFunction) =>
  recipe.builtIn === 'adobeRgb'
    ? t('contextMenus.editWith.tiffAdobe')
    : recipe.builtIn === 'proPhoto'
      ? t('contextMenus.editWith.tiffProPhoto')
      : (recipe.name ?? recipe.id);

interface Shown {
  program: string;
  name: string;
  /** The recipe the program uses now: its own, or the default one. */
  recipe: EditRecipe;
  /** The default recipe's name (for a program without own settings). */
  defaultName: string;
}

const useEditWithSettings = create<{ shown: Shown | null; show(shown: Shown): void; close(): void }>((set) => ({
  shown: null,
  show: (shown) => set({ shown }),
  close: () => set({ shown: null })
}));

/** "Settings for <program>…": the dialog with the settings `program`'s
 * renders are made with, starting from the ones it uses now. */
export async function showEditWithSettings(program: string, name: string, t: TFunction) {
  const recipes = editRecipes(useSettingsStore.getState().appSettings?.exportPresets);
  const prefs = await invoke<OpenWithPrefs>(Invokes.OpenWithPrefs).catch(() => null);
  useEditWithSettings.getState().show({
    program,
    name,
    recipe: programRecipe(recipes, prefs, program),
    defaultName: recipeLabel(chosenRecipe(recipes, prefs?.editRecipe), t)
  });
}

export function EditWithSettingsDialogHost() {
  const shown = useEditWithSettings((s) => s.shown);
  return shown ? <EditWithSettingsDialog key={shown.program} shown={shown} /> : null;
}

function EditWithSettingsDialog({ shown }: { shown: Shown }) {
  const { t } = useTranslation();
  const close = useEditWithSettings((s) => s.close);
  const [draft, setDraft] = useState<RecipeDraft>(shown.recipe.draft);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const dialog = useRef<HTMLDivElement>(null);
  const set = (patch: Partial<RecipeDraft>) => setDraft((current) => ({ ...current, ...patch }));
  const own = !!shown.recipe.program;
  const title = t('contextMenus.editWith.settingsTitle', { app: shown.name });
  // A saved recipe of another format keeps it (offered beside the others).
  const formats: string[] = PROGRAM_FORMATS.includes(draft.format as never)
    ? [...PROGRAM_FORMATS]
    : [...PROGRAM_FORMATS, draft.format];
  const range = resizeRange(draft.resizeMode);

  useEffect(() => {
    dialog.current?.focus();
  }, []);

  const store = async (settings: ReturnType<typeof programSettings> | null) => {
    setBusy(true);
    setProblem(null);
    try {
      await invoke(Invokes.EditWithSetProgramSettings, { program: shown.program, settings });
      close();
    } catch (e) {
      setProblem(t('contextMenus.editWith.settingsFailed', { error: errorMessage(e) }));
    } finally {
      setBusy(false);
    }
  };

  const keys = (e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.stopPropagation();
      close();
    }
  };

  return createPortal(
    <div className="studio-dialog-backdrop" onKeyDown={keys}>
      <div ref={dialog} className="studio-dialog" role="dialog" aria-modal="true" aria-label={title} tabIndex={-1}>
        <h2 className="studio-dialog-title">{title}</h2>
        <p className="studio-help">
          {own
            ? t('contextMenus.editWith.ownSettings', { app: shown.name })
            : t('contextMenus.editWith.usesDefault', { app: shown.name, recipe: shown.defaultName })}
        </p>
        <div className="studio-proof-sheet-form">
          <fieldset disabled={busy}>
            <legend>{t('studio.export.file.legend')}</legend>
            <label>
              {t('studio.export.file.format')}
              <select value={draft.format} onChange={(e) => set({ format: e.target.value })}>
                {formats.map((format) => (
                  <option key={format} value={format}>
                    {FORMAT_NAMES[format] ?? format.toUpperCase()}
                  </option>
                ))}
              </select>
            </label>
            {QUALITY_FORMATS.includes(draft.format) && (
              <label>
                {t('studio.export.file.quality')}
                <input
                  type="number"
                  min="1"
                  max="100"
                  value={draft.quality}
                  onChange={(e) => set({ quality: Number(e.target.value) })}
                />
              </label>
            )}
            {draft.format === 'tiff' && (
              <label>
                {t('studio.export.file.bitDepth')}
                <select value={draft.depth} onChange={(e) => set({ depth: Number(e.target.value) as 8 | 16 })}>
                  <option value="16">{t('studio.export.file.bits', { bits: 16 })}</option>
                  <option value="8">{t('studio.export.file.bits', { bits: 8 })}</option>
                </select>
              </label>
            )}
            <label>
              {t('studio.export.file.colorSpace')}
              <select
                value={draft.colorSpace}
                onChange={(e) => set({ colorSpace: e.target.value as OutputColorSpace })}
              >
                {COLOR_SPACES.map((space) => (
                  <option key={space} value={space}>
                    {COLOR_SPACE_NAMES[space]}
                  </option>
                ))}
              </select>
            </label>
            {draft.colorSpace === 'proPhoto' && !(draft.format === 'tiff' && draft.depth === 16) && (
              <small>{t('studio.export.file.proPhotoWarning')}</small>
            )}
          </fieldset>
          <fieldset disabled={busy}>
            <legend>{t('studio.export.sizing.legend')}</legend>
            <label className="studio-proof-sheet-check">
              <input type="checkbox" checked={draft.resize} onChange={(e) => set({ resize: e.target.checked })} />
              {t('studio.export.sizing.resize')}
            </label>
            {draft.resize && (
              <div className="studio-proof-sheet-pair">
                <label>
                  {t('studio.export.sizing.fit')}
                  <select
                    value={draft.resizeMode}
                    onChange={(e) => {
                      const mode = e.target.value as ResizeMode;
                      set({ resizeMode: mode, resizeValue: defaultResizeValue(mode) });
                    }}
                  >
                    {RESIZE_MODES.map((mode) => (
                      <option key={mode} value={mode}>
                        {t(`studio.export.sizing.modes.${mode}`)}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  {t('studio.export.sizing.valueLabel', {
                    name: t(`studio.export.sizing.modes.${draft.resizeMode}`),
                    unit: RESIZE_UNITS[draft.resizeMode]
                  })}
                  <input
                    type="number"
                    min={range.min}
                    max={range.max}
                    step={range.step}
                    value={draft.resizeValue}
                    onChange={(e) => set({ resizeValue: Number(e.target.value) })}
                  />
                </label>
                {draft.resizeMode === 'dimensions' && (
                  <label>
                    × (px)
                    <input
                      aria-label={t('studio.export.sizing.otherSideAria')}
                      type="number"
                      min={range.min}
                      max={range.max}
                      step={range.step}
                      value={draft.resizeValue2}
                      onChange={(e) => set({ resizeValue2: Number(e.target.value) })}
                    />
                  </label>
                )}
              </div>
            )}
            {draft.resize && (
              <label className="studio-proof-sheet-check">
                <input
                  type="checkbox"
                  checked={draft.dontEnlarge}
                  onChange={(e) => set({ dontEnlarge: e.target.checked })}
                />
                {t('studio.export.sizing.dontEnlarge')}
              </label>
            )}
          </fieldset>
          <fieldset disabled={busy}>
            <legend>{t('studio.export.metadata.legend')}</legend>
            <label className="studio-proof-sheet-check">
              <input type="checkbox" checked={draft.metadata} onChange={(e) => set({ metadata: e.target.checked })} />
              {t('studio.export.metadata.include')}
            </label>
            {!draft.metadata && <small>{t('studio.export.metadata.rightsKept')}</small>}
            <label className="studio-proof-sheet-check">
              <input
                type="checkbox"
                checked={!!draft.keepCreatorContact}
                disabled={draft.metadata}
                onChange={(e) => set({ keepCreatorContact: e.target.checked })}
              />
              {t('studio.export.metadata.keepCreatorContact')}
            </label>
            <label className="studio-proof-sheet-check">
              <input
                type="checkbox"
                checked={draft.stripGps}
                disabled={!draft.metadata}
                onChange={(e) => set({ stripGps: e.target.checked })}
              />
              {t('contextMenus.editWith.stripGps')}
            </label>
          </fieldset>
        </div>
        {problem && (
          <p className="studio-dialog-failure" role="alert">
            {problem}
          </p>
        )}
        <footer className="studio-dialog-footer">
          {own && (
            <button type="button" className="studio-button" disabled={busy} onClick={() => void store(null)}>
              {t('contextMenus.editWith.useDefault')}
            </button>
          )}
          <div className="studio-dialog-footer-end">
            <button type="button" className="studio-button" onClick={close}>
              {t('contextMenus.editWith.cancel')}
            </button>
            <button
              type="button"
              className="studio-button is-primary"
              disabled={busy}
              onClick={() => void store(programSettings(shown.program, shown.name, draft))}
            >
              {t('contextMenus.editWith.save')}
            </button>
          </div>
        </footer>
      </div>
    </div>,
    document.body
  );
}
