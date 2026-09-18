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

export function LoadingState({
  label = "正在加载",
  compact = false,
}: {
  label?: string;
  compact?: boolean;
}) {
  if (compact) {
    return (
      <Skeleton aria-label={label}>
        <SkeletonItem size={16} />
      </Skeleton>
    );
  }
  return (
    <section className="async-state async-state-loading" aria-live="polite">
      <Skeleton aria-label={label}>
        <SkeletonItem size={32} />
        <SkeletonItem size={16} />
        <SkeletonItem size={16} />
      </Skeleton>
      <span>{label}</span>
    </section>
  );
}

export function ErrorState({
  title = "暂时无法加载",
  detail = "请检查连接后重试。",
  onRetry,
  intent = "error",
}: {
  title?: string;
  detail?: ReactNode;
  onRetry?: () => void;
  intent?: MessageBarIntent;
}) {
  return (
    <MessageBar intent={intent} className="async-error" aria-live="assertive">
      <MessageBarBody>
        <b>{title}</b>
        <span>{detail}</span>
      </MessageBarBody>
      {onRetry && (
        <Button
          appearance="subtle"
          icon={<ArrowSyncRegular />}
          onClick={onRetry}
        >
          重试
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
  detail = "你已登录，但没有访问此工作区的权限。",
  action,
}: {
  tenantName?: string;
  detail?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <Card className="async-state async-unauthorized" role="alert">
      <LockClosedRegular aria-hidden="true" fontSize={28} />
      <h1>权限不足</h1>
      <p>{tenantName ? `无法访问“${tenantName}”。${detail}` : detail}</p>
      {action}
    </Card>
  );
}
