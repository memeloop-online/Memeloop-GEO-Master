import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@fluentui/react-components";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearchParams } from "react-router-dom";
import { createIdempotencyKey } from "../api/client";
import { createProject, projectQueryKeys } from "../api/projects";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant, queryScopeFor } from "../auth/types";
import {
  ErrorState,
  LoadingState,
  UnauthorizedState,
} from "../components/AsyncState";
import { WorkspacePage } from "./WorkspacePage";

export function SetupEntry() {
  const { session } = useAuth();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const tenantId = searchParams.get("tenant_id") ?? undefined;

  if (!tenantId) return <WorkspacePage />;
  if (!membershipForTenant(session, tenantId)) {
    return (
      <main className="guard-page">
        <UnauthorizedState
          tenantName={tenantId}
          action={
            <Button
              onClick={() => navigate("/workspaces")}
              appearance="primary"
            >
              选择其他工作区
            </Button>
          }
        />
      </main>
    );
  }
  return <ChatFirstProjectEntry key={tenantId} tenantId={tenantId} />;
}

interface PendingEntry {
  projectKey: string;
  projectId?: string;
}

function ChatFirstProjectEntry({ tenantId }: { tenantId: string }) {
  const { session } = useAuth();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const inFlight = useRef(false);
  const pending = useRef<PendingEntry | undefined>(undefined);
  const [failed, setFailed] = useState(false);
  const storageKey = JSON.stringify([
    "chat-first-project",
    session?.user.id,
    session?.operator.id,
    tenantId,
  ]);

  const enter = useCallback(async () => {
    if (!session || inFlight.current) return;
    inFlight.current = true;
    setFailed(false);
    try {
      if (!pending.current) {
        try {
          const stored: unknown = JSON.parse(
            sessionStorage.getItem(storageKey) ?? "null",
          );
          if (
            stored &&
            typeof stored === "object" &&
            "projectKey" in stored &&
            typeof stored.projectKey === "string" &&
            (!("projectId" in stored) || typeof stored.projectId === "string")
          )
            pending.current = stored as PendingEntry;
        } catch {
          // Storage can be unavailable; the mounted entry still retains retry keys.
        }
        pending.current ??= {
          projectKey: createIdempotencyKey(),
        };
      }
      const entry = pending.current;
      const persist = () => {
        try {
          sessionStorage.setItem(storageKey, JSON.stringify(entry));
        } catch {
          // Browser storage restrictions do not block opening a conversation.
        }
      };
      persist();
      if (!entry.projectId) {
        const project = await createProject(
          tenantId,
          { display_name: "新项目", settings: {} },
          entry.projectKey,
        );
        entry.projectId = project.id;
        persist();
      }
      void queryClient.invalidateQueries({
        queryKey: projectQueryKeys.list(queryScopeFor(session, tenantId)),
      });
      navigate(
        `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(entry.projectId)}/chat`,
        { replace: true },
      );
      try {
        sessionStorage.removeItem(storageKey);
      } catch {
        // The completed entry is already open.
      }
    } catch {
      setFailed(true);
    } finally {
      inFlight.current = false;
    }
  }, [navigate, queryClient, session, storageKey, tenantId]);

  useEffect(() => {
    void enter();
  }, [enter]);

  return (
    <main className="workspace-page">
      {failed ? (
        <ErrorState
          title="暂时无法打开项目对话"
          detail="重试会继续打开同一个项目，不会重新创建项目或启动付费任务。"
          onRetry={() => void enter()}
        />
      ) : (
        <LoadingState label="正在准备项目对话，无需填写表单…" />
      )}
    </main>
  );
}
