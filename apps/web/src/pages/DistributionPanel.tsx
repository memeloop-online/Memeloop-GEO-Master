import { useEffect, useState } from "react";
import {
  Badge,
  Button,
  Card,
  MessageBar,
  MessageBarBody,
} from "@fluentui/react-components";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError } from "../api/client";
import {
  freezeDistributionManifest,
  getCycleDistributionManifest,
  getDistributionManifest,
  getDistributionTargets,
  resumeDistributionManifest,
  type DistributionManifest,
  type DistributionTarget,
  type DistributionTargetStatus,
} from "../api/distribution";
import { getChannelTarget } from "../api/channelJobs";
import { useAuth } from "../auth/AuthProvider";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import { PublicationLookupPanel } from "./PublicationLookupPanel";

const targetLabels: Record<DistributionTargetStatus, string> = {
  pending: "待判定",
  blocked: "已阻断",
  deferred: "已延后",
  not_applicable: "不适用",
  cancelled: "已取消",
  ready: "就绪（非已发布）",
  reused_verified: "复用已有验证记录",
  reused_unknown: "复用既有未知结果，禁止重发",
};

const publicationLabels: Record<string, string> = {
  unknown: "结果未知",
  published: "已发布（未公开验证）",
  verified: "公开读回已验证",
  failed: "明确失败",
  login_required: "需要重新登录",
  unsupported: "暂不支持",
};

const message = (error: unknown) =>
  error instanceof Error ? error.message : "请求失败，请重试。";

function DistributionRow({
  row,
  manifest,
  tenantId,
  projectId,
}: {
  row: DistributionTarget;
  manifest: DistributionManifest;
  tenantId: string;
  projectId: string;
}) {
  const { session } = useAuth();
  const [expanded, setExpanded] = useState(false);
  // A reused intent can belong to an earlier cycle's target. Its original
  // channel target ID is not present in this coverage row.
  const canReadAttempt = Boolean(
    row.publication_intent_id && row.status === "ready",
  );
  const attempt = useQuery({
    queryKey: [
      "channel-target",
      session?.user.id,
      session?.operator.id,
      tenantId,
      projectId,
      row.target_id,
    ],
    queryFn: ({ signal }) =>
      getChannelTarget(tenantId, projectId, row.target_id, signal),
    enabled: canReadAttempt && expanded,
    retry: false,
  });
  const document = manifest.document_roster.find(
    (item) => item.document_item_id === row.document_item_id,
  );
  return (
    <Card className="channel-job-target">
      <div className="channel-job-target-heading">
        <div>
          <h3>
            {document?.document_key ?? row.document_item_id} → {row.platform_id}
          </h3>
          <p>
            {document?.content_type ?? "文档"} · {row.placement_slot} · 第{" "}
            {row.ordinal + 1} 项
          </p>
        </div>
        <Badge appearance="outline">
          {targetLabels[row.status] ?? row.status}
        </Badge>
      </div>
      {row.reason && <p role="status">原因：{row.reason}</p>}
      <p className="channel-job-ids">
        覆盖目标 {row.target_id} · 正文版本{" "}
        {row.content_revision_id ?? "未生成"}
      </p>
      {row.publication_intent_id && (
        <p className="channel-job-ids">
          已登记发布意图 {row.publication_intent_id}
          ；登记或排队不等于发送、发布或公开验证。
        </p>
      )}
      {row.status === "reused_unknown" && (
        <p>历史发送结果仍未知；本周期不会另建新意图盲目重发。</p>
      )}
      {canReadAttempt && (
        <>
          <Button
            appearance="subtle"
            aria-expanded={expanded}
            onClick={() => setExpanded((value) => !value)}
          >
            {expanded ? "收起执行记录" : "查看执行记录与查回"}
          </Button>
          {expanded && (
            <div className="channel-job-attempt">
              {attempt.isPending ? (
                <LoadingState label="正在读取原发送记录" compact />
              ) : attempt.isError ? (
                <ErrorState
                  title="原发送记录无法读取"
                  detail={message(attempt.error)}
                  onRetry={() => void attempt.refetch()}
                />
              ) : attempt.data?.attempts.length ? (
                <>
                  {attempt.data.attempts.map((entry) => (
                    <p key={entry.attempt_id}>
                      原发送尝试 {entry.attempt_id} ·{" "}
                      {!entry.outcome
                        ? "结果未知"
                        : (publicationLabels[entry.outcome.status] ??
                          "结果待核对")}
                    </p>
                  ))}
                  {attempt.data.attempts.some(
                    (entry) =>
                      !entry.outcome || entry.outcome.status === "unknown",
                  ) && (
                    <PublicationLookupPanel
                      tenantId={tenantId}
                      projectId={projectId}
                      targetId={row.target_id}
                    />
                  )}
                </>
              ) : (
                <p>尚无发送尝试；排队不等于已发送。</p>
              )}
            </div>
          )}
        </>
      )}
    </Card>
  );
}

export function DistributionPanel({
  tenantId,
  projectId,
  cycleId,
  canWrite,
}: {
  tenantId: string;
  projectId: string;
  cycleId: string;
  canWrite: boolean;
}) {
  const { session } = useAuth();
  const queryClient = useQueryClient();
  const scopedKey = [
    session?.user.id,
    session?.operator.id,
    tenantId,
    projectId,
  ];
  const cycleKey = ["distribution-cycle", ...scopedKey, cycleId];
  const cycle = useQuery({
    queryKey: cycleKey,
    queryFn: () => getCycleDistributionManifest(tenantId, projectId, cycleId),
    retry: false,
  });
  const manifestId = cycle.data?.manifest_id;
  const manifestKey = ["distribution-manifest", ...scopedKey, manifestId];
  const detail = useQuery({
    queryKey: manifestKey,
    queryFn: () => getDistributionManifest(tenantId, projectId, manifestId!),
    enabled: Boolean(manifestId),
    retry: false,
  });
  const manifest = detail.data ?? cycle.data;
  const [pageCursors, setPageCursors] = useState<Array<number | undefined>>([
    undefined,
  ]);
  const pageIndex = pageCursors.length - 1;
  const afterOrdinal = pageCursors[pageIndex];
  useEffect(() => {
    setPageCursors([undefined]);
  }, [cycleId, manifestId]);
  const pageKey = [
    "distribution-targets",
    ...scopedKey,
    manifestId,
    afterOrdinal,
  ];
  const targets = useQuery({
    queryKey: pageKey,
    queryFn: () =>
      getDistributionTargets(tenantId, projectId, manifestId!, afterOrdinal),
    enabled: Boolean(manifestId),
    retry: false,
  });
  const refresh = () => {
    void cycle.refetch();
    if (manifestId) {
      void detail.refetch();
      void targets.refetch();
    }
  };
  const applyManifest = (next: DistributionManifest) => {
    queryClient.setQueryData(cycleKey, next);
    queryClient.setQueryData(
      ["distribution-manifest", ...scopedKey, next.manifest_id],
      next,
    );
    void queryClient.invalidateQueries({
      queryKey: ["distribution-targets", ...scopedKey, next.manifest_id],
    });
  };
  const freeze = useMutation({
    mutationFn: () => freezeDistributionManifest(tenantId, projectId, cycleId),
    onSuccess: applyManifest,
    onError: refresh,
  });
  const resume = useMutation({
    mutationFn: (cursor?: number) =>
      resumeDistributionManifest(tenantId, projectId, manifestId!, cursor),
    onSuccess: applyManifest,
    onError: refresh,
  });

  return (
    <section className="channel-jobs-section" aria-label="正式文档分发清单">
      <h2>正式文档 × 平台分发清单</h2>
      <p>
        与下方从公开来源版本建立的旧渠道计划彼此独立。本清单从已封存的文档规划及正文交接展开，
        覆盖项、意图和排队记录都不代表真实发布。
      </p>
      {cycle.isPending ? (
        <LoadingState label="正在读取正式分发清单" />
      ) : cycle.isError ? (
        <ErrorState
          title="无法读取正式分发清单"
          detail={message(cycle.error)}
          onRetry={refresh}
        />
      ) : !manifestId ? (
        <>
          <EmptyState
            title="尚未冻结正式分发清单"
            detail="需先封存文档清单并完成本周期正文交接。冻结时服务器按周期配置及平台能力快照确定覆盖分母。"
          />
          {canWrite ? (
            <Button disabled={freeze.isPending} onClick={() => freeze.mutate()}>
              {freeze.isPending ? "正在冻结…" : "冻结正式分发清单"}
            </Button>
          ) : (
            <p role="status">当前角色仅可查看，不能冻结或恢复分发清单。</p>
          )}
          {freeze.isError && (
            <ErrorState
              title="冻结结果未确认"
              detail={
                <>
                  {message(freeze.error)}
                  {freeze.error instanceof ApiError &&
                  freeze.error.status === 409
                    ? "。请检查文档清单、正文交接和项目状态，再刷新核对。"
                    : "。请先刷新核对是否已创建，避免重复操作。"}
                </>
              }
              onRetry={refresh}
            />
          )}
        </>
      ) : (
        <>
          {detail.isPending && (
            <LoadingState label="正在核对分发清单详情" compact />
          )}
          {detail.isError && (
            <ErrorState
              title="清单详情无法读取"
              detail={message(detail.error)}
              onRetry={refresh}
            />
          )}
          {manifest && (
            <>
              <p className="channel-job-ids">
                清单 {manifest.manifest_id} · 修订 {manifest.revision} ·
                文档清单修订 {manifest.document_manifest_revision}
              </p>
              <p role="status">
                冻结分母 {manifest.expected_count} 项 · 已展开{" "}
                {manifest.expansion_cursor} 项 · 尚未展开{" "}
                {Math.max(
                  0,
                  manifest.expected_count - manifest.expansion_cursor,
                )}{" "}
                项 ·{" "}
                {manifest.complete ? "覆盖展开完成" : "部分展开，可继续恢复"}
              </p>
              <MessageBar intent="info">
                <MessageBarBody>
                  就绪与已排队仅指内部准备；真实发送与公开验证须在独立执行证据中核对。
                </MessageBarBody>
              </MessageBar>
              {canWrite && !detail.isError ? (
                <Button
                  disabled={resume.isPending}
                  onClick={() => resume.mutate(afterOrdinal)}
                >
                  {resume.isPending
                    ? "正在恢复…"
                    : pageIndex
                      ? "继续展开／复查当前页起的延后项"
                      : "继续展开／检查延后项"}
                </Button>
              ) : !canWrite ? (
                <p role="status">只读访问：无法恢复分发目标。</p>
              ) : null}
              {resume.isError && (
                <ErrorState
                  title="恢复结果未确认"
                  detail={`${message(resume.error)}。请刷新核对覆盖项后再操作。`}
                  onRetry={refresh}
                />
              )}
              {targets.isPending ? (
                <LoadingState label="正在读取分发目标" />
              ) : targets.isError ? (
                <ErrorState
                  title="分发目标无法读取"
                  detail={message(targets.error)}
                  onRetry={() => void targets.refetch()}
                />
              ) : (
                <>
                  {targets.data?.rows.length ? (
                    <div className="channel-jobs-list">
                      <p>
                        当前页 {targets.data.rows.length} 项（冻结分母{" "}
                        {targets.data.expected_count}）
                      </p>
                      {targets.data.rows.map((row) => (
                        <DistributionRow
                          key={row.target_id}
                          row={row}
                          manifest={manifest}
                          tenantId={tenantId}
                          projectId={projectId}
                        />
                      ))}
                    </div>
                  ) : (
                    <EmptyState
                      title={
                        manifest.expected_count === 0
                          ? "冻结清单没有覆盖目标"
                          : manifest.complete
                            ? "本页没有更多目标"
                            : "本页目标尚未展开"
                      }
                      detail={
                        manifest.complete
                          ? "覆盖记录为空，不代表曾发布任何内容。"
                          : "冻结分母保持不变；继续展开后刷新本页查看。"
                      }
                    />
                  )}
                  <div className="channel-job-target-heading">
                    <Button
                      disabled={pageIndex === 0}
                      onClick={() =>
                        setPageCursors((cursors) => cursors.slice(0, -1))
                      }
                    >
                      上一页
                    </Button>
                    <span>第 {pageIndex + 1} 页</span>
                    <Button
                      disabled={targets.data?.next_ordinal == null}
                      onClick={() => {
                        if (targets.data?.next_ordinal != null)
                          setPageCursors((cursors) => [
                            ...cursors,
                            targets.data!.next_ordinal!,
                          ]);
                      }}
                    >
                      下一页
                    </Button>
                  </div>
                </>
              )}
              <Button appearance="subtle" onClick={refresh}>
                刷新分发状态
              </Button>
            </>
          )}
        </>
      )}
    </section>
  );
}
