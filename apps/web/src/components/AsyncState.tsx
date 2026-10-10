import {
  Button,
  Card,
  MessageBar,
  MessageBarBody,
  Skeleton,
  SkeletonItem,
  type MessageBarIntent,
} from "@fluentui/react-components";
import { ArrowSyncRegular, LockClosedRegular } from "@fluentui/react-icons";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import "../i18n";

export function LoadingState({
  label,
  compact = false,
}: {
  label?: string;
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const displayLabel = label ?? t("asyncState.loading");
  if (compact) {
    return (
      <Skeleton aria-label={displayLabel}>
        <SkeletonItem size={16} />
      </Skeleton>
    );
  }
  return (
    <section className="async-state async-state-loading" aria-live="polite">
      <Skeleton aria-label={displayLabel}>
        <SkeletonItem size={32} />
        <SkeletonItem size={16} />
        <SkeletonItem size={16} />
      </Skeleton>
      <span>{displayLabel}</span>
    </section>
  );
}

export function ErrorState({
  title,
  detail,
  onRetry,
  intent = "error",
}: {
  title?: string;
  detail?: ReactNode;
  onRetry?: () => void;
  intent?: MessageBarIntent;
}) {
  const { t } = useTranslation();
  return (
    <MessageBar intent={intent} className="async-error" aria-live="assertive">
      <MessageBarBody>
        <b>{title ?? t("asyncState.loadError")}</b>
        <span>{detail ?? t("asyncState.checkConnection")}</span>
      </MessageBarBody>
      {onRetry && (
        <Button
          appearance="subtle"
          icon={<ArrowSyncRegular />}
          onClick={onRetry}
        >
          {t("asyncState.retry")}
        </Button>
      )}
    </MessageBar>
  );
}

export function EmptyState({
  title,
  detail,
  action,
}: {
  title: string;
  detail: ReactNode;
  action?: ReactNode;
}) {
  return (
    <Card className="async-state async-empty-state">
      <h2>{title}</h2>
      <p>{detail}</p>
      {action}
    </Card>
  );
}

export function UnauthorizedState({
  tenantName,
  detail,
  action,
}: {
  tenantName?: string;
  detail?: ReactNode;
  action?: ReactNode;
}) {
  const { t } = useTranslation();
  return (
    <Card className="async-state async-unauthorized" role="alert">
      <LockClosedRegular aria-hidden="true" fontSize={28} />
      <h1>{t("asyncState.accessDenied")}</h1>
      <p>
        {tenantName && t("asyncState.cannotAccessTenant", { name: tenantName })}
        {detail ?? t("asyncState.workspaceForbidden")}
      </p>
      {action}
    </Card>
  );
}
