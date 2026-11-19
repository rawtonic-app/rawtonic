import { useTranslation } from 'react-i18next';
import { useSettingsStore } from '../../store/useSettingsStore';
import { nikonHighEfficiencyMessage } from '../../utils/loadError';

export default function NikonHighEfficiencyNotice({ visible }: { visible?: boolean }) {
  const { t } = useTranslation();
  const osPlatform = useSettingsStore((state) => state.osPlatform);
  if (!visible) return null;
  return (
    <div className="studio-raw-preview-notice" role="status">
      {nikonHighEfficiencyMessage(t, osPlatform)}
    </div>
  );
}
