import { invoke } from '@tauri-apps/api/core';
import i18n from 'i18next';
import { notify } from '../activity/notify';
import { Invokes } from '../components/ui/AppProperties';
import { useSettingsStore } from '../store/useSettingsStore';
import { exportSettingsFromDraft, subfolderName, type RecipeDraft } from './exportRecipes';

/** A durable description of the settings chosen when the job was queued. */
export function exportJobLabel(name: string, draft: RecipeDraft, format: string, destination: string, paths: string[] = []): string {
  const resize = exportSettingsFromDraft(draft).settings.resize;
  const size = resize
    ? `${i18n.t(`studio.export.sizing.modes.${draft.resizeMode}`)} ${resize.value}${resize.mode === 'dimensions' ? ` × ${resize.secondValue}` : ''} ${resize.mode === 'percentage' ? '%' : resize.mode === 'megapixels' ? 'MP' : 'px'}`
    : i18n.t('studio.export.queue.fullResolution');
  const folder = draft.where === 'original' ? i18n.t('studio.export.queue.originalFolder') : destination;
  const sourcePath = paths[0]?.split('?vc=')[0];
  const source = paths.length === 1 ? sourcePath?.split(/[\\/]/).pop() : paths.length ? i18n.t('studio.export.queue.photos', { count: paths.length }) : null;
  const subfolder = subfolderName(draft.subfolder);
  return [source, name, format.toUpperCase(), size, subfolder ? `${folder} / ${subfolder}` : folder].filter(Boolean).join(' · ');
}

/** The containing folder, accepting both native path separators. */
export function outputFolder(output: string): string {
  const separator = Math.max(output.lastIndexOf('/'), output.lastIndexOf('\\'));
  if (separator === 2 && output[1] === ':') return output.slice(0, 3);
  return separator > 0 ? output.slice(0, separator) : output.slice(0, separator + 1) || output;
}

/** One notification per run, with one reveal action for each output folder. */
export function notifyExportOutputs(outputs: string[]) {
  const folders = new Map<string, string>();
  for (const output of outputs) {
    const folder = outputFolder(output);
    if (!folders.has(folder)) folders.set(folder, output);
  }
  if (!folders.size) return;
  notify.success(
    <div>
      <div>{i18n.t('studio.export.queue.exportedTo')}</div>
      {[...folders].map(([folder, output]) => (
        <div key={folder}>
          <span>{folder}</span>{' '}
          <button type="button" onClick={() => void invoke(Invokes.ShowInFinder, { path: output }).catch((error) => notify.error(String(error)))}>
            {i18n.t(useSettingsStore.getState().osPlatform === 'macos' ? 'studio.library.folders.menu.showInFinder' : 'studio.library.folders.menu.showInFileManager')}
          </button>
        </div>
      ))}
    </div>,
    { closeOnClick: false },
  );
}
