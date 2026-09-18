import { Badge } from "@fluentui/react-components";
import {
  CheckmarkCircleRegular,
  ClockRegular,
  ErrorCircleRegular,
  WarningRegular,
} from "@fluentui/react-icons";
import type { ReactElement } from "react";

export type StatusKind =
  | "complete"
  | "active"
  | "blocked"
  | "queued"
  | "verified"
  | "running"
  | "uncertain";

const statusMeta: Record<
  StatusKind,
  {
    text: string;
    color: "success" | "brand" | "danger" | "warning" | "informative";
    icon: ReactElement;
  }
> = {
  complete: {
    text: "已完成",
    color: "success",
    icon: <CheckmarkCircleRegular />,
  },
  active: { text: "进行中", color: "brand", icon: <ClockRegular /> },
  blocked: { text: "已阻断", color: "danger", icon: <ErrorCircleRegular /> },
  queued: { text: "等待中", color: "informative", icon: <ClockRegular /> },
  verified: {
    text: "已验证",
    color: "success",
    icon: <CheckmarkCircleRegular />,
  },
  running: { text: "运行中", color: "brand", icon: <ClockRegular /> },
  uncertain: { text: "结果待确认", color: "warning", icon: <WarningRegular /> },
};

export function StatusPill({
  status,
  text,
}: {
  status: StatusKind;
  text?: string;
}) {
  const meta = statusMeta[status];
  return (
    <Badge appearance="tint" color={meta.color} icon={meta.icon}>
      {text ?? meta.text}
    </Badge>
  );
}
