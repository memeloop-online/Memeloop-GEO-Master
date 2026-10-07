import { useEffect, useRef, useState } from "react";
import { Button, Field, Select, Textarea } from "@fluentui/react-components";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useSearchParams } from "react-router-dom";
import { useAuth } from "../auth/AuthProvider";
import { useChannelData } from "../api/channels";
import {
  createMeasurementPlan,
  getMeasurementPlan,
  getMeasurementOptions,
  listMeasurementPlans,
  type StandaloneMeasurementRequest,
} from "../api/channelJobs";
import {
  getQuestionSetVersion,
  listAllQuestionSets,
  listAllQuestionSetVersions,
} from "../api/questions";
import { ErrorState, LoadingState } from "../components/AsyncState";
import { PlannedTarget } from "./ChannelJobsPage";

const errorText = (error: unknown) =>
  error instanceof Error ? error.message : "请稍后重试。";

export function StandaloneMeasurementPanel({
  tenantId,
  projectId,
  canWrite,
  recordsOnly = false,
}: {
  tenantId: string;
  projectId: string;
  canWrite: boolean;
  recordsOnly?: boolean;
}) {
  const [params, setParams] = useSearchParams();
  const selectedId = params.get("record") ?? params.get("planId") ?? undefined;
  const selectRecord = (id: string) =>
    setParams((previous) => {
      const next = new URLSearchParams(previous);
      next.set("tab", "records");
      next.delete("planId");
      next.set("record", id);
      return next;
    });
  const { session } = useAuth();
  const client = useQueryClient();
  const channels = useChannelData(tenantId, projectId);
  const [question, setQuestion] = useState("");
  const [accountId, setAccountId] = useState("");
  const [model, setModel] = useState("");
  const [mode, setMode] = useState<"custom" | "bound">("custom");
  const [setId, setSetId] = useState("");
  const [versionId, setVersionId] = useState("");
  const [questionId, setQuestionId] = useState("");
  const [after, setAfter] = useState<string>();
  const [previous, setPrevious] = useState<(string | undefined)[]>([]);
  // Preserve the entire accepted-intent candidate across transport failures,
  // including its timestamp, so retrying cannot create a second measurement.
  const pendingRequest = useRef<{
    fingerprint: string;
    input: StandaloneMeasurementRequest;
  } | null>(null);
  const key = [
    "standalone-measurements",
    session?.user.id,
    session?.operator.id,
    tenantId,
    projectId,
  ];
  const history = useQuery({
    queryKey: [...key, "list", after],
    queryFn: () => listMeasurementPlans(tenantId, projectId, after),
    enabled: Boolean(session && recordsOnly),
    retry: false,
  });
  const detail = useQuery({
    queryKey: [...key, "detail", selectedId],
    queryFn: () => getMeasurementPlan(tenantId, projectId, selectedId!),
    enabled: Boolean(session && recordsOnly && selectedId),
    retry: false,
  });
  const accounts = (channels.accounts.data?.items ?? []).filter(
    (account) => account.platform === "kimi",
  );
  const selectedAccount = accounts.find(
    (account) =>
      account.account_id ===
      (accountId ||
        accounts.find((item) => item.enabled && item.status === "ready")
          ?.account_id),
  );
  const accountReady = Boolean(
    selectedAccount?.enabled && selectedAccount.status === "ready",
  );
  const options = useQuery({
    queryKey: [...key, "options", selectedAccount?.account_id],
    queryFn: () =>
      getMeasurementOptions(tenantId, projectId, selectedAccount!.account_id),
    enabled: Boolean(session && accountReady && canWrite && !recordsOnly),
    retry: false,
  });
  const models = options.data?.models ?? [];
  const selectedModel =
    models.find((item) => item.id === model)?.id ??
    models.find((item) => item.id === options.data?.selected_model)?.id ??
    models[0]?.id;
  const sets = useQuery({
    queryKey: [
      "question-sets",
      session?.user.id,
      session?.operator.id,
      tenantId,
      projectId,
    ],
    queryFn: () => listAllQuestionSets(tenantId, projectId),
    enabled: Boolean(session && canWrite && !recordsOnly),
    retry: false,
  });
  const selectedSet =
    sets.data?.items.find((item) => item.id === setId) ?? sets.data?.items[0];
  const versions = useQuery({
    queryKey: [...key, "versions", selectedSet?.id],
    queryFn: () =>
      listAllQuestionSetVersions(tenantId, projectId, selectedSet!.id),
    enabled: Boolean(
      session && !recordsOnly && mode === "bound" && selectedSet,
    ),
    retry: false,
  });
  const activeVersionId = versionId || selectedSet?.current_version_id;
  const version = useQuery({
    queryKey: [...key, "question-version", selectedSet?.id, activeVersionId],
    queryFn: () =>
      getQuestionSetVersion(
        tenantId,
        projectId,
        selectedSet!.id,
        activeVersionId!,
      ),
    enabled: Boolean(
      session &&
      !recordsOnly &&
      mode === "bound" &&
      selectedSet &&
      activeVersionId,
    ),
    retry: false,
  });
  const selectedQuestion =
    version.data?.questions.find((item) => item.question_id === questionId) ??
    version.data?.questions[0];
  const refreshAccounts = channels.accounts.refetch;
  useEffect(() => {
    if (recordsOnly) return;
    const refresh = () => {
      if (document.visibilityState !== "hidden") void refreshAccounts();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [recordsOnly, refreshAccounts]);
  const submission = useMutation({
    mutationFn: (input: StandaloneMeasurementRequest) =>
      createMeasurementPlan(tenantId, projectId, input),
    onSuccess: (plan) => {
      pendingRequest.current = null;
      selectRecord(plan.plan_id);
      client.setQueryData([...key, "detail", plan.plan_id], plan);
      setAfter(undefined);
      setPrevious([]);
      void client.invalidateQueries({ queryKey: [...key, "list"] });
    },
  });
  function start() {
    if (!selectedAccount || !selectedModel) return;
    const common = {
      account_id: selectedAccount.account_id,
      provider: "kimi",
      model: selectedModel,
      surface: "consumer_web",
      search_mode: "web_search",
      protocol_version: "v1",
      sample_ordinal: 0,
    };
    const values =
      mode === "bound" && selectedSet && selectedQuestion && version.data
        ? {
            measurements: [],
            bound_measurements: [
              {
                ...common,
                question: {
                  question_set_id: selectedSet.id,
                  question_set_version_id: version.data.id,
                  question_id: selectedQuestion.question_id,
                  question_revision_id: selectedQuestion.id,
                },
              },
            ],
          }
        : {
            measurements: [
              {
                ...common,
                question_set_version: "ad_hoc.v1",
                question: question.trim(),
                market: "CN",
                language: "zh-CN",
              },
            ],
          };
    const fingerprint = JSON.stringify(values);
    if (pendingRequest.current?.fingerprint !== fingerprint) {
      pendingRequest.current = {
        fingerprint,
        input: {
          idempotency_key: crypto.randomUUID(),
          title: mode === "bound" ? "问题集测量" : "自定义问题测量",
          measurements: values.measurements.map((item) => ({
            ...item,
            scheduled_at: new Date().toISOString(),
          })),
          ...(values.bound_measurements
            ? {
                bound_measurements: values.bound_measurements.map((item) => ({
                  ...item,
                  scheduled_at: new Date().toISOString(),
                })),
              }
            : {}),
        },
      };
    }
    submission.mutate(pendingRequest.current.input);
  }
  const valid =
    accountReady &&
    Boolean(selectedModel) &&
    (mode === "bound"
      ? Boolean(selectedQuestion && version.data)
      : Boolean(question.trim()) &&
        new TextEncoder().encode(question.trim()).length <= 4000);
  return (
    <section className="channel-jobs-section" aria-label="独立问题测量">
      {!recordsOnly && (
        <>
          <h2>开始测量</h2>
          <p>输入问题，选择账号，查看联网搜索答案。</p>
          <div
            style={{
              display: "flex",
              alignItems: "center",
              flexWrap: "wrap",
              gap: 8,
            }}
          >
            <Link to={`/app/${tenantId}/${projectId}/channels/connect`}>
              登录或连接 Kimi 账号
            </Link>
            <Button
              appearance="subtle"
              onClick={() => void channels.accounts.refetch()}
            >
              刷新账号
            </Button>
          </div>
          {channels.accounts.isPending && (
            <LoadingState label="正在读取测量账号" />
          )}
          {channels.accounts.isError && (
            <ErrorState
              title="测量账号无法读取"
              detail={errorText(channels.accounts.error)}
              onRetry={() => void channels.accounts.refetch()}
            />
          )}
          {!channels.accounts.isPending &&
            !channels.accounts.isError &&
            !accounts.length && (
              <p role="status">请先连接并登录一个 Kimi 账号。</p>
            )}
          {!canWrite && <p role="status">当前权限只能查看测量历史。</p>}
          {canWrite && (
            <div>
              <Field label="问题来源">
                <Select
                  value={mode}
                  onChange={(_, data) =>
                    setMode(data.value as "custom" | "bound")
                  }
                  disabled={submission.isPending}
                >
                  <option value="custom">自定义问题</option>
                  <option value="bound">
                    已有问题集{sets.data ? `（${sets.data.items.length}）` : ""}
                  </option>
                </Select>
              </Field>
              {mode === "custom" ? (
                <Field label="要测量的问题">
                  <Textarea
                    value={question}
                    onChange={(_, data) => setQuestion(data.value)}
                    maxLength={4000}
                    disabled={submission.isPending}
                  />
                </Field>
              ) : (
                <>
                  {sets.isPending && <LoadingState label="正在读取问题集" />}
                  {sets.isError && (
                    <ErrorState
                      title="问题集无法读取"
                      detail={errorText(sets.error)}
                      onRetry={() => void sets.refetch()}
                    />
                  )}
                  {sets.data?.items.length === 0 && (
                    <p>
                      还没有问题集。
                      <Link
                        to={`/app/${tenantId}/${projectId}/measurement?tab=sets`}
                      >
                        创建问题集
                      </Link>
                    </p>
                  )}
                  {selectedSet && (
                    <Field label="选择问题集">
                      <Select
                        value={selectedSet.id}
                        onChange={(_, data) => {
                          setSetId(data.value);
                          setVersionId("");
                          setQuestionId("");
                        }}
                        disabled={submission.isPending}
                      >
                        {sets.data?.items.map((item) => (
                          <option key={item.id} value={item.id}>
                            {item.name}（{item.question_count} 题）
                          </option>
                        ))}
                      </Select>
                    </Field>
                  )}
                  {version.isFetching && <LoadingState label="正在读取问题" />}
                  {version.isError && (
                    <ErrorState
                      title="问题无法读取"
                      detail={errorText(version.error)}
                      onRetry={() => void version.refetch()}
                    />
                  )}
                  {version.data && (
                    <Field label="选择已有问题">
                      <Select
                        value={selectedQuestion?.question_id ?? ""}
                        onChange={(_, data) => setQuestionId(data.value)}
                        disabled={submission.isPending}
                      >
                        {version.data.questions.map((item) => (
                          <option
                            key={item.question_id}
                            value={item.question_id}
                          >
                            {item.text}
                          </option>
                        ))}
                      </Select>
                    </Field>
                  )}
                  {selectedQuestion && (
                    <p>
                      {selectedQuestion.purpose === "frozen_evaluation"
                        ? "冻结评估题 · 答案不进入内容优化"
                        : "优化问题"}{" "}
                      · {selectedQuestion.market} / {selectedQuestion.language}
                    </p>
                  )}
                </>
              )}
              <Field label="独立测量账号">
                <Select
                  value={selectedAccount?.account_id ?? ""}
                  onChange={(_, data) => setAccountId(data.value)}
                  disabled={submission.isPending || !accounts.length}
                >
                  {!selectedAccount && (
                    <option value="">
                      {accounts.length ? "请重新连接账号" : "尚未连接账号"}
                    </option>
                  )}
                  {accounts.map((account) => (
                    <option
                      key={account.account_id}
                      value={account.account_id}
                      disabled={!account.enabled || account.status !== "ready"}
                    >
                      {account.display_name ?? "Kimi 账号"} ·{" "}
                      {account.enabled && account.status === "ready"
                        ? "已登录"
                        : !account.enabled || account.status === "disabled"
                          ? "已停用"
                          : account.status === "unverified"
                            ? "身份待验证"
                            : "需要重新连接"}
                    </option>
                  ))}
                </Select>
              </Field>
              {accounts.some(
                (account) => !account.enabled || account.status !== "ready",
              ) && (
                <p>
                  <Link to={`/app/${tenantId}/${projectId}/channels`}>
                    重新连接已有账号
                  </Link>
                </p>
              )}
              {accountReady && options.isPending && (
                <LoadingState label="正在识别网页模型" />
              )}
              {options.isError && (
                <ErrorState
                  title="暂时无法读取网页模型"
                  detail={errorText(options.error)}
                  onRetry={() => void options.refetch()}
                />
              )}
              {accountReady && options.data && !models.length && (
                <p>
                  未发现可用网页模型。
                  <Button
                    appearance="subtle"
                    onClick={() => void options.refetch()}
                  >
                    重新识别模型
                  </Button>
                </p>
              )}
              {selectedModel && (
                <p>
                  模型：
                  {models.find((item) => item.id === selectedModel)?.label}
                </p>
              )}
              <details>
                <summary>高级选项</summary>
                <div className="channel-jobs-form">
                  <Field label="网页模型">
                    <Select
                      value={selectedModel ?? ""}
                      onChange={(_, data) => setModel(data.value)}
                      disabled={!models.length || submission.isPending}
                    >
                      {models.map((item) => (
                        <option key={item.id} value={item.id}>
                          {item.label}
                        </option>
                      ))}
                    </Select>
                  </Field>
                  {mode === "bound" && (
                    <Field label="问题集版本">
                      <Select
                        value={activeVersionId ?? ""}
                        onChange={(_, data) => {
                          setVersionId(data.value);
                          setQuestionId("");
                        }}
                        disabled={submission.isPending || !versions.data}
                      >
                        {versions.data?.items.map((item) => (
                          <option key={item.id} value={item.id}>
                            v{item.revision} · {item.name}
                          </option>
                        ))}
                      </Select>
                    </Field>
                  )}
                </div>
                {versions.isError && mode === "bound" && (
                  <ErrorState
                    title="版本列表无法读取"
                    detail={errorText(versions.error)}
                    onRetry={() => void versions.refetch()}
                  />
                )}
                <p>
                  {mode === "custom"
                    ? "自定义问题 · CN / zh-CN · 不进入内容优化。"
                    : "保留所选问题的原始版本和用途。"}{" "}
                  消费端网页联网搜索；未核验的搜索记录为缺测，不替换为普通回答。
                </p>
              </details>
              <Button
                appearance="primary"
                disabled={!valid || submission.isPending}
                onClick={start}
              >
                {submission.isPending
                  ? "正在提交测量…"
                  : submission.isError
                    ? "重试提交测量"
                    : "开始测量"}
              </Button>
              {submission.isSuccess && (
                <p role="status">测量计划已受理，执行结果以以下记录为准。</p>
              )}
              {submission.isError && (
                <ErrorState
                  title="测量提交未确认"
                  detail={`${errorText(submission.error)} 可重试，或在测量记录中查看提交结果。`}
                />
              )}
            </div>
          )}
        </>
      )}
      {recordsOnly && (
        <>
          <h2>测量记录</h2>
          <Button appearance="subtle" onClick={() => void history.refetch()}>
            刷新测量历史
          </Button>
          {history.isPending && <LoadingState label="正在读取独立测量历史" />}
          {history.isError && (
            <ErrorState
              title="测量历史无法读取"
              detail={errorText(history.error)}
              onRetry={() => void history.refetch()}
            />
          )}
          {history.data?.items.length === 0 && <p>还没有独立测量计划。</p>}
          {history.data?.items.map((plan) => (
            <p key={plan.plan_id}>
              <Button
                appearance="subtle"
                onClick={() => selectRecord(plan.plan_id)}
              >
                {plan.title} ·{" "}
                {new Date(plan.created_at).toLocaleString("zh-CN")}
              </Button>
            </p>
          ))}
          {previous.length > 0 && (
            <Button
              onClick={() => {
                setAfter(previous.at(-1));
                setPrevious((items) => items.slice(0, -1));
              }}
            >
              上一页测量
            </Button>
          )}
          {history.data?.next_after && (
            <Button
              onClick={() => {
                setPrevious((items) => [...items, after]);
                setAfter(history.data!.next_after!);
              }}
            >
              下一页测量
            </Button>
          )}
          {selectedId && detail.isPending && (
            <LoadingState label="正在读取测量计划" />
          )}
          {detail.isError && (
            <ErrorState
              title="测量计划无法读取"
              detail={errorText(detail.error)}
              onRetry={() => void detail.refetch()}
            />
          )}
          {detail.data && (
            <div className="channel-jobs-list">
              <p>
                计划 {detail.data.plan_id} · {detail.data.title}
              </p>
              {detail.data.targets.map((target) => (
                <PlannedTarget
                  key={target.target_id}
                  target={target}
                  tenantId={tenantId}
                  projectId={projectId}
                  canWrite={canWrite}
                  automatic
                />
              ))}
            </div>
          )}
        </>
      )}
    </section>
  );
}
