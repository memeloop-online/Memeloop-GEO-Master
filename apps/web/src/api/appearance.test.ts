import { afterEach, describe, expect, it, vi } from "vitest";
import {
  genericAppearance,
  getPublicAppearance,
  parseAppearance,
  updateOperatorAppearance,
} from "./appearance";
import { setCsrfToken } from "./client";

const valid = {
  ...genericAppearance,
  display_name: "Example Operator",
  revision: 7,
};

afterEach(() => {
  setCsrfToken(undefined);
  vi.unstubAllGlobals();
});

describe("appearance API", () => {
  it("rejects invalid public appearance payloads instead of using them as branding", () => {
    expect(() =>
      parseAppearance({ ...valid, primary_color: "url(foo)" }),
    ).toThrow();
    expect(() => parseAppearance({ ...valid, default_locale: "fr" })).toThrow();
    expect(() =>
      parseAppearance({ ...valid, logo_url: "https://other.example/logo" }),
    ).not.toThrow();
  });

  it("allows an anonymous host-resolved read and avoids handling 401 as logout", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(JSON.stringify(valid)));
    vi.stubGlobal("fetch", fetchMock);
    expect(await getPublicAppearance()).toEqual(valid);
    expect(fetchMock.mock.calls[0][1]).toMatchObject({
      credentials: "same-origin",
      cache: "no-store",
    });
  });

  it("sends a strong revision precondition and CSRF token for updates", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(JSON.stringify(valid)));
    vi.stubGlobal("fetch", fetchMock);
    setCsrfToken("test-csrf");
    await updateOperatorAppearance(
      {
        display_name: "Example Operator",
        primary_color: "#2563EB",
        default_locale: "en",
      },
      6,
    );
    const [, request] = fetchMock.mock.calls[0] as [string, RequestInit];
    const headers = new Headers(request.headers);
    expect(headers.get("If-Match")).toBe('"6"');
    expect(headers.get("X-CSRF-Token")).toBe("test-csrf");
    expect(request.body).toBe(
      JSON.stringify({
        display_name: "Example Operator",
        primary_color: "#2563EB",
        default_locale: "en",
      }),
    );
  });
});
