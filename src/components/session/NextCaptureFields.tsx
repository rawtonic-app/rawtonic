import { useTranslation } from 'react-i18next';
import IngestMetadata from './IngestMetadata';
import type { NextCaptureForm, PresetChoice } from './nextCapture';

/** What new captures start with (Capture One's Next Capture Adjustments
 * and Metadata): adjustments, rating, keywords and the import's metadata preset — for the hot folder and
 * the tether alike. */
export default function NextCaptureFields({
  value,
  onChange,
  presets,
  onMetadataSavingChange
}: {
  value: NextCaptureForm;
  onChange: (patch: Partial<NextCaptureForm>) => void;
  presets: PresetChoice[];
  onMetadataSavingChange?: (saving: boolean) => void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <label className="session-field">
        {t('session.hotFolder.nextCapture')}
        <select
          aria-label={t('session.hotFolder.nextCapture')}
          value={value.choice}
          onChange={(e) => onChange({ choice: e.target.value })}
        >
          <option value="defaults">{t('session.hotFolder.defaults')}</option>
          <option value="copyFromLast">{t('session.hotFolder.copyFromLast')}</option>
          {presets.map((p) => (
            <option key={p.id} value={`preset:${p.id}`}>
              {t('session.hotFolder.preset', { name: p.name })}
            </option>
          ))}
        </select>
      </label>
      <label className="session-field">
        {t('session.hotFolder.rating')}
        <select
          aria-label={t('session.hotFolder.rating')}
          value={value.rating}
          onChange={(e) => onChange({ rating: Number(e.target.value) })}
        >
          <option value={0}>{t('session.hotFolder.noRating')}</option>
          {[1, 2, 3, 4, 5].map((stars) => (
            <option key={stars} value={stars}>
              {'★'.repeat(stars)}
            </option>
          ))}
        </select>
      </label>
      <label className="session-field">
        {t('session.hotFolder.keywords')}
        <input
          aria-label={t('session.hotFolder.keywords')}
          value={value.keywords}
          placeholder={t('session.hotFolder.keywordsPlaceholder')}
          spellCheck={false}
          onChange={(e) => onChange({ keywords: e.target.value })}
        />
      </label>
      <IngestMetadata
        value={value.import}
        onChange={(next) => onChange({ import: next })}
        onSavingChange={onMetadataSavingChange}
      />
    </>
  );
}
