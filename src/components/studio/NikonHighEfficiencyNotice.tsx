import { useTranslation } from 'react-i18next';

export default function NikonHighEfficiencyNotice({ visible }: { visible?: boolean }) {
  const { t } = useTranslation();
  if (!visible) return null;
  return (
    <div className="studio-raw-preview-notice" role="status">
      {t('studio.loadError.nikonHighEfficiency')}
    </div>
  );
}
