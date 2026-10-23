import { useTranslation } from "react-i18next";

/** Backend commands in these dialogs return free-form strings, not error kinds.
 * Keep their diagnostic detail separate from the translated user message. */
export default function OperationError({ detail }: { detail: string }) {
  const { t } = useTranslation();
  return <>
    <span className="block">{t("studio.operationFailed")}</span>
    {detail && <small className="block whitespace-pre-wrap break-words">{detail}</small>}
  </>;
}
