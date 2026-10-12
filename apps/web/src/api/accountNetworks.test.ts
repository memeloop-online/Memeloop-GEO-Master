import { beforeEach, expect, it, vi } from "vitest";
import { apiFetch } from "./client";
import { getAccountNetwork, saveAccountNetwork } from "./accountNetworks";

vi.mock("./client", () => ({ apiFetch: vi.fn() }));
beforeEach(() => vi.clearAllMocks());

it("reads the account binding without tenant inventory selectors", async () => {
  await getAccountNetwork("account/a");
  expect(apiFetch).toHaveBeenCalledExactlyOnceWith(
    "/operator/channel-accounts/account%2Fa/network-binding",
  );
});

it("writes a CAS binding without generic request hashing or implicit retry", async () => {
  const body = {
    expected_version: 0,
    source: {
      kind: "standalone" as const,
      proxy: {
        server: "http://proxy.example:80",
        password: "synthetic-secret",
      },
    },
  };
  await saveAccountNetwork("account-a", body);
  expect(apiFetch).toHaveBeenCalledExactlyOnceWith(
    "/operator/channel-accounts/account-a/network-binding",
    { method: "PUT", body, idempotency: "omit" },
  );
});
