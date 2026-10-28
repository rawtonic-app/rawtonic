import { useId, useMemo } from 'react';
import { SettingFieldContext } from '../ui/settingField';

interface SettingItemProps {
  children: React.ReactNode;
  description?: string;
  label: string;
}

// The row's name and description name and describe its control (Dropdown,
// Switch, Slider read them from SettingFieldContext).
const SettingItem = ({ children, description, label }: SettingItemProps) => {
  const id = useId();
  const field = useMemo(
    () => ({ labelId: `${id}-label`, descriptionId: description ? `${id}-description` : undefined }),
    [id, description],
  );
  return (
    <div className="pref-row">
      <div className="pref-text">
        <div className="pref-label" id={field.labelId}>
          {label}
        </div>
        {description && (
          <p className="pref-description" id={field.descriptionId}>
            {description}
          </p>
        )}
      </div>
      <div className="pref-control">
        <SettingFieldContext.Provider value={field}>{children}</SettingFieldContext.Provider>
      </div>
    </div>
  );
};

export default SettingItem;
