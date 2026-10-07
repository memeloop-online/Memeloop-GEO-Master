import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  Button,
  Card,
  Field,
  MessageBar,
  MessageBarBody,
  Select,
} from "@fluentui/react-components";
import { Link, useSearchParams } from "react-router-dom";
import { safeOriginalPublicUrl } from "../pages/PublicationLookupPanel";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor } from "../auth/types";
import { createIdempotencyKey } from "../api/client";
import {
  listChannelAccounts,
  listChannelPlatforms,
  listProjectConnectorCapabilities,
  type ChannelAccount,
  type ChannelPlatform,
  type ProjectConnectorCapability,
} from "../api/channels";
import type { ContentRevision } from "../api/content";
import {
  distributionFormat,
  getContentDistributionPublication,
  getContentDistributionRequest,
  submitContentDistributionRequest,
  type ContentDistributionInput,
  type ContentDistributionRequest,
} from "../api/contentDistribution";
import "../i18n/contentDistribution";

const requiredCapability = {
  "markdown.v1": "plain_text_article.v1",
  "rich_markdown.v2": "rich_markdown.v2",
} as const;

interface PendingRequest {
  input: ContentDistributionInput;
  key: string;
}

function readPending(
  storageKey: string | null,
  assetId: string,
): PendingRequest | null {
  if (!storageKey) return null;
  try {
    const raw = window.sessionStorage.getItem(storageKey);
    if (!raw) return null;
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object") return null;
    const record = value as {
      input?: Partial<ContentDistributionInput>;
      key?: unknown;
    };
    if (
      typeof record.key !== "string" ||
      !record.key ||
      record.key.length > 256 ||
      record.input?.content_asset_id !== assetId ||
      typeof record.input.content_revision_id !== "string" ||
      typeof record.input.account_id !== "string" ||
      record.input.placement_slot !== "primary" ||
      !["markdown.v1", "rich_markdown.v2"].includes(record.input.format ?? "")
    )
      return null;
    return { input: record.input as ContentDistributionInput, key: record.key };
  } catch {
    return null;
  }
}

function savePending(
  storageKey: string | null,
  pending: PendingRequest,
): boolean {
  if (!storageKey) return false;
  try {
    window.sessionStorage.setItem(storageKey, JSON.stringify(pending));
    return true;
  } catch {
    return false;
  }
}

function removePending(storageKey: string | null) {
  if (!storageKey) return;
  try {
    window.sessionStorage.removeItem(storageKey);
  } catch {
    // The request ID in the URL still prevents an automatic repeat.
  }
}

function pendingMatchesReceipt(
  pending: PendingRequest,
  receipt: ContentDistributionRequest,
): boolean {
  return (
    pending.input.content_asset_id === receipt.content_asset_id &&
    pending.input.content_revision_id === receipt.content_revision_id &&
    pending.input.account_id === receipt.account_id &&
    pending.input.placement_slot === receipt.placement_slot &&
    pending.input.format === receipt.format
  );
}

export function eligiblePublishingAccounts(
  accounts: ChannelAccount[],
  platforms: ChannelPlatform[],
  capabilities: ProjectConnectorCapability[],
  revision: ContentRevision,
): ChannelAccount[] {
  const publishing = new Set(
    platforms
      .filter((platform) => platform.purpose === "publishing")
      .map((platform) => platform.id),
  );
  const required = requiredCapability[distributionFormat(revision)];
  const supported = new Set(
    capabilities
      .filter(
        (capability) =>
          capability.placement_slot === "primary" &&
          capability.availability === "available" &&
          capability.enabled &&
          capability.content_types.includes(required),
      )
      .map((capability) => capability.platform_id),
  );
  return accounts.filter(
    (account) =>
      account.enabled &&
      account.status === "ready" &&
      publishing.has(account.platform) &&
      supported.has(account.platform),
  );
}

interface Props {
  tenantId: string;
  projectId: string;
  assetId: string;
  revision: ContentRevision;
  readonly: boolean;
  unsaved: boolean;
  onSelectRevision?: (revisionId: string) => void;
}

export function ContentDistributionPanel({
  tenantId,
  projectId,
  assetId,
  revision,
  readonly,
  unsaved,
  onSelectRevision,
}: Props) {
  const { t } = useTranslation();
  const { session } = useAuth();
  const [searchParams, setSearchParams] = useSearchParams();
  const requestId = searchParams.get("distribution_request_id");
  const scope = session && queryScopeFor(session, tenantId, projectId);
  const scopeKey = scope
    ? [scope.userId, scope.operatorId, scope.tenantId, scope.projectId]
    : ["anonymous", tenantId, projectId];
  const storageKey = scope
    ? `content-distribution-pending:${JSON.stringify([
        scope.userId,
        scope.operatorId,
        tenantId,
        projectId,
        assetId,
      ])}`
    : null;
  const [pending, setPending] = useState<PendingRequest | null>(() =>
    readPending(storageKey, assetId),
  );
  const [storageUnavailable, setStorageUnavailable] = useState(false);
  useEffect(() => {
    setPending(readPending(storageKey, assetId));
    setStorageUnavailable(false);
  }, [storageKey, assetId]);
  const accounts = useQuery({
    queryKey: ["channel-accounts", ...scopeKey],
    queryFn: () => listChannelAccounts(tenantId, projectId),
    enabled: Boolean(scope && !requestId),
  });
  const platforms = useQuery({
    queryKey: ["channel-platforms"],
    queryFn: listChannelPlatforms,
    enabled: Boolean(scope && !requestId),
  });
  const capabilities = useQuery({
    queryKey: ["project-connector-capabilities", ...scopeKey],
    queryFn: () => listProjectConnectorCapabilities(tenantId, projectId),
    enabled: Boolean(scope && !requestId),
  });
  const receipt = useQuery({
    queryKey: [
      "content-distribution-request",
      ...scopeKey,
      projectId,
      requestId,
    ],
    queryFn: () =>
      getContentDistributionRequest(tenantId, projectId, requestId!),
    enabled: Boolean(scope && requestId),
    retry: false,
  });
  const validReceipt = Boolean(
    receipt.data &&
    receipt.data.content_asset_id === assetId &&
    receipt.data.scope.project_id === projectId,
  );
  useEffect(() => {
    if (
      validReceipt &&
      receipt.data &&
      (!pending || pendingMatchesReceipt(pending, receipt.data))
    ) {
      removePending(storageKey);
      if (pending) setPending(null);
    }
  }, [validReceipt, storageKey, pending, receipt.data]);
  const publication = useQuery({
    queryKey: [
      "content-distribution-publication",
      ...scopeKey,
      projectId,
      requestId,
    ],
    queryFn: () =>
      getContentDistributionPublication(tenantId, projectId, requestId!),
    enabled: Boolean(scope && requestId && validReceipt),
    retry: false,
    refetchInterval: (query) =>
      query.state.data?.outcome === "verified" ? false : 5000,
  });
  const [accountId, setAccountId] = useState("");
  const mutation = useMutation({
    mutationFn: ({
      input,
      key,
    }: {
      input: ContentDistributionInput;
      key: string;
    }) => submitContentDistributionRequest(tenantId, projectId, input, key),
    onSuccess: (accepted) => {
      removePending(storageKey);
      setPending(null);
      setSearchParams(
        (current) => {
          const next = new URLSearchParams(current);
          next.set("distribution_request_id", accepted.request_id);
          return next;
        },
        { replace: true },
      );
    },
  });
  const options =
    accounts.data && platforms.data && capabilities.data
      ? eligiblePublishingAccounts(
          accounts.data.items,
          platforms.data.items,
          capabilities.data.items,
          revision,
        )
      : [];
  const selectedAccount = options.find(
    (account) => account.account_id === accountId,
  );
  const submit = () => {
    if (readonly || mutation.isPending || requestId) return;
    if (pending) {
      mutation.mutate(pending);
      return;
    }
    if (!selectedAccount) return;
    const next = {
      key: createIdempotencyKey(),
      input: {
        content_asset_id: assetId,
        content_revision_id: revision.revision_id,
        account_id: selectedAccount.account_id,
        placement_slot: "primary",
        format: distributionFormat(revision),
      },
    };
    // Fail closed if this tab cannot keep the retry key across a lost response.
    if (!savePending(storageKey, next)) {
      setStorageUnavailable(true);
      return;
    }
    setStorageUnavailable(false);
    setPending(next);
    mutation.mutate(next);
  };
  const startNewRequest = () => {
    removePending(storageKey);
    setPending(null);
    setStorageUnavailable(false);
    mutation.reset();
  };
  const clearReceipt = () => {
    startNewRequest();
    setSearchParams((current) => {
      const next = new URLSearchParams(current);
      next.delete("distribution_request_id");
      return next;
    });
  };
  const publicationKey = (() => {
    if (
      !publication.data ||
      publication.data.request_id !== receipt.data?.request_id
    )
      return null;
    if (publication.data.fixture) return "fixture";
    switch (publication.data.outcome) {
      case "verified":
        return "verified";
      case "published":
        return "published";
      case "unknown":
        return "unknown";
      case "failed":
        return "failedPublication";
      case "login_required":
        return "loginRequired";
      case "unsupported":
        return "unsupportedPublication";
      case null:
        return publication.data.attempt_id ? "inFlight" : "notAttempted";
      default:
        return "unrecognized";
    }
  })();
  const publicUrl =
    publication.data?.request_id === receipt.data?.request_id &&
    !publication.data?.fixture
      ? safeOriginalPublicUrl(publication.data?.public_url ?? null)
      : null;

  return (
    <Card className="panel-card">
      <h2>{t("contentDistribution.title")}</h2>
      {!requestId && !pending && (
        <p>
          {t("contentDistribution.version", { revision: revision.revision })}
        </p>
      )}
      {requestId ? (
        <>
          {receipt.isPending && (
            <p>{t("contentDistribution.loadingReceipt")}</p>
          )}
          {receipt.isError && (
            <MessageBar intent="error">
              <MessageBarBody>
                {t("contentDistribution.receiptUnavailable")}{" "}
                <Button onClick={() => void receipt.refetch()}>
                  {t("contentDistribution.retry")}
                </Button>
              </MessageBarBody>
            </MessageBar>
          )}
          {receipt.data &&
            (!validReceipt ? (
              <MessageBar intent="error">
                <MessageBarBody>
                  {t("contentDistribution.mismatch")}
                </MessageBarBody>
              </MessageBar>
            ) : (
              <>
                {receipt.data.content_revision_id === revision.revision_id && (
                  <p>
                    {t("contentDistribution.version", {
                      revision: revision.revision,
                    })}
                  </p>
                )}
                {receipt.data.content_revision_id !== revision.revision_id && (
                  <MessageBar intent="warning">
                    <MessageBarBody>
                      {t("contentDistribution.differentRevision")}{" "}
                      {onSelectRevision && (
                        <Button
                          onClick={() =>
                            onSelectRevision(receipt.data!.content_revision_id)
                          }
                        >
                          {t("contentDistribution.openRevision")}
                        </Button>
                      )}
                    </MessageBarBody>
                  </MessageBar>
                )}
                <MessageBar intent="info">
                  <MessageBarBody>
                    {t("contentDistribution.accepted")}
                  </MessageBarBody>
                </MessageBar>
                {publication.isPending && (
                  <p>{t("contentDistribution.publicationLoading")}</p>
                )}
                {publication.isError && (
                  <MessageBar intent="warning">
                    <MessageBarBody>
                      {t("contentDistribution.publicationUnavailable")}
                    </MessageBarBody>
                  </MessageBar>
                )}
                {publicationKey && (
                  <p role="status">
                    {t(`contentDistribution.${publicationKey}`)}
                  </p>
                )}
                {publicUrl && (
                  <a href={publicUrl} target="_blank" rel="noopener noreferrer">
                    {t("contentDistribution.openPublicPage")}
                  </a>
                )}
                <Button onClick={() => void publication.refetch()}>
                  {t("contentDistribution.refreshPublication")}
                </Button>
                {publication.data?.channel_target_id && (
                  <details>
                    <summary>{t("contentDistribution.target")}</summary>
                    <p>{publication.data.channel_target_id}</p>
                  </details>
                )}
                <details>
                  <summary>{t("contentDistribution.frozenRevision")}</summary>
                  <p>{receipt.data.content_revision_id}</p>
                </details>
                <details>
                  <summary>{t("contentDistribution.receipt")}</summary>
                  <p>{receipt.data.request_id}</p>
                </details>
                {!readonly && (
                  <Button onClick={clearReceipt}>
                    {t("contentDistribution.newRequest")}
                  </Button>
                )}
              </>
            ))}
        </>
      ) : (
        <>
          {pending ? (
            <>
              <MessageBar intent="warning">
                <MessageBarBody>
                  {t("contentDistribution.failed")}
                </MessageBarBody>
              </MessageBar>
              <details>
                <summary>{t("contentDistribution.frozenRevision")}</summary>
                <p>{pending.input.content_revision_id}</p>
                <p>
                  {t("contentDistribution.account")}: {pending.input.account_id}
                </p>
              </details>
              {!readonly && (
                <>
                  <Button
                    appearance="primary"
                    disabled={mutation.isPending}
                    onClick={submit}
                  >
                    {t(
                      mutation.isPending
                        ? "contentDistribution.sending"
                        : "contentDistribution.retry",
                    )}
                  </Button>
                  <Button
                    disabled={mutation.isPending}
                    onClick={startNewRequest}
                  >
                    {t("contentDistribution.newRequest")}
                  </Button>
                </>
              )}
            </>
          ) : (
            <>
              {unsaved && <p>{t("contentDistribution.unsaved")}</p>}
              {distributionFormat(revision) === "rich_markdown.v2" && (
                <p>{t("contentDistribution.rich")}</p>
              )}
              {(accounts.isPending ||
                platforms.isPending ||
                capabilities.isPending) && (
                <p>{t("contentDistribution.loading")}</p>
              )}
              {(accounts.isError ||
                platforms.isError ||
                capabilities.isError) && (
                <MessageBar intent="error">
                  <MessageBarBody>
                    {t("contentDistribution.unavailable")}
                  </MessageBarBody>
                </MessageBar>
              )}
              {accounts.isSuccess &&
                platforms.isSuccess &&
                capabilities.isSuccess &&
                (options.length ? (
                  <Field label={t("contentDistribution.account")}>
                    <Select
                      value={accountId}
                      disabled={readonly || mutation.isPending}
                      onChange={(event) => {
                        setAccountId(event.target.value);
                        mutation.reset();
                      }}
                    >
                      <option value="">
                        {t("contentDistribution.chooseAccount")}
                      </option>
                      {options.map((account) => (
                        <option
                          key={account.account_id}
                          value={account.account_id}
                        >
                          {account.display_name ??
                            account.platform_account_id ??
                            account.platform}{" "}
                          ·{" "}
                          {platforms.data.items.find(
                            (platform) => platform.id === account.platform,
                          )?.label ?? account.platform}{" "}
                          ·{" "}
                          {t(
                            account.owner_kind === "operator_pool"
                              ? "contentDistribution.shared"
                              : "contentDistribution.own",
                          )}
                        </option>
                      ))}
                    </Select>
                  </Field>
                ) : (
                  <p>
                    {accounts.data.items.some(
                      (account) =>
                        account.enabled &&
                        account.status === "ready" &&
                        platforms.data.items.some(
                          (platform) =>
                            platform.id === account.platform &&
                            platform.purpose === "publishing",
                        ),
                    )
                      ? t("contentDistribution.unsupported")
                      : t("contentDistribution.noAccount")}
                  </p>
                ))}
              {!readonly &&
                accounts.isSuccess &&
                platforms.isSuccess &&
                capabilities.isSuccess &&
                options.length === 0 && (
                  <Link to="../channels">
                    {t("contentDistribution.connect")}
                  </Link>
                )}
              {!readonly && options.length > 0 && (
                <Button
                  appearance="primary"
                  disabled={!selectedAccount || mutation.isPending}
                  onClick={submit}
                >
                  {mutation.isPending
                    ? t("contentDistribution.sending")
                    : t("contentDistribution.send")}
                </Button>
              )}
              {storageUnavailable && (
                <MessageBar intent="error">
                  <MessageBarBody>
                    {t("contentDistribution.failed")}
                  </MessageBarBody>
                </MessageBar>
              )}
            </>
          )}
        </>
      )}
    </Card>
  );
}
