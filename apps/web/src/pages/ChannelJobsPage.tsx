import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Badge,
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Spinner,
  Textarea,
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
  getCurrentCycle,
  submitChannelPlan,
  type ChannelPlan,
  type ChannelTarget,
  type ChannelTargetView,
  type MeasurementRequest,
  type BoundMeasurementRequest,
  type PublicationRequest,
} from "../api/channelJobs";
import {
  getQuestionSetVersion,
  listAllQuestionSets,
  listAllQuestionSetVersions,
} from "../api/questions";
import { useChannelData } from "../api/channels";
import { useSourceQuery, useSourcesQuery } from "../api/knowledge";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import { DistributionPanel } from "./DistributionPanel";
import { ObservationAnalysisPanel } from "./ObservationAnalysisPanel";
import "./measurementMessages";
import {
  PublicationLookupPanel,
  safeOriginalPublicUrl,
} from "./PublicationLookupPanel";
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

function validText(value: string, maxBytes: number) {
  return (
    Boolean(value.trim()) &&
    new TextEncoder().encode(value.trim()).length <= maxBytes
  );
}

export function TargetCard({
  target,
  tenantId,
  projectId,
  view,
  loading,
  loadError,
  canWrite,
  onRefresh,
  onExecute,
  executing,
  executeError,
  automatic = false,
  accountDisplayName,
}: {
  target: ChannelTarget;
  tenantId: string;
  projectId: string;
  view?: ChannelTargetView;
  loading: boolean;
  loadError: unknown;
  canWrite: boolean;
  onRefresh: () => void;
  onExecute: () => void;
  executing: boolean;
  executeError: unknown;
  automatic?: boolean;
  accountDisplayName?: string | null;
}) {
  const { t, i18n } = useTranslation("measurement");
  const [showTechnicalDetails, setShowTechnicalDetails] = useState(false);
  const input = target.input;
  const classification =
    input.kind === "measure"
      ? input.question_binding?.purpose === "optimization"
        ? "优化问题"
        : input.question_binding?.purpose === "frozen_evaluation"
          ? "冻结评估（不进入优化）"
          : "自定义问题（不进入优化）"
      : null;
  const status = resultStatus(view);
  const attempted = Boolean(view?.attempts.length);
  const automaticLabel = loading
    ? t("automaticLoading")
    : loadError
      ? t("automaticUnavailable")
      : !attempted
        ? t("automaticQueued")
        : status === "in_flight"
          ? t("automaticInFlight")
          : t(`automaticOutcome.${status}`, { defaultValue: status });
  if (automatic && input.kind === "measure") {
    const measurementDate = (value?: string | null) =>
      value
        ? new Date(value).toLocaleString(i18n.resolvedLanguage)
        : t("resultNotRecorded");
    return (
      <Card className="channel-job-target">
        <div className="channel-job-target-heading">
          <div>
            <h3>{input.question}</h3>
            <p>
              {input.provider}
              {accountDisplayName?.trim() &&
                ` · ${t("resultAccount", { value: accountDisplayName })}`}
            </p>
          </div>
          <Badge appearance="outline">
            {view?.attempts.at(-1)?.outcome?.fixture && status === "observed"
              ? t("resultFixture")
              : automaticLabel}
          </Badge>
        </div>
        <p>
          {t("resultScheduled", { value: measurementDate(input.scheduled_at) })}
        </p>
        {loading && <Spinner label={t("automaticLoading")} size="tiny" />}
        {loadError && (
          <ErrorState
            title={t("automaticReadError")}
            detail={errorText(loadError)}
            onRetry={onRefresh}
          />
        )}
        {!loading && !loadError && !attempted && (
          <p role="status">{t("automaticQueuedDetail")}</p>
        )}
        {view?.attempts.map((attempt) => {
          const outcome = attempt.outcome;
          const observed = outcome?.status === "observed" && !outcome.fixture;
          const citations = [
            ...new Set(
              (outcome?.citations ?? [])
                .map(safeOriginalPublicUrl)
                .filter((url): url is string => url !== null),
            ),
          ];
          return (
            <div className="channel-job-attempt" key={attempt.attempt_id}>
              <p>
                {outcome ? (
                  outcome.fixture && outcome.status === "observed" ? (
                    t("resultFixture")
                  ) : (
                    t(`automaticOutcome.${outcome.status}`, {
                      defaultValue: outcome.status,
                    })
                  )
                ) : (
                  <span>{t("automaticInFlightDetail")}</span>
                )}
                {" · "}
                {t("resultTime", {
                  value: measurementDate(
                    outcome?.occurred_at ?? attempt.claimed_at,
                  ),
                })}
              </p>
              {observed && (
                <>
                  <h4>{t("resultAnswer")}</h4>
                  <div
                    style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}
                  >
                    {outcome.raw_answer?.trim()
                      ? outcome.raw_answer
                      : t("resultAnswerMissing")}
                  </div>
                  <h4>{t("resultCitations")}</h4>
                  {citations.length ? (
                    <ul>
                      {citations.map((url) => (
                        <li key={url} style={{ overflowWrap: "anywhere" }}>
                          <a
                            href={url}
                            target="_blank"
                            rel="noopener noreferrer"
                          >
                            {url}
                          </a>
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p>
                      {t(
                        outcome.citations.length
                          ? "resultNoSafeCitations"
                          : "resultNoCitations",
                      )}
                    </p>
                  )}
                </>
              )}
              {outcome && (
                <ObservationAnalysisPanel
                  tenantId={tenantId}
                  projectId={projectId}
                  targetId={target.target_id}
                  attemptId={attempt.attempt_id}
                  canWrite={canWrite}
                />
              )}
            </div>
          );
        })}
        {attempted && status === "unknown" && (
          <p role="status">{t("automaticUnknownDetail")}</p>
        )}
        <details
          onToggle={(event) =>
            setShowTechnicalDetails(event.currentTarget.open)
          }
        >
          <summary>{t("resultTechnicalDetails")}</summary>
          {showTechnicalDetails && (
            <pre style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>
              {JSON.stringify(
                { target, attempts: view?.attempts ?? [] },
                null,
                2,
              )}
            </pre>
          )}
        </details>
        <Button appearance="subtle" onClick={onRefresh}>
          {t("resultRefresh")}
        </Button>
      </Card>
    );
  }
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
        <Badge appearance="outline">
          {automatic ? automaticLabel : (statusLabels[status] ?? status)}
        </Badge>
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
      {input.kind === "measure" && (
        <p>
          独立测量 · {input.surface} / {input.search_mode} · 模型 {input.model}
          {" · "}协议 {input.protocol_version} · 问题集{" "}
          {input.question_set_version}
          {" · "}
          {input.market} / {input.language} · 样本 {input.sample_ordinal}
          {" · "}排期 {dateTime(input.scheduled_at)}
          {" · "}用途 {classification}
        </p>
      )}
      {loading && <Spinner label="正在读取执行记录" size="tiny" />}
      {loadError && (
        <ErrorState
          title={automatic ? t("automaticReadError") : "执行记录无法读取"}
          detail={errorText(loadError)}
          onRetry={onRefresh}
        />
      )}
      {automatic && !loading && !loadError && !attempted && (
        <p role="status">{t("automaticQueuedDetail")}</p>
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
                {automatic
                  ? t(`automaticOutcome.${attempt.outcome.status}`, {
                      defaultValue: attempt.outcome.status,
                    })
                  : (statusLabels[attempt.outcome.status] ??
                    attempt.outcome.status)}
                {" · "}
                {attempt.outcome.detail ?? "无详细说明"}
              </p>
              <p>
                结果时间 {dateTime(attempt.outcome.occurred_at)}
                {" · "}连接器 {attempt.outcome.connector_version ?? "未记录"}
                {attempt.outcome.fixture &&
                  (automatic
                    ? ` · ${t("automaticUnverifiedResult")}`
                    : " · 测试数据，非真实外部结果")}
              </p>
              {safeOriginalPublicUrl(attempt.outcome.public_url) && (
                <p>
                  <a
                    href={safeOriginalPublicUrl(attempt.outcome.public_url)!}
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
            <p>
              {automatic
                ? t("automaticInFlightDetail")
                : "尝试已领取但尚未收到结果；需要查回，不能重发。"}
            </p>
          )}
          {input.kind === "measure" && attempt.outcome && (
            <ObservationAnalysisPanel
              tenantId={tenantId}
              projectId={projectId}
              targetId={target.target_id}
              attemptId={attempt.attempt_id}
              canWrite={canWrite}
            />
          )}
        </div>
      ))}
      {!automatic &&
        !attempted &&
        !loadError &&
        !executeError &&
        !loading &&
        canWrite && (
          <Button disabled={executing} onClick={onExecute}>
            {executing ? "正在执行…" : "执行此目标"}
          </Button>
        )}
      {attempted && status === "unknown" && (
        <p role="status">
          {automatic
            ? t("automaticUnknownDetail")
            : "结果未知；当前没有安全的手动重发操作，请等待对账。"}
        </p>
      )}
      {input.kind === "publish" &&
        view?.attempts.some(
          (attempt) => !attempt.outcome || attempt.outcome.status === "unknown",
        ) && (
          <PublicationLookupPanel
            tenantId={tenantId}
            projectId={projectId}
            targetId={target.target_id}
          />
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

export function PlannedTarget({
  target,
  tenantId,
  projectId,
  canWrite,
  automatic = false,
  accountDisplayName,
}: {
  target: ChannelTarget;
  tenantId: string;
  projectId: string;
  canWrite: boolean;
  automatic?: boolean;
  accountDisplayName?: string | null;
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
    refetchInterval: (query) =>
      query.state.data?.attempts.at(-1)?.outcome ? false : 3000,
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
      tenantId={tenantId}
      projectId={projectId}
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
      automatic={automatic}
      accountDisplayName={accountDisplayName}
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
      <h2>来源版本发布计划</h2>
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
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  const currentCycle = useQuery({
    queryKey: [
      "current-channel-cycle",
      scope?.userId,
      scope?.operatorId,
      tenantId,
      projectId,
    ],
    queryFn: () => getCurrentCycle(tenantId!, projectId!),
    enabled: Boolean(scope),
    retry: false,
  });
  const cycleId = currentCycle.data?.cycle_id;
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
  const [measurementDraft, setMeasurementDraft] = useState<
    MeasurementRequest[]
  >([]);
  const [boundDraft, setBoundDraft] = useState<BoundMeasurementRequest[]>([]);
  const [measurementMode, setMeasurementMode] = useState<"bound" | "legacy">(
    "bound",
  );
  const [questionSetId, setQuestionSetId] = useState("");
  const [boundVersionId, setBoundVersionId] = useState("");
  const [boundQuestionId, setBoundQuestionId] = useState("");
  const questionSets = useQuery({
    queryKey: [
      "question-sets",
      scope?.userId,
      scope?.operatorId,
      tenantId,
      projectId,
    ],
    queryFn: () => listAllQuestionSets(tenantId!, projectId!),
    enabled: Boolean(scope && cycleId && !plan.data),
    retry: false,
  });
  const questionVersions = useQuery({
    queryKey: [
      "question-versions",
      scope?.userId,
      scope?.operatorId,
      tenantId,
      projectId,
      questionSetId,
    ],
    queryFn: () =>
      listAllQuestionSetVersions(tenantId!, projectId!, questionSetId),
    enabled: Boolean(scope && questionSetId && !plan.data),
    retry: false,
  });
  const boundVersion = useQuery({
    queryKey: [
      "question-version",
      scope?.userId,
      scope?.operatorId,
      tenantId,
      projectId,
      questionSetId,
      boundVersionId,
    ],
    queryFn: () =>
      getQuestionSetVersion(
        tenantId!,
        projectId!,
        questionSetId,
        boundVersionId,
      ),
    enabled: Boolean(scope && questionSetId && boundVersionId && !plan.data),
    retry: false,
  });
  const [measurementAccountId, setMeasurementAccountId] = useState("");
  const [model, setModel] = useState("");
  const [protocolVersion, setProtocolVersion] = useState("");
  const [questionSetVersion, setQuestionSetVersion] = useState("");
  const [question, setQuestion] = useState("");
  const [market, setMarket] = useState("");
  const [language, setLanguage] = useState("");
  const [scheduledAt, setScheduledAt] = useState("");
  const [sampleOrdinal, setSampleOrdinal] = useState("0");
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
  // The platform registry describes login support, not verified official search.
  const kimiPlatform = channels.platforms.data?.items.find(
    (platform) =>
      platform.id === "kimi" &&
      platform.purpose === "measurement" &&
      platform.login_supported,
  );
  const measurementAccounts = kimiPlatform
    ? (channels.accounts.data?.items ?? []).filter(
        (account) =>
          account.platform === "kimi" &&
          account.enabled &&
          account.status === "ready",
      )
    : [];
  const selectedMeasurementAccount = measurementAccounts.find(
    (account) => account.account_id === measurementAccountId,
  );
  const scheduledDate =
    scheduledAt && /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(scheduledAt)
      ? new Date(scheduledAt)
      : null;
  const validMeasurement =
    Boolean(selectedMeasurementAccount) &&
    validText(model, 100) &&
    validText(protocolVersion, 100) &&
    (measurementMode === "bound" ||
      (validText(questionSetVersion, 100) &&
        validText(question, 4000) &&
        validText(market, 100) &&
        validText(language, 100))) &&
    Boolean(scheduledDate && !Number.isNaN(scheduledDate.getTime())) &&
    /^(0|[1-9]\d*)$/.test(sampleOrdinal) &&
    Number(sampleOrdinal) <= 10000;
  const selectedBoundQuestion = boundVersion.data?.questions.find(
    (entry) => entry.question_id === boundQuestionId,
  );
  useEffect(() => {
    setVersionId(sourceDetail.data?.source.current_version_id ?? "");
  }, [sourceDetail.data, sourceId]);
  useEffect(() => {
    setDraft([]);
    setMeasurementDraft([]);
    setBoundDraft([]);
    setSourceId("");
    setAccountId("");
    setMeasurementAccountId("");
  }, [cycleId, projectId]);
  const submission = useMutation({
    mutationFn: () =>
      submitChannelPlan(tenantId!, projectId!, cycleId!, {
        publications: draft,
        measurements: measurementDraft,
        bound_measurements: boundDraft,
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
  const addMeasurement = () => {
    if (!validMeasurement || !selectedMeasurementAccount || !scheduledDate)
      return;
    if (measurementMode === "bound") {
      if (!questionSetId || !boundVersionId || !selectedBoundQuestion) return;
      const item: BoundMeasurementRequest = {
        account_id: selectedMeasurementAccount.account_id,
        provider: "kimi",
        model: model.trim(),
        surface: "consumer_web",
        search_mode: "web_search",
        protocol_version: protocolVersion.trim(),
        question: {
          question_set_id: questionSetId,
          question_set_version_id: boundVersionId,
          question_id: selectedBoundQuestion.question_id,
          question_revision_id: selectedBoundQuestion.id,
        },
        scheduled_at: scheduledDate.toISOString(),
        sample_ordinal: Number(sampleOrdinal),
      };
      if (
        !boundDraft.some(
          (other) => JSON.stringify(other) === JSON.stringify(item),
        )
      )
        setBoundDraft((items) => [...items, item]);
      return;
    }
    const item: MeasurementRequest = {
      account_id: selectedMeasurementAccount.account_id,
      provider: "kimi",
      model: model.trim(),
      surface: "consumer_web",
      search_mode: "web_search",
      protocol_version: protocolVersion.trim(),
      question_set_version: questionSetVersion.trim(),
      question: question.trim(),
      market: market.trim(),
      language: language.trim(),
      scheduled_at: scheduledDate.toISOString(),
      sample_ordinal: Number(sampleOrdinal),
    };
    if (
      measurementDraft.some(
        (other) => JSON.stringify(other) === JSON.stringify(item),
      )
    )
      return;
    setMeasurementDraft((items) => [...items, item]);
  };

  if (!tenantId || !projectId) {
    return <ErrorState title="缺少项目" detail="请从项目中进入发布记录。" />;
  }
  return (
    <main className="channel-jobs-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">发布执行</p>
          <h1>发布目标与执行记录</h1>
          <p>
            按本轮冻结的来源版本、账号和平台查看执行记录。账号可选或连接器支持某平台，
            都不代表已发布；只有公开读回验证才标为已验证。
          </p>
        </div>
        <div>
          <Link to={`/app/${tenantId}/${projectId}/channels`}>
            管理渠道账号
          </Link>
          {" · "}
          <Link to={`/app/${tenantId}/${projectId}/measurement`}>
            管理问题集
          </Link>
        </div>
      </section>
      {currentCycle.isPending ? (
        <LoadingState label="正在读取当前项目周期" />
      ) : currentCycle.isError ? (
        <ErrorState
          title="无法读取当前项目周期"
          detail={errorText(currentCycle.error)}
          onRetry={() => void currentCycle.refetch()}
        />
      ) : !cycleId ? (
        <EmptyState
          title="尚无周期发布计划"
          detail="上方可直接进行独立测量；内容发布计划会在启动自动运营后显示。"
        />
      ) : (
        <>
          <p className="channel-job-ids">当前周期：{cycleId}</p>
          <DistributionPanel
            tenantId={tenantId}
            projectId={projectId}
            cycleId={cycleId}
            canWrite={canWrite}
          />
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
              <h2>建立来源版本发布计划</h2>
              <MessageBar intent="warning">
                <MessageBarBody>
                  提交将一次性封存本轮所有发布目标，之后不能追加、替换来源版本或账号。
                  请先加入全部目标。独立测量可单独封存，无需发布来源；冻结计划不等于采样成功。
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
                  <Field label="测量问题模式">
                    <Select
                      value={measurementMode}
                      onChange={(_, data) =>
                        setMeasurementMode(data.value as "bound" | "legacy")
                      }
                    >
                      <option value="bound">
                        绑定已保存的问题集版本（推荐）
                      </option>
                      <option value="legacy">
                        临时自由文本 · 未分类，不进入优化
                      </option>
                    </Select>
                  </Field>
                  {measurementMode === "bound" && (
                    <>
                      {questionSets.isPending && (
                        <LoadingState label="正在读取问题集" compact />
                      )}
                      {questionSets.isError && (
                        <ErrorState
                          title="问题集无法读取"
                          detail={errorText(questionSets.error)}
                          onRetry={() => void questionSets.refetch()}
                        />
                      )}
                      {!questionSets.isPending &&
                        !questionSets.isError &&
                        !questionSets.data?.items.length && (
                          <p role="status">
                            没有可绑定的问题集。请先在{" "}
                            <Link
                              to={`/app/${tenantId}/${projectId}/measurement`}
                            >
                              问题集
                            </Link>
                            创建版本。
                          </p>
                        )}
                      <div className="channel-jobs-form">
                        <Field label="绑定问题集">
                          <Select
                            value={questionSetId}
                            onChange={(_, data) => {
                              setQuestionSetId(data.value);
                              setBoundVersionId("");
                              setBoundQuestionId("");
                            }}
                          >
                            <option value="">选择问题集</option>
                            {questionSets.data?.items.map((set) => (
                              <option value={set.id} key={set.id}>
                                {set.name} · 优化 {set.optimization_count} /
                                冻结评估 {set.evaluation_count}
                              </option>
                            ))}
                          </Select>
                        </Field>
                        <Field label="绑定不可变版本">
                          <Select
                            value={boundVersionId}
                            disabled={
                              !questionSetId ||
                              questionVersions.isPending ||
                              questionVersions.isError
                            }
                            onChange={(_, data) => {
                              setBoundVersionId(data.value);
                              setBoundQuestionId("");
                            }}
                          >
                            <option value="">选择版本</option>
                            {questionVersions.data?.items.map((item) => (
                              <option key={item.id} value={item.id}>
                                v{item.revision} · 优化{" "}
                                {item.optimization_count} / 冻结评估{" "}
                                {item.evaluation_count}
                              </option>
                            ))}
                          </Select>
                        </Field>
                        <Field label="绑定问题">
                          <Select
                            value={boundQuestionId}
                            disabled={
                              !boundVersionId ||
                              boundVersion.isPending ||
                              boundVersion.isError
                            }
                            onChange={(_, data) =>
                              setBoundQuestionId(data.value)
                            }
                          >
                            <option value="">选择问题</option>
                            {boundVersion.data?.questions.map((item) => (
                              <option value={item.question_id} key={item.id}>
                                {item.text} ·{" "}
                                {item.purpose === "optimization"
                                  ? "优化"
                                  : "冻结评估"}
                              </option>
                            ))}
                          </Select>
                        </Field>
                      </div>
                      {questionVersions.isError && (
                        <ErrorState
                          title="问题版本无法读取"
                          detail={errorText(questionVersions.error)}
                          onRetry={() => void questionVersions.refetch()}
                        />
                      )}
                      {boundVersion.isError && (
                        <ErrorState
                          title="绑定问题无法读取"
                          detail={errorText(boundVersion.error)}
                          onRetry={() => void boundVersion.refetch()}
                        />
                      )}
                      {selectedBoundQuestion && (
                        <p role="status">
                          已选用途：
                          {selectedBoundQuestion.purpose === "optimization"
                            ? "优化问题"
                            : "冻结评估，不进入优化"}
                          。问题文本、市场及语言以服务端不可变修订为准。
                        </p>
                      )}
                    </>
                  )}
                  {measurementMode === "legacy" && (
                    <p role="status">
                      临时自由文本永久未分类，不进入优化；即使填写与真实版本相同的标签也不会成为绑定问题。
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
                    <p>还没有发布目标；可以仅封存独立测量。</p>
                  )}
                  <h3>独立 AI 测量（消费端网页）</h3>
                  <MessageBar intent="warning">
                    <MessageBarBody>
                      Kimi
                      网页账号登录只表示可尝试采样。官方联网搜索适配器尚未实测验证，
                      无法确认实际搜索、完整答案或所选模型时会记录不支持或缺测；
                      不能将登录或普通模型回答视为官方搜索成功。
                      模型标识和协议版本须按实际观测填写，系统尚不提供已验证的模型能力列表。
                    </MessageBarBody>
                  </MessageBar>
                  {!channels.accounts.isPending &&
                    !channels.platforms.isPending &&
                    !channels.accounts.isError &&
                    !channels.platforms.isError &&
                    !measurementAccounts.length && (
                      <p role="status">
                        没有已连接且就绪的项目 Kimi
                        测量账号；发布账号不能用于测量。
                      </p>
                    )}
                  <div className="channel-jobs-form">
                    <Field label="项目 Kimi 测量账号">
                      <Select
                        value={measurementAccountId}
                        onChange={(_, data) =>
                          setMeasurementAccountId(data.value)
                        }
                        disabled={!measurementAccounts.length}
                      >
                        <option value="">选择测量账号</option>
                        {measurementAccounts.map((account) => (
                          <option
                            key={account.account_id}
                            value={account.account_id}
                          >
                            {account.display_name ?? account.account_id}
                            {" · "}
                            {account.owner_kind === "operator_pool"
                              ? "已分配资源"
                              : "项目自有"}
                          </option>
                        ))}
                      </Select>
                    </Field>
                    <Field
                      label="可见模型标识"
                      hint="填写账号当前可选模型的标识，例如 k2d6-chat；不会自动切换为其他模型。"
                    >
                      <Input
                        value={model}
                        onChange={(_, data) => setModel(data.value)}
                        maxLength={100}
                      />
                    </Field>
                    <Field label="采样协议版本">
                      <Input
                        value={protocolVersion}
                        onChange={(_, data) => setProtocolVersion(data.value)}
                        maxLength={100}
                      />
                    </Field>
                    {measurementMode === "legacy" && (
                      <>
                        <Field label="临时问题集标签">
                          <Input
                            value={questionSetVersion}
                            onChange={(_, data) =>
                              setQuestionSetVersion(data.value)
                            }
                            maxLength={100}
                          />
                        </Field>
                        <Field label="市场">
                          <Input
                            value={market}
                            onChange={(_, data) => setMarket(data.value)}
                            maxLength={100}
                          />
                        </Field>
                        <Field label="语言">
                          <Input
                            value={language}
                            onChange={(_, data) => setLanguage(data.value)}
                            maxLength={100}
                          />
                        </Field>
                      </>
                    )}
                    <Field label="计划采样时间（本地时间）">
                      <Input
                        type="datetime-local"
                        value={scheduledAt}
                        onChange={(_, data) => setScheduledAt(data.value)}
                      />
                    </Field>
                    <Field label="样本序号（0–10000）">
                      <Input
                        type="number"
                        min={0}
                        max={10000}
                        step={1}
                        value={sampleOrdinal}
                        onChange={(_, data) => setSampleOrdinal(data.value)}
                      />
                    </Field>
                    {measurementMode === "legacy" && (
                      <Field label="临时问题（未分类）">
                        <Textarea
                          value={question}
                          onChange={(_, data) => setQuestion(data.value)}
                          maxLength={4000}
                        />
                      </Field>
                    )}
                    <Button
                      onClick={addMeasurement}
                      disabled={
                        !validMeasurement ||
                        (measurementMode === "bound" &&
                          !selectedBoundQuestion) ||
                        draft.length +
                          measurementDraft.length +
                          boundDraft.length >=
                          100
                      }
                    >
                      加入测量目标
                    </Button>
                  </div>
                  <p>
                    固定协议：Kimi · consumer_web ·
                    web_search。绑定问题的用途由服务端确定；
                    冻结评估题及逐题答案不进入优化，临时未分类问题也不进入优化。
                  </p>
                  <h3>
                    待封存测量（{measurementDraft.length + boundDraft.length}）
                  </h3>
                  {measurementDraft.length || boundDraft.length ? (
                    <ul className="channel-jobs-draft">
                      {measurementDraft.map((item, index) => (
                        <li
                          key={`${item.account_id}-${item.question_set_version}-${item.sample_ordinal}-${index}`}
                        >
                          {item.question} · {item.model} /{" "}
                          {item.protocol_version} · 未分类，不进入优化
                          {" · "}
                          {item.market} / {item.language}
                          {" · "}样本 {item.sample_ordinal} ·{" "}
                          {dateTime(item.scheduled_at)}
                          <Button
                            appearance="subtle"
                            onClick={() =>
                              setMeasurementDraft((items) =>
                                items.filter((_, i) => i !== index),
                              )
                            }
                          >
                            移除测量
                          </Button>
                        </li>
                      ))}
                      {boundDraft.map((item, index) => (
                        <li
                          key={`${item.question.question_revision_id}-${item.sample_ordinal}-${index}`}
                        >
                          {item.question.question_set_version_id} /{" "}
                          {item.question.question_id}
                          {" · "}绑定问题，服务端决定用途 · {item.model} /{" "}
                          {item.protocol_version}
                          {" · "}样本 {item.sample_ordinal} ·{" "}
                          {dateTime(item.scheduled_at)}
                          <Button
                            appearance="subtle"
                            onClick={() =>
                              setBoundDraft((items) =>
                                items.filter((_, i) => i !== index),
                              )
                            }
                          >
                            移除测量
                          </Button>
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p>还没有测量目标。</p>
                  )}
                  <Button
                    appearance="primary"
                    disabled={
                      (!draft.length &&
                        !measurementDraft.length &&
                        !boundDraft.length) ||
                      draft.length +
                        measurementDraft.length +
                        boundDraft.length >
                        100 ||
                      submission.isPending
                    }
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
