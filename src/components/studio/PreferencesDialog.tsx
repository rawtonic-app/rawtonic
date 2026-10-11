import { useRef, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useModalFocus } from "../../hooks/useModalFocus";

/**
 * Preferences over the workspace: a modal dialog that holds the keyboard
 * focus, makes the workspace behind it inert and gives the focus back to its
 * opener when it closes. Escape is handled by the shortcut handler.
 */
export default function PreferencesDialog({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  useModalFocus(ref, true);
  return (
    <div
      ref={ref}
      className="studio-settings"
      role="dialog"
      aria-modal="true"
      aria-label={t("studio.workspace.preferences")}
      tabIndex={-1}
    >
      {children}
    </div>
  );
}
