import { Select } from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import { useAppearance } from "../appearance/AppearanceProvider";
import type { UiLocale } from "../api/appearance";

export function LanguageSelect() {
  const { t } = useTranslation();
  const { locale, chooseLocale } = useAppearance();
  return (
    <Select
      aria-label={t("language.label")}
      value={locale}
      onChange={(event) => chooseLocale(event.target.value as UiLocale)}
      size="small"
    >
      <option value="zh-CN">{t("language.zh")}</option>
      <option value="en">{t("language.en")}</option>
    </Select>
  );
}
