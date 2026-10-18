import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Accordion,
  AccordionHeader,
  AccordionItem,
  AccordionPanel,
  Button,
  Field,
  Select,
  Textarea,
  Tooltip,
} from "@fluentui/react-components";
import { AddRegular } from "@fluentui/react-icons";
import {
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { Link, useSearchParams } from "react-router-dom";
import { useAuth } from "../auth/AuthProvider";
import { useChannelData } from "../api/channels";
import {
  createMeasurementPlan,
  getMeasurementPlan,
  getMeasurementOptions,
  listMeasurementPlans,
  type StandaloneMeasurementRequest,
  type StandaloneMeasurementPlan,
} from "../api/channelJobs";
import {
  getQuestionSetVersion,
  listAllQuestionSets,
  listAllQuestionSetVersions,
} from "../api/questions";
import { ErrorState, LoadingState } from "../components/AsyncState";
import { QuestionSetRecommendationTrend } from "../components/QuestionSetRecommendationTrend";
import { PlannedTarget } from "./ChannelJobsPage";
import { formatUiDate } from "../i18n";
import "./measurementMessages";

const errorText = (error: unknown) =>
  error instanceof Error ? error.message : "请稍后重试。";

export function StandaloneMeasurementPanel({
  tenantId,
  projectId,
  canWrite,
  recordsOnly = false,
  active = true,
}: {
  tenantId: string;
  projectId: string;
  canWrite: boolean;
  recordsOnly?: boolean;
  active?: boolean;
}) {
  const { t } = useTranslation("measurement");
  const [params, setParams] = useSearchParams();
  const requestedSetId = params.get("set") ?? params.get("questionSetId") ?? "";
  const requestedVersionId =
    params.get("version") ?? params.get("versionId") ?? "";
  const selectedId = params.get("record") ?? params.get("planId") ?? undefined;
  const selectedRankReport = params.get("rankReport");
  const [historyOpen, setHistoryOpen] = useState(
    !selectedId || Boolean(selectedRankReport),
  );
  useEffect(() => {
    setHistoryOpen(!selectedId || Boolean(selectedRankReport));
  }, [selectedId, selectedRankReport, tenantId, projectId]);
  const selectRecord = (
    id: string,
    questionSetId?: string,
    questionVersionId?: string,
  ) => {
    setHistoryOpen(false);
    setParams((previous) => {
      const next = new URLSearchParams(previous);
      next.set("tab", "records");
      next.delete("planId");
      next.delete("target");
      next.delete("attempt");
      next.delete("analysisRevision");
      next.delete("rankReport");
      next.set("record", id);
      if (questionSetId !== undefined) {
        next.delete("questionSetId");
        next.delete("versionId");
        if (questionSetId) next.set("set", questionSetId);
        else next.delete("set");
        if (questionVersionId) next.set("version", questionVersionId);
        else next.delete("version");
        if (questionSetId) next.delete("folder");
        else next.set("folder", "temporary");
      }
      return next;
    });
  };
  const { session } = useAuth();
  const client = useQueryClient();
  const channels = useChannelData(tenantId, projectId);
  const [question, setQuestion] = useState("");
  const [accountId, setAccountId] = useState("");
  const [model, setModel] = useState("");
  const [mode, setMode] = useState<"custom" | "bound">(
    requestedSetId ? "bound" : "custom",
  );
  const [setId, setSetId] = useState(requestedSetId);
  const [versionId, setVersionId] = useState(requestedVersionId);
  useEffect(() => {
    if (recordsOnly || !requestedSetId) return;
    setMode("bound");
    setSetId(requestedSetId);
    setVersionId(requestedVersionId);
    setQuestionId("");
  }, [recordsOnly, requestedSetId, requestedVersionId]);
  const [questionId, setQuestionId] = useState("");
  const [after, setAfter] = useState<string>();
  const [previous, setPrevious] = useState<(string | undefined)[]>([]);
  const [openFolders, setOpenFolders] = useState<Record<string, boolean>>({});
  useEffect(() => {
    if (!requestedSetId) return;
    setOpenFolders((previous) =>
      previous[requestedSetId] === true
        ? previous
        : { ...previous, [requestedSetId]: true },
    );
  }, [requestedSetId, selectedRankReport]);
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
  const accounts = (channels.accounts.data?.items ?? []).filter((account) =>
    channels.platforms.data?.items.some(
      (platform) =>
        platform.id === account.platform && platform.purpose === "measurement",
    ),
  );
  const supportsMeasurement = (platformId: string) =>
    channels.platforms.data?.items.some(
      (platform) =>
        platform.id === platformId &&
        platform.login_supported &&
        platform.measurement_supported,
    ) === true;
  const selectedAccount = accounts.find(
    (account) =>
      account.account_id ===
      (accountId ||
        accounts.find(
          (item) =>
            item.enabled &&
            item.status === "ready" &&
            supportsMeasurement(item.platform),
        )?.account_id),
  );
  const accountReady = Boolean(
    selectedAccount?.enabled &&
    selectedAccount.status === "ready" &&
    supportsMeasurement(selectedAccount.platform),
  );
  const options = useQuery({
    queryKey: [...key, "options", selectedAccount?.account_id],
    queryFn: () =>
      getMeasurementOptions(tenantId, projectId, selectedAccount!.account_id),
    enabled: Boolean(
      session && accountReady && canWrite && !recordsOnly && active,
    ),
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
    enabled: Boolean(session && (recordsOnly || canWrite)),
    retry: false,
  });
  const selectedSet =
    sets.data?.items.find((item) => item.id === setId) ??
    (!setId ? sets.data?.items[0] : undefined);
  const groups = new Map<string, Map<string, StandaloneMeasurementPlan[]>>();
  for (const item of sets.data?.items ?? [])
    groups.set(item.id, new Map([[item.current_version_id, []]]));
  const temporaryPlans: StandaloneMeasurementPlan[] = [];
  for (const plan of history.data?.items ?? []) {
    const associations = new Map<string, Set<string>>();
    let temporary = plan.targets.length === 0;
    for (const target of plan.targets) {
      if (target.input.kind !== "measure") continue;
      const reference = target.input.question_binding?.reference;
      if (!reference) {
        temporary = true;
        continue;
      }
      const versions =
        associations.get(reference.question_set_id) ?? new Set<string>();
      versions.add(reference.question_set_version_id);
      associations.set(reference.question_set_id, versions);
    }
    if (temporary) temporaryPlans.push(plan);
    for (const [setId, versionIds] of associations) {
      const versions =
        groups.get(setId) ?? new Map<string, StandaloneMeasurementPlan[]>();
      for (const versionId of versionIds) {
        versions.set(versionId, [...(versions.get(versionId) ?? []), plan]);
      }
      groups.set(setId, versions);
    }
  }
  if (requestedSetId && requestedVersionId) {
    const versions =
      groups.get(requestedSetId) ??
      new Map<string, StandaloneMeasurementPlan[]>();
    if (!versions.has(requestedVersionId)) versions.set(requestedVersionId, []);
    groups.set(requestedSetId, versions);
  }
  const folderIds = [...groups.keys()];
  const folderVersions = useQueries({
    queries: folderIds.map((id) => ({
      queryKey: [...key, "versions", id],
      queryFn: () => listAllQuestionSetVersions(tenantId, projectId, id),
      enabled: Boolean(
        session &&
        recordsOnly &&
        historyOpen &&
        !(selectedRankReport && requestedSetId === id) &&
        (openFolders[id] ?? requestedSetId === id),
      ),
      retry: false,
    })),
  });
  for (const [index, id] of folderIds.entries()) {
    for (const version of folderVersions[index].data?.items ?? []) {
      if (!groups.get(id)!.has(version.id)) groups.get(id)!.set(version.id, []);
    }
  }
  const recordButton = (
    plan: StandaloneMeasurementPlan,
    setId: string,
    versionId?: string,
  ) => (
    <li key={plan.plan_id}>
      <Button
        appearance="subtle"
        aria-current={selectedId === plan.plan_id ? "true" : undefined}
        onClick={() => selectRecord(plan.plan_id, setId, versionId)}
      >
        {plan.title} · {formatUiDate(plan.created_at)}
      </Button>
    </li>
  );
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
  const selectedQuestions = questionId
    ? (version.data?.questions.filter(
        (item) => item.question_id === questionId,
      ) ?? [])
    : (version.data?.questions ?? []);
  const refreshAccounts = channels.accounts.refetch;
  useEffect(() => {
    if (recordsOnly || !active) return;
    const refresh = () => {
      if (document.visibilityState !== "hidden") void refreshAccounts();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [recordsOnly, active, refreshAccounts]);
  const submission = useMutation({
    mutationFn: (input: StandaloneMeasurementRequest) =>
      createMeasurementPlan(tenantId, projectId, input),
    onSuccess: (plan) => {
      pendingRequest.current = null;
      selectRecord(
        plan.plan_id,
        mode === "bound" ? selectedSet?.id : "",
        mode === "bound" ? version.data?.id : undefined,
      );
      client.setQueryData([...key, "detail", plan.plan_id], plan);
      setAfter(undefined);
      setPrevious([]);
      void client.invalidateQueries({ queryKey: [...key, "list"] });
    },
  });
  function start() {
    if (!selectedAccount || !selectedModel || !accountReady) return;
    const common = {
      account_id: selectedAccount.account_id,
      provider: selectedAccount.platform,
      model: selectedModel,
      surface: "consumer_web",
      search_mode: "web_search",
      protocol_version: "v1",
      sample_ordinal: 0,
    };
    const values =
      mode === "bound" &&
      selectedSet &&
      selectedQuestions.length &&
      version.data
        ? {
            measurements: [],
            bound_measurements: selectedQuestions.map((item) => ({
              ...common,
              question: {
                question_set_id: selectedSet.id,
                question_set_version_id: version.data!.id,
                question_id: item.question_id,
                question_revision_id: item.id,
              },
            })),
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
      ? Boolean(selectedQuestions.length && version.data)
      : Boolean(question.trim()) &&
        new TextEncoder().encode(question.trim()).length <= 4000);
  return (
    <section className="channel-jobs-section" aria-label="独立问题测量">
      {!recordsOnly && (
        <>
          <h2>开始测量</h2>
          <p>输入问题，选择账号，查看联网搜索答案。</p>
          {channels.platforms.isPending && (
            <LoadingState label={t("platformCatalogLoading")} />
          )}
          {channels.platforms.isError && (
            <ErrorState
              title={t("platformCatalogError")}
              detail={errorText(channels.platforms.error)}
              onRetry={() => void channels.platforms.refetch()}
            />
          )}
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
            !channels.platforms.isPending &&
            !channels.platforms.isError &&
            !accounts.length &&
            canWrite && (
              <p role="status">
                {t("noMeasurementAccount")}{" "}
                <Link to={`/app/${tenantId}/${projectId}/channels/connect`}>
                  {t("connectMeasurementAccount")}
                </Link>
              </p>
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
                        value={questionId}
                        onChange={(_, data) => setQuestionId(data.value)}
                        disabled={submission.isPending}
                      >
                        <option value="">
                          {t("allSetQuestions", {
                            count: version.data.questions.length,
                          })}
                        </option>
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
                  {selectedQuestion &&
                    (questionId || selectedQuestions.length === 1) && (
                      <p>
                        {selectedQuestion.purpose === "frozen_evaluation"
                          ? "冻结评估题 · 答案不进入内容优化"
                          : "优化问题"}{" "}
                        · {selectedQuestion.market} /{" "}
                        {selectedQuestion.language}
                      </p>
                    )}
                  {!questionId && selectedQuestions.length > 1 && (
                    <p>
                      {t("allQuestionsMeasurement", {
                        count: selectedQuestions.length,
                      })}
                    </p>
                  )}
                </>
              )}
              <Field label="独立测量账号">
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <Select
                    style={{ flex: 1, minWidth: 0 }}
                    value={selectedAccount?.account_id ?? ""}
                    onChange={(_, data) => setAccountId(data.value)}
                    disabled={submission.isPending || !accounts.length}
                  >
                    {!selectedAccount && (
                      <option value="">
                        {accounts.length
                          ? accounts.some((account) =>
                              supportsMeasurement(account.platform),
                            )
                            ? "请重新连接账号"
                            : t("noMeasurementAccount")
                          : "尚未连接账号"}
                      </option>
                    )}
                    {accounts.map((account) => (
                      <option
                        key={account.account_id}
                        value={account.account_id}
                        disabled={
                          !account.enabled ||
                          account.status !== "ready" ||
                          !supportsMeasurement(account.platform)
                        }
                      >
                        {account.display_name ??
                          t("platformAccount", {
                            platform:
                              channels.platforms.data?.items.find(
                                (platform) => platform.id === account.platform,
                              )?.label ?? account.platform,
                          })}{" "}
                        ·{" "}
                        {!supportsMeasurement(account.platform)
                          ? t("measurementUnavailable")
                          : account.enabled && account.status === "ready"
                            ? "已登录"
                            : !account.enabled || account.status === "disabled"
                              ? "已停用"
                              : account.status === "unverified"
                                ? "身份待验证"
                                : "需要重新连接"}
                      </option>
                    ))}
                  </Select>
                  {accounts.length > 0 && (
                    <Tooltip
                      content={t("addMeasurementAccount")}
                      relationship="label"
                    >
                      <Link
                        to={`/app/${tenantId}/${projectId}/channels/connect`}
                        aria-label={t("addMeasurementAccount")}
                        style={{
                          display: "inline-flex",
                          alignItems: "center",
                          justifyContent: "center",
                          minWidth: 32,
                          minHeight: 32,
                        }}
                      >
                        <AddRegular aria-hidden="true" />
                      </Link>
                    </Tooltip>
                  )}
                </div>
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
                  {t(
                    mode === "custom"
                      ? "customMeasurementScope"
                      : "questionSetMeasurementScope",
                  )}{" "}
                  {t("measurementMethod")}
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
          <Accordion
            collapsible
            openItems={historyOpen ? ["history"] : []}
            onToggle={(_, data) =>
              setHistoryOpen(data.openItems.includes("history"))
            }
          >
            <AccordionItem value="history">
              <AccordionHeader>{t("measurementHistory")}</AccordionHeader>
              <AccordionPanel>
                <Button
                  appearance="subtle"
                  onClick={() => void history.refetch()}
                >
                  刷新测量历史
                </Button>
                {history.isPending && (
                  <LoadingState label="正在读取独立测量历史" />
                )}
                {history.isError && (
                  <ErrorState
                    title="测量历史无法读取"
                    detail={errorText(history.error)}
                    onRetry={() => void history.refetch()}
                  />
                )}
                {history.data?.items.length === 0 && (
                  <p>还没有独立测量计划。</p>
                )}
                {sets.isError && (
                  <ErrorState
                    title={t("historySetsUnavailable")}
                    detail={errorText(sets.error)}
                    onRetry={() => void sets.refetch()}
                  />
                )}
                <nav aria-label={t("measurementFolders")}>
                  {[...groups].map(([groupId, versions]) => {
                    const versionHistory =
                      folderVersions[folderIds.indexOf(groupId)];
                    const set = sets.data?.items.find(
                      (item) => item.id === groupId,
                    );
                    const count = new Set(
                      [...versions.values()].flat().map((plan) => plan.plan_id),
                    ).size;
                    return (
                      <details
                        key={groupId}
                        open={
                          openFolders[groupId] ?? requestedSetId === groupId
                        }
                        onToggle={(event) => {
                          const open = event.currentTarget.open;
                          setOpenFolders((previous) =>
                            previous[groupId] === open
                              ? previous
                              : { ...previous, [groupId]: open },
                          );
                        }}
                      >
                        <summary>
                          {set?.name ?? t("unnamedQuestionSet")} ·{" "}
                          {t("folderRecordCount", { count })}
                        </summary>
                        <Link
                          to={`/app/${tenantId}/${projectId}/measurement?tab=sets&set=${encodeURIComponent(groupId)}`}
                        >
                          {t("viewQuestionSet")}
                        </Link>{" "}
                        {canWrite && (
                          <Link
                            to={`/app/${tenantId}/${projectId}/measurement?tab=measure&set=${encodeURIComponent(groupId)}`}
                          >
                            {t("measureQuestionSet")}
                          </Link>
                        )}
                        {count === 0 && <p>{t("folderNoRecords")}</p>}
                        {versionHistory.isFetching && (
                          <LoadingState
                            label={t("folderVersionsLoading")}
                            compact
                          />
                        )}
                        {versionHistory.isError && (
                          <ErrorState
                            title={t("folderVersionsUnavailable")}
                            detail={errorText(versionHistory.error)}
                            onRetry={() => void versionHistory.refetch()}
                          />
                        )}
                        {[...versions].map(([versionId, plans]) => {
                          const savedVersion = versionHistory.data?.items.find(
                            (item) => item.id === versionId,
                          );
                          return (
                            <details key={versionId} open>
                              <summary>
                                {set?.current_version_id === versionId
                                  ? t("currentSetVersion", {
                                      revision: set.current_revision,
                                    })
                                  : t("savedSetVersion", {
                                      value: savedVersion
                                        ? `v${savedVersion.revision}`
                                        : versionId,
                                    })}
                              </summary>
                              <QuestionSetRecommendationTrend
                                tenantId={tenantId}
                                projectId={projectId}
                                questionSetId={groupId}
                                questionSetVersionId={versionId}
                              />
                              <ul>
                                {plans.map((plan) =>
                                  recordButton(plan, groupId, versionId),
                                )}
                              </ul>
                            </details>
                          );
                        })}
                      </details>
                    );
                  })}
                  <details open={!requestedSetId}>
                    <summary>
                      {t("temporaryMeasurements")} ·{" "}
                      {t("folderRecordCount", { count: temporaryPlans.length })}
                    </summary>
                    {temporaryPlans.length ? (
                      <ul>
                        {temporaryPlans.map((plan) => recordButton(plan, ""))}
                      </ul>
                    ) : (
                      <p>{t("folderNoRecords")}</p>
                    )}
                  </details>
                </nav>
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
              </AccordionPanel>
            </AccordionItem>
          </Accordion>
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
              <p>{detail.data.title}</p>
              <details>
                <summary>{t("resultPlanDetails")}</summary>
                <p>{t("resultPlanId", { value: detail.data.plan_id })}</p>
              </details>
              {detail.data.targets
                .filter((target) => {
                  if (
                    params.get("target") &&
                    params.get("target") !== target.target_id
                  )
                    return false;
                  if (target.input.kind !== "measure") return true;
                  const reference = target.input.question_binding?.reference;
                  if (requestedSetId)
                    return (
                      reference?.question_set_id === requestedSetId &&
                      (!requestedVersionId ||
                        reference.question_set_version_id ===
                          requestedVersionId)
                    );
                  if (params.get("folder") === "temporary") return !reference;
                  return true;
                })
                .map((target) => (
                  <PlannedTarget
                    key={target.target_id}
                    target={target}
                    tenantId={tenantId}
                    projectId={projectId}
                    canWrite={canWrite}
                    accountDisplayName={
                      channels.accounts.data?.items.find(
                        (account) =>
                          account.account_id === target.input.account_id,
                      )?.display_name
                    }
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
