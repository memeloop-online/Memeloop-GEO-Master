import {
  FluentProvider,
  createLightTheme,
  type BrandVariants,
} from "@fluentui/react-components";
import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import {
  genericAppearance,
  getPublicAppearance,
  type Appearance,
  type UiLocale,
} from "../api/appearance";
import i18n from "../i18n";

type AppearanceContextValue = {
  appearance: Appearance;
  locale: UiLocale;
  chooseLocale: (locale: UiLocale) => void;
  refreshAppearance: () => Promise<void>;
};
const AppearanceContext = createContext<AppearanceContextValue>({
  appearance: genericAppearance,
  locale: "zh-CN",
  chooseLocale: () => undefined,
  refreshAppearance: async () => undefined,
});

function localePreferenceKey() {
  // A UI-only preference. No identity, customer content, or appearance is stored.
  return `geo-ui-locale:${window.location.host}`;
}

function storedLocale(): UiLocale | undefined {
  try {
    const choice = window.localStorage.getItem(localePreferenceKey());
    return choice === "en" || choice === "zh-CN" ? choice : undefined;
  } catch {
    return undefined;
  }
}

function blend(primary: string, target: number, amount: number) {
  const channels = [1, 3, 5].map((offset) =>
    parseInt(primary.slice(offset, offset + 2), 16),
  );
  return `#${channels
    .map((channel) =>
      Math.round(channel * (1 - amount) + target * amount)
        .toString(16)
        .padStart(2, "0"),
    )
    .join("")}`;
}

export function appearanceTheme(primaryColor: string) {
  const shades: BrandVariants = {
    10: blend(primaryColor, 0, 0.78),
    20: blend(primaryColor, 0, 0.69),
    30: blend(primaryColor, 0, 0.6),
    40: blend(primaryColor, 0, 0.52),
    50: blend(primaryColor, 0, 0.44),
    60: blend(primaryColor, 0, 0.36),
    70: blend(primaryColor, 0, 0.28),
    80: blend(primaryColor, 0, 0.18),
    90: blend(primaryColor, 0, 0.08),
    100: primaryColor,
    110: blend(primaryColor, 255, 0.12),
    120: blend(primaryColor, 255, 0.25),
    130: blend(primaryColor, 255, 0.38),
    140: blend(primaryColor, 255, 0.53),
    150: blend(primaryColor, 255, 0.7),
    160: blend(primaryColor, 255, 0.85),
  };
  return createLightTheme(shades);
}

function applyHeadAppearance(appearance: Appearance, locale: UiLocale) {
  document.title = appearance.display_name;
  document.documentElement.lang = locale;
  document
    .querySelector('meta[name="theme-color"]')
    ?.setAttribute("content", appearance.primary_color);
  document.documentElement.style.setProperty(
    "--oem-primary-color",
    appearance.primary_color,
  );
}

export function AppearanceProvider({ children }: { children: ReactNode }) {
  const [appearance, setAppearance] = useState(genericAppearance);
  const [choice, setChoice] = useState<UiLocale | undefined>(storedLocale);
  const locale = choice ?? appearance.default_locale;

  async function refreshAppearance() {
    try {
      setAppearance(await getPublicAppearance());
    } catch {
      // Old/unavailable API: never reuse appearance from another host.
      setAppearance(genericAppearance);
    }
  }

  useEffect(() => {
    let active = true;
    getPublicAppearance().then(
      (next) => {
        if (active) setAppearance(next);
      },
      () => {
        if (active) setAppearance(genericAppearance);
      },
    );
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    applyHeadAppearance(appearance, locale);
    void i18n.changeLanguage(locale);
  }, [appearance, locale]);

  const context = useMemo(
    () => ({
      appearance,
      locale,
      chooseLocale: (next: UiLocale) => {
        setChoice(next);
        try {
          window.localStorage.setItem(localePreferenceKey(), next);
        } catch {
          // Browsers that disable storage still allow in-memory choice.
        }
      },
      refreshAppearance,
    }),
    [appearance, locale],
  );
  const theme = useMemo(
    () => appearanceTheme(appearance.primary_color),
    [appearance.primary_color],
  );

  return (
    <AppearanceContext.Provider value={context}>
      <FluentProvider theme={theme}>{children}</FluentProvider>
    </AppearanceContext.Provider>
  );
}

export function useAppearance() {
  return useContext(AppearanceContext);
}
