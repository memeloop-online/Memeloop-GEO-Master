import { useEffect, useState } from "react";
import {
  Badge,
  Button,
  Card,
  Field,
  MessageBar,
  MessageBarBody,
  Select,
  Spinner,
} from "@fluentui/react-components";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant, queryScopeFor } from "../auth/types";
import { ApiError } from "../api/client";
import {
  executeChannelTarget,
  getChannelPlan,
  getChannelTarget,
  submitChannelPlan,
  type ChannelPlan,
  type ChannelTarget,
  type ChannelTargetView,
  type PublicationRequest,
} from "../api/channelJobs";
import { useChannelData } from "../api/channels";
import { useSourceQuery, useSourcesQuery } from "../api/knowledge";
import { useProjectStartQuery } from "../api/projects";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import "./ChannelJobsPage.css";

const statusLabels: Record<string, string> = {
  planned: "待执行",
  in_flight: "执行中／待查回",
  published: "已发布（未公开验证）",
  verified: "公开读回已验证",
  unknown: "结果未知，禁止盲目重发",
  failed: "明确失败",
  login_required: "需要重新登录",
  unsupported: "暂不支持",
  observed: "已观察",
  refused: "拒答",
  missing: "缺测",
};

function resultStatus(view?: ChannelTargetView) {
  const last = view?.attempts.at(-1);
  return last?.outcome?.status ?? (last ? "in_flight" : "planned");
}

function errorText(error: unknown) {
  return error instanceof Error ? error.message : "操作失败，请重试。";
}

function dateTime(value?: string | null) {
  return value ? new Date(value).toLocaleString("zh-CN") : "未记录";
}

function TargetCard({
  target,
  view,
  loading,
  loadError,
  canWrite,
  onRefresh,
  onExecute,
  executing,
  executeError,
}: {
  target: ChannelTarget;
  view?: ChannelTargetView;
  loading: boolean;
  loadError: unknown;
  canWrite: boolean;
  onRefresh: () => void;
  onExecute: () => void;
  executing: boolean;
  executeError: unknown;
}) {
  const input = target.input;
  const status = resultStatus(view);
  const attempted = Boolean(view?.attempts.length);
  return (
    <Card className="channel-job-target">
      <div className="channel-job-target-heading">
        <div>
          <h3>{input.kind === "publish" ? input.title : input.question}</h3>
          <p>
            {input.kind === "publish" ? input.platform : input.provider}
            {" · "}账号 {input.account_id}
          </p>
        </div>
        <Badge appearance="outline">{statusLabels[status] ?? status}</Badge>
      </div>
      <p className="channel-job-ids">
        目标 {target.target_id}
        {input.kind === "publish" && (
          <>
            {" · "}来源 {input.source_id}
            {" · "}冻结版本 {input.source_version_id}
          </>
        )}
      </p>
      {loading && <Spinner label="正在读取执行记录" size="tiny" />}
      {loadError && (
        <ErrorState
          title="执行记录无法读取"
          detail={errorText(loadError)}
          onRetry={onRefresh}
        />
      )}
      {view?.attempts.map((attempt) => (
        <div className="channel-job-attempt" key={attempt.attempt_id}>
          <p>
            尝试 {attempt.attempt_id} · 领取 {dateTime(attempt.claimed_at)}
            {" · "}回收 {dateTime(attempt.received_at)}
          </p>
          {attempt.outcome ? (
            <>
              <p>
                结果：
                {statusLabels[attempt.outcome.status] ?? attempt.outcome.status}
                {" · "}
                {attempt.outcome.detail ?? "无详细说明"}
              </p>
              <p>
                结果时间 {dateTime(attempt.outcome.occurred_at)}
                {" · "}连接器 {attempt.outcome.connector_version ?? "未记录"}
                {attempt.outcome.fixture && " · 测试数据，非真实外部结果"}
              </p>
              {attempt.outcome.public_url && (
                <p>
                  <a
                    href={attempt.outcome.public_url}
                    target="_blank"
                    rel="noopener noreferrer"
                  >
                    已验证的公开链接
                  </a>
                </p>
              )}
              {(attempt.outcome.raw_answer ||
                attempt.outcome.citations.length > 0 ||
                attempt.outcome.runner_evidence.length > 0) && (
                <details>
                  <summary>原始结果与证据</summary>
                  <pre>
                    {JSON.stringify(
                      {
                        raw_answer: attempt.outcome.raw_answer,
                        citations: attempt.outcome.citations,
                        runner_evidence: attempt.outcome.runner_evidence,
                        screenshot_ref: attempt.outcome.screenshot_ref,
                      },
                      null,
                      2,
                    )}
                  </pre>
                </details>
              )}
            </>
          ) : (
            <p>尝试已领取但尚未收到结果；需要查回，不能重发。</p>
          )}
        </div>
      ))}
      {!attempted && !loadError && !executeError && !loading && canWrite && (
        <Button disabled={executing} onClick={onExecute}>
          {executing ? "正在执行…" : "执行此目标"}
        </Button>
      )}
      {attempted && status === "unknown" && (
        <p role="status">结果未知；当前没有安全的手动重发操作，请等待对账。</p>
      )}
      {executeError && (
        <ErrorState
          title="执行请求未确认"
          detail={
            <>
              {errorText(executeError)}
              。请求可能已被服务端领取；请先刷新记录核对， 不要盲目重发。
            </>
          }
          onRetry={onRefresh}
        />
      )}
      <Button appearance="subtle" onClick={onRefresh}>
        刷新记录
      </Button>
    </Card>
  );
}

function PlannedTarget({
  target,
  tenantId,
  projectId,
  canWrite,
}: {
  target: ChannelTarget;
  tenantId: string;
  projectId: string;
  canWrite: boolean;
}) {
  const { session } = useAuth();
  const client = useQueryClient();
  const key = [
    "channel-target",
    session?.user.id,
    session?.operator.id,
    tenantId,
    projectId,
    target.target_id,
  ];
  const detail = useQuery({
    queryKey: key,
    queryFn: () => getChannelTarget(tenantId, projectId, target.target_id),
    retry: false,
  });
  const execution = useMutation({
    mutationFn: () =>
      executeChannelTarget(tenantId, projectId, target.target_id),
    onSuccess: (result) => client.setQueryData(key, result),
    onError: () => void client.invalidateQueries({ queryKey: key }),
  });
  return (
    <TargetCard
      target={target}
      view={detail.data}
      loading={detail.isPending}
      loadError={detail.error}
      canWrite={canWrite}
      onRefresh={() => {
        void detail.refetch();
      }}
      onExecute={() => execution.mutate()}
      executing={execution.isPending}
      executeError={execution.error}
    />
  );
}

function FrozenPlan({
  plan,
  tenantId,
  projectId,
  canWrite,
}: {
  plan: ChannelPlan;
  tenantId: string;
  projectId: string;
  canWrite: boolean;
}) {
  return (
    <section className="channel-jobs-section">
      <h2>本轮冻结计划</h2>
      <p>
        版本 {plan.revision} · {dateTime(plan.created_at)} ·{" "}
        {plan.targets.length}
        个目标。提交后不能追加或改变目标。
      </p>
      {plan.targets.length === 0 ? (
        <EmptyState title="计划没有目标" detail="本轮计划已封存，无法追加。" />
      ) : (
        <div className="channel-jobs-list">
          {plan.targets.map((target) => (
            <PlannedTarget
              key={target.target_id}
              target={target}
              tenantId={tenantId}
              projectId={projectId}
              canWrite={canWrite}
            />
          ))}
        </div>
      )}
    </section>
  );
}

export function ChannelJobsPage() {
  const { tenantId, projectId } = useParams();
  const { session } = useAuth();
  const client = useQueryClient();
  const membership = membershipForTenant(session, tenantId);
  const canWrite = Boolean(
    membership && ["tenant_admin", "member"].includes(membership.role),
  );
  const start = useProjectStartQuery(tenantId, projectId);
  const cycleId = start.data?.cycle_id;
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  const planKey = [
    "channel-plan",
    scope?.userId,
    scope?.operatorId,
    tenantId,
    projectId,
    cycleId,
  ];
  const plan = useQuery({
    queryKey: planKey,
    queryFn: () => getChannelPlan(tenantId!, projectId!, cycleId!),
    enabled: Boolean(scope && cycleId),
    retry: false,
  });
  const sources = useSourcesQuery(tenantId, projectId);
  const channels = useChannelData(tenantId, projectId);
  const [sourceId, setSourceId] = useState("");
  const [versionId, setVersionId] = useState("");
  const [accountId, setAccountId] = useState("");
  const [draft, setDraft] = useState<PublicationRequest[]>([]);
  const sourceDetail = useSourceQuery(
    tenantId,
    projectId,
    sourceId || undefined,
  );
  const eligibleSources = (sources.data?.items ?? []).filter(
    (source) =>
      source.purpose === "public" &&
      source.state === "active" &&
      Boolean(source.current_version_id) &&
      (source.kind === "text" ||
        (source.kind === "file" && /\.(txt|md|markdown)$/i.test(source.name))),
  );
  const accounts = (channels.accounts.data?.items ?? []).filter(
    (account) =>
      account.enabled &&
      ["zhihu", "baidu_creator", "xiaohongshu"].includes(account.platform),
  );
  const selectedAccount = accounts.find(
    (account) => account.account_id === accountId,
  );
  useEffect(() => {
    setVersionId(sourceDetail.data?.source.current_version_id ?? "");
  }, [sourceDetail.data, sourceId]);
  useEffect(() => {
    setDraft([]);
    setSourceId("");
    setAccountId("");
  }, [cycleId, projectId]);
  const submission = useMutation({
    mutationFn: () =>
      submitChannelPlan(tenantId!, projectId!, cycleId!, {
        publications: draft,
        measurements: [],
      }),
    onSuccess: (saved) => client.setQueryData(planKey, saved),
    onError: () => void client.invalidateQueries({ queryKey: planKey }),
  });
  const addDraft = () => {
    if (!sourceId || !versionId || !selectedAccount) return;
    const item = {
      source_id: sourceId,
      source_version_id: versionId,
      account_id: selectedAccount.account_id,
      platform: selectedAccount.platform,
    };
    if (
      draft.some(
        (other) =>
          other.source_version_id === item.source_version_id &&
          other.account_id === item.account_id,
      )
    )
      return;
    setDraft((items) => [...items, item]);
  };

  if (!tenantId || !projectId) {
    return <ErrorState title="缺少项目" detail="请从项目中进入发布记录。" />;
  }
  return (
    <main className="channel-jobs-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">P12 · 发布记录</p>
          <h1>发布目标与执行记录</h1>
          <p>
            按本轮冻结的来源版本、账号和平台查看执行记录。账号可选或连接器支持某平台，
            都不代表已发布；只有公开读回验证才标为已验证。
          </p>
        </div>
        <Link to={`/app/${tenantId}/${projectId}/channels`}>管理渠道账号</Link>
      </section>
      {start.isPending ? (
        <LoadingState label="正在读取当前项目周期" />
      ) : start.isError ? (
        <ErrorState
          title="无法读取当前项目周期"
          detail={errorText(start.error)}
          onRetry={() => void start.refetch()}
        />
      ) : !cycleId ? (
        <EmptyState
          title="项目尚未启动"
          detail="启动项目后，当前周期才能创建发布计划。"
        />
      ) : (
        <>
          <p className="channel-job-ids">当前周期：{cycleId}</p>
          {plan.isPending ? (
            <LoadingState label="正在读取冻结计划" />
          ) : plan.isError ? (
            <ErrorState
              title="无法读取发布计划"
              detail={errorText(plan.error)}
              onRetry={() => void plan.refetch()}
            />
          ) : plan.data ? (
            <FrozenPlan
              plan={plan.data}
              tenantId={tenantId}
              projectId={projectId}
              canWrite={canWrite}
            />
          ) : (
            <section className="channel-jobs-section">
              <h2>建立本轮发布计划</h2>
              <MessageBar intent="warning">
                <MessageBarBody>
                  提交将一次性封存本轮所有发布目标，之后不能追加、替换来源版本或账号。
                  请先加入全部目标。当前独立测量列表为空，不会伪造测量样本。
                </MessageBarBody>
              </MessageBar>
              {!canWrite ? (
                <EmptyState
                  title="只读访问"
                  detail="当前角色无权创建或执行发布目标。"
                />
              ) : (
                <>
                  {(sources.isPending ||
                    channels.accounts.isPending ||
                    channels.platforms.isPending) && (
                    <LoadingState label="正在读取来源和账号" compact />
                  )}
                  {(sources.isError ||
                    channels.accounts.isError ||
                    channels.platforms.isError) && (
                    <ErrorState
                      title="无法读取来源或账号"
                      detail={errorText(
                        sources.error ||
                          channels.accounts.error ||
                          channels.platforms.error,
                      )}
                      onRetry={() => {
                        void sources.refetch();
                        void channels.accounts.refetch();
                        void channels.platforms.refetch();
                      }}
                    />
                  )}
                  {!sources.isPending &&
                    !sources.isError &&
                    !eligibleSources.length && (
                      <p role="status">
                        没有可选的公开 TXT/Markdown
                        来源版本。请先在知识库导入并设为公开。
                      </p>
                    )}
                  {!channels.accounts.isPending &&
                    !channels.accounts.isError &&
                    !accounts.length && (
                      <p role="status">
                        尚无分配给项目的可用发布账号。请先接入或分配账号。
                      </p>
                    )}
                  <div className="channel-jobs-form">
                    <Field label="公开 TXT/Markdown 来源">
                      <Select
                        value={sourceId}
                        onChange={(_, data) => setSourceId(data.value)}
                        disabled={!eligibleSources.length}
                      >
                        <option value="">选择来源</option>
                        {eligibleSources.map((source) => (
                          <option
                            key={source.source_id}
                            value={source.source_id}
                          >
                            {source.name}
                          </option>
                        ))}
                      </Select>
                    </Field>
                    <Field label="来源版本">
                      <Select
                        value={versionId}
                        onChange={(_, data) => setVersionId(data.value)}
                        disabled={
                          !sourceId ||
                          sourceDetail.isPending ||
                          sourceDetail.isError
                        }
                      >
                        <option value="">选择版本</option>
                        {sourceDetail.data?.versions.map((version) => (
                          <option
                            key={version.source_version_id}
                            value={version.source_version_id}
                          >
                            v{version.version} · {version.source_version_id}
                            {version.source_version_id ===
                              sourceDetail.data.source.current_version_id &&
                              "（当前）"}
                          </option>
                        ))}
                      </Select>
                    </Field>
                    <Field label="项目发布账号">
                      <Select
                        value={accountId}
                        onChange={(_, data) => setAccountId(data.value)}
                        disabled={!accounts.length}
                      >
                        <option value="">选择账号</option>
                        {accounts.map((account) => (
                          <option
                            key={account.account_id}
                            value={account.account_id}
                          >
                            {account.platform} ·{" "}
                            {account.display_name ?? account.account_id}
                            {" · "}
                            {account.status}
                          </option>
                        ))}
                      </Select>
                    </Field>
                    <Button
                      onClick={addDraft}
                      disabled={!sourceId || !versionId || !selectedAccount}
                    >
                      加入目标
                    </Button>
                  </div>
                  {sourceDetail.isError && (
                    <ErrorState
                      title="来源版本无法读取"
                      detail={errorText(sourceDetail.error)}
                      onRetry={() => void sourceDetail.refetch()}
                    />
                  )}
                  <h3>待封存目标（{draft.length}）</h3>
                  {draft.length ? (
                    <ul className="channel-jobs-draft">
                      {draft.map((item, index) => (
                        <li
                          key={`${item.source_version_id}-${item.account_id}`}
                        >
                          {eligibleSources.find(
                            (source) => source.source_id === item.source_id,
                          )?.name ?? item.source_id}
                          {" · "}
                          {item.source_version_id} → {item.platform} /{" "}
                          {item.account_id}
                          <Button
                            appearance="subtle"
                            onClick={() =>
                              setDraft((items) =>
                                items.filter((_, i) => i !== index),
                              )
                            }
                          >
                            移除
                          </Button>
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p>还没有目标。不会提交空计划。</p>
                  )}
                  <Button
                    appearance="primary"
                    disabled={!draft.length || submission.isPending}
                    onClick={() => submission.mutate()}
                  >
                    {submission.isPending ? "正在封存…" : "封存本轮计划"}
                  </Button>
                  {submission.isError && (
                    <ErrorState
                      title="计划未确认"
                      detail={
                        <>
                          {errorText(submission.error)}
                          {submission.error instanceof ApiError &&
                            submission.error.status === 409 &&
                            "。本轮可能已由其他操作封存，请刷新计划。"}
                        </>
                      }
                      onRetry={() => void plan.refetch()}
                    />
                  )}
                </>
              )}
            </section>
          )}
        </>
      )}
    </main>
  );
}
