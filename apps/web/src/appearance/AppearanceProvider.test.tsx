import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { AppearanceProvider, appearanceTheme } from "./AppearanceProvider";
import { Brand } from "../components/Brand";
import { LanguageSelect } from "../components/LanguageSelect";
import { useAppearance } from "./AppearanceProvider";
import i18n from "../i18n";

const appearance = {
  display_name: "Example Operator",
  logo_url: null,
  primary_color: "#7340A2",
  default_locale: "en",
  revision: 3,
};

beforeEach(() => {
  const values = new Map<string, string>();
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
      clear: () => values.clear(),
    },
  });
});

afterEach(async () => {
  cleanup();
  window.localStorage.clear();
  vi.unstubAllGlobals();
  await i18n.changeLanguage("zh-CN");
});

function Selection() {
  const { locale } = useAppearance();
  return <output data-testid="locale">{locale}</output>;
}

describe("host-scoped public appearance", () => {
  it("loads the operator brand before login without any session or project identity", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        new Response(JSON.stringify(appearance), { status: 200 }),
      );
    vi.stubGlobal("fetch", fetchMock);
    render(
      <AppearanceProvider>
        <Brand />
        <Selection />
      </AppearanceProvider>,
    );
    expect(await screen.findByText("Example Operator")).toBeInTheDocument();
    // The brand render precedes the effect that updates document metadata.
    await waitFor(() => {
      expect(document.title).toBe("Example Operator");
      expect(document.documentElement.lang).toBe("en");
      expect(
        document.documentElement.style.getPropertyValue("--oem-primary-color"),
      ).toBe("#7340A2");
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/v1/public/appearance");
    expect(screen.getByTestId("locale")).toHaveTextContent("en");
  });

  it("does not expose stale branding when an older API returns no appearance", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response(null, { status: 404 })),
    );
    render(
      <AppearanceProvider>
        <Brand />
      </AppearanceProvider>,
    );
    await waitFor(() => expect(document.title).toBe("GEO"));
    expect(screen.getByText("GEO")).toBeInTheDocument();
    expect(
      document.documentElement.style.getPropertyValue("--oem-primary-color"),
    ).toBe("#2563EB");
  });

  it("keeps a user interface language choice independent of operator default", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response(JSON.stringify(appearance))),
    );
    render(
      <AppearanceProvider>
        <LanguageSelect />
        <Selection />
      </AppearanceProvider>,
    );
    await waitFor(() =>
      expect(screen.getByTestId("locale")).toHaveTextContent("en"),
    );
    fireEvent.change(screen.getByRole("combobox"), {
      target: { value: "zh-CN" },
    });
    expect(screen.getByTestId("locale")).toHaveTextContent("zh-CN");
    expect(window.localStorage.getItem(`geo-ui-locale:${location.host}`)).toBe(
      "zh-CN",
    );
    expect(appearance.default_locale).toBe("en");
  });

  it("constructs a Fluent theme from the host's primary color", () => {
    expect(appearanceTheme("#7340A2").colorBrandBackground).toBeTruthy();
  });
});
