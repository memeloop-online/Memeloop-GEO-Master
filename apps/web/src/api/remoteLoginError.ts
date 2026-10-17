import { ApiError } from "./client";
import i18n from "../i18n";

export function remoteLoginErrorMessage(error: unknown): string | undefined {
  if (
    !(error instanceof ApiError) ||
    !error.details ||
    typeof error.details !== "object" ||
    !("reason" in error.details)
  )
    return undefined;
  if (
    error.code === "capability_missing" &&
    error.details.reason === "browser_login_runtime_missing"
  )
    return i18n.t("account.remote.runtimeMissingDetail");
  if (
    error.code === "dependency_unavailable" &&
    error.details.reason === "desktop_unavailable"
  )
    return i18n.t("account.remote.temporarilyUnavailableDetail");
  return undefined;
}
