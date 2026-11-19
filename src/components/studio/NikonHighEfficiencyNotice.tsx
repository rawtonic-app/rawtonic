import { useTranslation } from 'react-i18next';
import { useSettingsStore } from '../../store/useSettingsStore';
import { canSuggestAppleRaw9, nikonHighEfficiencyMessage } from '../../utils/loadError';

export default function NikonHighEfficiencyNotice({ visible }: { visible?: boolean }) {
  const { t } = useTranslation();
  const suggestAppleRaw9 = useSettingsStore(canSuggestAppleRaw9);
  if (!visible) return null;
  return (
    <div className="studio-raw-preview-notice" role="status">
      {nikonHighEfficiencyMessage(t, suggestAppleRaw9)}
    </div>
  );
}
