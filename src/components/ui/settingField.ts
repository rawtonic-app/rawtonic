import { createContext, useContext } from 'react';

/** The ids of a preference row's name and description (SettingItem), so the
 * control in the row is named by the setting rather than by its value. */
export interface SettingField {
  labelId: string;
  descriptionId?: string;
}

export const SettingFieldContext = createContext<SettingField | null>(null);

export const useSettingField = () => useContext(SettingFieldContext);
