import { useEffect, useMemo, useRef, useState } from "react";
import {
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  ProgressBar,
  Select,
} from "@fluentui/react-components";
import {
  AddRegular,
  ArrowRightRegular,
  CheckmarkRegular,
  DeleteRegular,
} from "@fluentui/react-icons";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { createIdempotencyKey } from "../api/client";
import {
  type CreateProjectInput,
  type InitialSource,
  type Project,
  type ProjectEstimate,
  type ResourceMode,
  type SourceKind,
  type SourceVisibility,
  useCreateProjectMutation,
  useProjectEstimateQuery,
  useStartProjectMutation,
  useUpdateProjectMutation,
} from "../api/projects";

const DEFAULT_OBJECTIVE = "提升产品在购买决策问题中的可见度";
const weekDays = [
  ["monday", "周一"],
  ["tuesday", "周二"],
  ["wednesday", "周三"],
  ["thursday", "周四"],
  ["friday", "周五"],
  ["saturday", "周六"],
  ["sunday", "周日"],
] as const;

const steps = [
  {
    title: "品牌与资料",
    helper:
      "填写品牌并保存资料引用。URL 与粘贴文本会保存在配置中；资料解析会在启动后异步处理。",
  },
  {
    title: "目标与市场",
    helper: "产品和目标用户可留空；请明确市场、语言与优化目标。",
  },
  {
    title: "发布资源与预算",
    helper:
      "设置资源、预算与周报节奏，核对服务端估算后直接启动。没有额外确认或人工审批步骤。",
  },
];

type SourceDraft = {
  kind: "auto" | SourceKind;
  value: string;
  visibility: SourceVisibility;
  versionRef: string;
  contentHash: string;
};
type FieldErrors = Record<string, string>;
type SaveState = "idle" | "saving" | "saved" | "error";

function isUrl(value: string) {
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

function slugify(value: string) {
  const slug = value
    .trim()
    .toLocaleLowerCase()
    .normalize("NFKD")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 80);
  return slug || undefined;
}

function toMinorUnits(value: string) {
  const normalized = value.replace(/,/g, "").trim();
  if (!/^\d+(\.\d{1,2})?$/.test(normalized)) return null;
  const amount = Number(normalized);
  if (!Number.isSafeInteger(Math.round(amount * 100)) || amount < 0) {
    return null;
  }
  return Math.round(amount * 100);
}

function formatMinor(currency: string, amount: number) {
  try {
    return new Intl.NumberFormat("zh-CN", {
      style: "currency",
      currency,
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    }).format(amount / 100);
  } catch {
    return `${currency} ${(amount / 100).toFixed(2)}`;
  }
}

function estimateValue(
  value: { value: number | null; min: number | null; max: number | null },
  format: (amount: number) => string,
) {
  if (value.value !== null) return format(value.value);
  if (value.min !== null && value.max !== null) {
    return `${format(value.min)} – ${format(value.max)}`;
  }
  return null;
}

function unknownEstimateLabel(reason: string | null) {
  const normalized = reason?.toLocaleLowerCase() ?? "";
  if (normalized.includes("capability") || normalized.includes("能力")) {
    return "待能力快照";
  }
  if (normalized.includes("measurement") || normalized.includes("测量")) {
    return "待测量协议";
  }
  if (
    normalized.includes("component") ||
    normalized.includes("pricing") ||
    normalized.includes("cost")
  ) {
    return "待分项估算";
  }
  return "待知识规划";
}

function estimateBlockerText(code: string, fallback: string) {
  const labels: Record<string, string> = {
    knowledge_release_unavailable:
      "资料尚未形成不可变知识版本，因此文档清单尚未封存。",
    capability_snapshot_unavailable:
      "平台、账号和出口能力尚未形成快照，因此分发目标尚未展开。",
    measurement_protocol_unavailable:
      "测量协议和样本计划尚未冻结，因此测量分母未知。",
    pricing_snapshot_unavailable:
      "适用价格表尚未形成快照，因此当前不展示总价。",
  };
  return labels[code] ?? fallback;
}

function estimateAssumptionText(value: string) {
  const labels: Record<string, string> = {
    "Estimate is side-effect free: it creates no project, reservation, or task.":
      "估算本身不会创建项目、预算预留或后台任务。",
    "Zero budget permits later free knowledge work but must block paid actions.":
      "零预算仍可进行后续免费知识处理；付费动作会在执行前被预算规则阻止。",
  };
  return labels[value] ?? value;
}

function sourceValues(sources: SourceDraft[]): InitialSource[] {
  return sources
    .map((source) => ({ ...source, value: source.value.trim() }))
    .filter((source) => source.value.length > 0)
    .map((source) => ({
      kind:
        source.kind === "auto"
          ? isUrl(source.value)
            ? "url"
            : "text"
          : source.kind,
      value: source.value,
      visibility: source.visibility,
      version_ref: source.versionRef.trim() || null,
      content_hash: source.contentHash.trim() || null,
    }));
}

function defaultTimezone() {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "Asia/Shanghai";
}

function EstimateItem({
  label,
  estimate,
  currency,
}: {
  label: string;
  estimate: {
    state: "unknown" | "estimated" | "frozen";
    reason: string | null;
  } & (
    | {
        value: number | null;
        min: number | null;
        max: number | null;
      }
    | {
        value_minor: number | null;
        min_minor: number | null;
        max_minor: number | null;
      }
  );
  currency?: string;
}) {
  const amount =
    "value_minor" in estimate
      ? {
          value: estimate.value_minor,
          min: estimate.min_minor,
          max: estimate.max_minor,
        }
      : estimate;
  const display = estimateValue(
    amount,
    currency
      ? (amount) => formatMinor(currency, amount)
      : (amount) => String(amount),
  );

  return (
    <div>
      <span>{label}</span>
      <strong>
        {estimate.state === "unknown" || display === null
          ? unknownEstimateLabel(estimate.reason)
          : display}
      </strong>
      {estimate.reason && estimate.state !== "unknown" && (
        <small>{estimate.reason}</small>
      )}
    </div>
  );
}

function EstimatePanel({ estimate }: { estimate: ProjectEstimate }) {
  return (
    <section className="estimate-panel" aria-label="资源与预算估算">
      <div>
        <p className="eyebrow">服务端估算</p>
        <h3>资源与预算估算</h3>
        <p>
          估算版本 {estimate.estimator_version}。未知覆盖不会展示伪数字或总价。
        </p>
      </div>
      <div className="estimate-grid">
        <div>
          <span>月度预算</span>
          <strong>
            {formatMinor(
              estimate.budget.currency,
              estimate.budget.monthly_limit_minor,
            )}
          </strong>
        </div>
        <div>
          <span>测量预留</span>
          <strong>
            {formatMinor(
              estimate.budget.currency,
              estimate.budget.measurement_reserve_minor,
            )}
          </strong>
        </div>
        <EstimateItem
          label="第一阶段文档成本"
          estimate={estimate.costs.phase_one_documents}
          currency={estimate.budget.currency}
        />
        <EstimateItem
          label="第二阶段分发成本"
          estimate={estimate.costs.phase_two_distribution}
          currency={estimate.budget.currency}
        />
        <EstimateItem
          label="测量成本"
          estimate={estimate.costs.measurement}
          currency={estimate.budget.currency}
        />
        <EstimateItem
          label="预计总成本"
          estimate={estimate.costs.total}
          currency={estimate.budget.currency}
        />
      </div>
      <dl className="estimate-coverage">
        <div>
          <dt>文档</dt>
          <dd>
            <EstimateItem label="" estimate={estimate.coverage.documents} />
          </dd>
        </div>
        <div>
          <dt>文档 × 平台目标</dt>
          <dd>
            <EstimateItem
              label=""
              estimate={estimate.coverage.document_platform_targets}
            />
          </dd>
        </div>
        <div>
          <dt>测量样本</dt>
          <dd>
            <EstimateItem
              label=""
              estimate={estimate.coverage.measurement_samples}
            />
          </dd>
        </div>
      </dl>
      {(estimate.blockers.length > 0 || estimate.assumptions.length > 0) && (
        <div className="estimate-notes">
          {estimate.blockers.length > 0 && (
            <MessageBar intent="warning">
              <MessageBarBody>
                {estimate.blockers
                  .map((blocker) =>
                    estimateBlockerText(blocker.code, blocker.reason),
                  )
                  .join("；")}
              </MessageBarBody>
            </MessageBar>
          )}
          <MessageBar intent="info">
            <MessageBarBody>
              这是资源配置估算，不是曝光、引用、转化、收入或任何效果承诺。
              {estimate.assumptions.length > 0
                ? ` ${estimate.assumptions
                    .map(estimateAssumptionText)
                    .join("；")}`
                : ""}
            </MessageBarBody>
          </MessageBar>
        </div>
      )}
    </section>
  );
}

export function SetupPage({ tenantId: routeTenantId }: { tenantId?: string }) {
  const navigate = useNavigate();
  const { tenantId: paramTenantId } = useParams();
  const [searchParams] = useSearchParams();
  const tenantId =
    routeTenantId ??
    paramTenantId ??
    searchParams.get("tenant_id") ??
    undefined;
  const createProject = useCreateProjectMutation(tenantId);
  const startProject = useStartProjectMutation(tenantId);
  const [current, setCurrent] = useState(0);
  const [brandName, setBrandName] = useState("");
  const [sources, setSources] = useState<SourceDraft[]>([
    {
      kind: "auto",
      value: "",
      visibility: "public",
      versionRef: "",
      contentHash: "",
    },
  ]);
  const [productName, setProductName] = useState("");
  const [market, setMarket] = useState("中国大陆");
  const [language, setLanguage] = useState("简体中文");
  const [targetAudience, setTargetAudience] = useState("");
  const [objective, setObjective] = useState(DEFAULT_OBJECTIVE);
  const [competitorsText, setCompetitorsText] = useState("");
  const [resourceMode, setResourceMode] = useState<ResourceMode>("mixed");
  const [budgetCurrency, setBudgetCurrency] = useState("CNY");
  const [monthlyBudget, setMonthlyBudget] = useState("0");
  const [reservePercent, setReservePercent] = useState("20");
  const [reportTimezone, setReportTimezone] = useState(defaultTimezone);
  const [reportWeekday, setReportWeekday] = useState("monday");
  const [reportLocalTime, setReportLocalTime] = useState("09:00");
  const [cutoffWeekday, setCutoffWeekday] = useState("sunday");
  const [cutoffLocalTime, setCutoffLocalTime] = useState("23:59");
  const [errors, setErrors] = useState<FieldErrors>({});
  const [draft, setDraft] = useState<Project | null>(null);
  const [createSubmissionKey, setCreateSubmissionKey] = useState<string | null>(
    null,
  );
  const [startSubmissionKey, setStartSubmissionKey] = useState<string | null>(
    null,
  );
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [saveError, setSaveError] = useState<Error | null>(null);
  const updateProject = useUpdateProjectMutation(tenantId, draft?.id ?? "");
  const draftRef = useRef<Project | null>(null);
  const persistedFingerprintRef = useRef<string | null>(null);
  const saveQueueRef = useRef<Promise<unknown>>(Promise.resolve());

  const competitors = useMemo(
    () =>
      competitorsText
        .split(/[\n,，]/)
        .map((competitor) => competitor.trim())
        .filter(Boolean),
    [competitorsText],
  );

  const draftInput = useMemo<CreateProjectInput | undefined>(() => {
    if (brandName.trim().length < 2) return undefined;
    const budgetMinor = toMinorUnits(monthlyBudget) ?? 0;
    return {
      slug: slugify(brandName),
      display_name: brandName.trim(),
      settings: {
        brand_name: brandName.trim(),
        product_name: productName.trim() || null,
        market: market.trim(),
        language: language.trim(),
        target_audience: targetAudience.trim() || null,
        objective: objective.trim(),
        competitors,
        initial_sources: sourceValues(sources),
        resource_mode: resourceMode,
        monthly_budget_minor: budgetMinor,
        budget_currency: budgetCurrency.trim().toUpperCase(),
        monitoring_reserve_percent: Number(reservePercent),
        report_timezone: reportTimezone.trim(),
        report_schedule: {
          report_weekday: reportWeekday,
          report_local_time: reportLocalTime,
          cutoff_weekday: cutoffWeekday,
          cutoff_local_time: cutoffLocalTime,
          period_policy: "previous_calendar_week",
        },
        document_scope: {
          all_active_products: true,
          excluded_product_ids: [],
          markets: market.trim() ? [market.trim()] : [],
          languages: language.trim() ? [language.trim()] : [],
          content_types: ["product_page", "faq"],
          question_clusters: [],
        },
        distribution_scope: {
          mode: "all_eligible",
          included_platform_ids: [],
          excluded_platform_ids: [],
          resource_pool_ids: [],
          replication_policy: "one_account_per_platform",
        },
      },
    };
  }, [
    brandName,
    budgetCurrency,
    competitors,
    cutoffLocalTime,
    cutoffWeekday,
    language,
    market,
    monthlyBudget,
    objective,
    productName,
    reportLocalTime,
    reportTimezone,
    reportWeekday,
    reservePercent,
    resourceMode,
    sources,
    targetAudience,
  ]);
  const inputFingerprint = draftInput ? JSON.stringify(draftInput) : null;

  function errorsForStep(step: number): FieldErrors {
    const nextErrors: FieldErrors = {};
    if (step === 0) {
      if (brandName.trim().length < 2) {
        nextErrors.brandName = "请输入至少 2 个字符的品牌名称。";
      }
      if (sources.some((source) => source.value.trim().length > 4_000)) {
        nextErrors.sources = "单个资料输入最多可输入 4,000 个字符。";
      } else if (
        sources.some(
          (source) =>
            source.kind === "url" &&
            source.value.trim().length > 0 &&
            !isUrl(source.value.trim()),
        )
      ) {
        nextErrors.sources = "URL 类型的资料必须以 http:// 或 https:// 开头。";
      }
    }
    if (step === 1) {
      if (!market.trim()) nextErrors.market = "请输入目标市场。";
      if (!language.trim()) nextErrors.language = "请输入内容语言。";
      if (!objective.trim()) nextErrors.objective = "请输入优化目标。";
      if (competitors.length > 5) {
        nextErrors.competitors = "最多添加 5 个竞品。";
      }
    }
    if (step === 2) {
      if (!resourceMode) nextErrors.resourceMode = "请选择资源模式。";
      if (!/^[A-Z]{3}$/.test(budgetCurrency.trim())) {
        nextErrors.budgetCurrency = "请输入 3 位 ISO 币种代码，例如 CNY。";
      }
      if (toMinorUnits(monthlyBudget) === null) {
        nextErrors.monthlyBudget = "请输入不小于 0、最多两位小数的金额。";
      }
      const reserve = Number(reservePercent);
      if (!Number.isInteger(reserve) || reserve < 0 || reserve > 100) {
        nextErrors.reservePercent = "请输入 0 到 100 的整数百分比。";
      }
      try {
        Intl.DateTimeFormat(undefined, { timeZone: reportTimezone.trim() });
      } catch {
        nextErrors.reportTimezone =
          "请输入有效的 IANA 时区，例如 Asia/Shanghai。";
      }
      if (!/^\d{2}:\d{2}$/.test(reportLocalTime)) {
        nextErrors.reportLocalTime = "请输入 HH:MM 格式的时间。";
      }
      if (!/^\d{2}:\d{2}$/.test(cutoffLocalTime)) {
        nextErrors.cutoffLocalTime = "请输入 HH:MM 格式的时间。";
      }
    }
    return nextErrors;
  }

  function validateStep(step: number) {
    const nextErrors = errorsForStep(step);
    setErrors(nextErrors);
    return Object.keys(nextErrors).length === 0;
  }

  const configurationIsValid = [0, 1, 2].every(
    (step) => Object.keys(errorsForStep(step)).length === 0,
  );
  const estimateInput =
    current === steps.length - 1 && configurationIsValid
      ? draftInput
      : undefined;
  const estimate = useProjectEstimateQuery(tenantId, estimateInput);
  const isLast = current === steps.length - 1;
  const commandPending =
    createProject.isPending ||
    updateProject.isPending ||
    startProject.isPending;

  useEffect(() => {
    draftRef.current = draft;
  }, [draft]);

  function recordEdited() {
    if (startProject.isError) startProject.reset();
    setStartSubmissionKey(null);
    if (saveState === "error") {
      setSaveState("idle");
      setSaveError(null);
    }
  }

  async function ensureDraft() {
    if (draftRef.current) return draftRef.current;
    if (!tenantId || !draftInput) return null;
    const idempotencyKey = createSubmissionKey ?? createIdempotencyKey();
    setCreateSubmissionKey(idempotencyKey);
    setSaveState("saving");
    setSaveError(null);
    try {
      const created = await createProject.mutateAsync({
        input: draftInput,
        idempotencyKey,
      });
      draftRef.current = created;
      persistedFingerprintRef.current = JSON.stringify(draftInput);
      setDraft(created);
      setSaveState("saved");
      return created;
    } catch (error) {
      setSaveState("error");
      setSaveError(
        error instanceof Error ? error : new Error("草稿无法保存。"),
      );
      return null;
    }
  }

  function enqueueSave(input: CreateProjectInput) {
    const fingerprint = JSON.stringify(input);
    const job = saveQueueRef.current
      .catch(() => undefined)
      .then(async () => {
        const activeDraft = draftRef.current;
        if (!activeDraft || persistedFingerprintRef.current === fingerprint) {
          return activeDraft;
        }
        setSaveState("saving");
        setSaveError(null);
        try {
          const updated = await updateProject.mutateAsync({
            revision: activeDraft.revision,
            display_name: input.display_name,
            settings: input.settings,
          });
          draftRef.current = updated;
          persistedFingerprintRef.current = fingerprint;
          setDraft(updated);
          setSaveState("saved");
          return updated;
        } catch (error) {
          const saveFailure =
            error instanceof Error ? error : new Error("草稿无法保存。");
          setSaveState("error");
          setSaveError(saveFailure);
          throw saveFailure;
        }
      });
    saveQueueRef.current = job;
    return job;
  }

  async function flushDraft() {
    const activeDraft = await ensureDraft();
    if (!activeDraft || !draftInput) return null;
    await saveQueueRef.current.catch(() => undefined);
    const currentFingerprint = JSON.stringify(draftInput);
    if (persistedFingerprintRef.current !== currentFingerprint) {
      try {
        await enqueueSave(draftInput);
      } catch {
        return null;
      }
    }
    return draftRef.current;
  }

  useEffect(() => {
    if (!draft || !draftInput || !inputFingerprint) return;
    if (persistedFingerprintRef.current === inputFingerprint) return;
    const timeout = window.setTimeout(() => {
      void enqueueSave(draftInput).catch(() => undefined);
    }, 700);
    return () => window.clearTimeout(timeout);
  }, [draft, draftInput, inputFingerprint]);

  function updateSource(index: number, next: Partial<SourceDraft>) {
    recordEdited();
    setSources((previous) =>
      previous.map((source, sourceIndex) =>
        sourceIndex === index ? { ...source, ...next } : source,
      ),
    );
  }

  async function saveDraft() {
    if (!validateStep(0)) return;
    const saved = await flushDraft();
    if (saved) setSaveState("saved");
  }

  async function nextStep() {
    if (!validateStep(current)) return;
    if (current === 0) {
      const saved = await ensureDraft();
      if (!saved) return;
    } else if (!(await flushDraft())) {
      return;
    }
    setCurrent((value) => Math.min(value + 1, steps.length - 1));
  }

  async function moveToStep(index: number) {
    if (index === current) return;
    if (index < current) {
      setCurrent(index);
      return;
    }
    for (let step = current; step < index; step += 1) {
      if (!validateStep(step)) return;
    }
    if (!(await flushDraft())) return;
    setCurrent(index);
  }

  async function submit() {
    if (!tenantId || !configurationIsValid || !estimate.data) return;
    const saved = await flushDraft();
    if (!saved) return;
    const idempotencyKey = startSubmissionKey ?? createIdempotencyKey();
    setStartSubmissionKey(idempotencyKey);
    try {
      await startProject.mutateAsync({
        projectId: saved.id,
        expectedRevision: saved.revision,
        idempotencyKey,
      });
      navigate(`/app/${tenantId}/${saved.id}/overview`, { replace: true });
    } catch {
      // The server receives the same idempotency key on retry. The draft is
      // retained, so a failed start cannot make a second project.
    }
  }

  function finalAction() {
    if (sourceValues(sources).length === 0) {
      setErrors((previous) => ({
        ...previous,
        sources: "启动项目前请至少添加一个初始资料。",
      }));
      setCurrent(0);
      return;
    }
    if (!configurationIsValid) {
      for (const step of [0, 1, 2]) {
        if (!validateStep(step)) {
          setCurrent(step);
          return;
        }
      }
      return;
    }
    if (estimate.isError || !estimate.data) {
      void estimate.refetch();
      return;
    }
    void submit();
  }

  const estimateWaiting = isLast && (estimate.isPending || estimate.isFetching);
  const finalButtonLabel = estimateWaiting
    ? "正在计算估算…"
    : estimate.isError || !estimate.data
      ? "重试估算"
      : startProject.isPending
        ? "正在受理启动…"
        : startProject.isError
          ? "重试启动"
          : "启动项目";

  return (
    <div className="setup-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">P01 · 首次配置</p>
          <h1>创建并启动项目</h1>
          <p>保存草稿 → 服务端估算 → 受理启动；不需要额外确认或人工审批。</p>
        </div>
      </section>
      <Card className="setup-card">
        <div className="setup-steps">
          {steps.map((step, index) => (
            <button
              type="button"
              key={step.title}
              onClick={() => void moveToStep(index)}
              className={
                index === current ? "current" : index < current ? "done" : ""
              }
            >
              <span>{index < current ? <CheckmarkRegular /> : index + 1}</span>
              {step.title}
            </button>
          ))}
        </div>
        <ProgressBar
          value={(current + 1) / steps.length}
          aria-label={`第 ${current + 1} 步，共 ${steps.length} 步`}
        />
        <div className="setup-form">
          <p className="eyebrow">
            第 {current + 1} 步 / {steps.length} 步
          </p>
          <h2>{steps[current].title}</h2>
          <p>{steps[current].helper}</p>
          {saveState === "error" && saveError && current !== 2 && (
            <MessageBar intent="error" aria-live="polite">
              <MessageBarBody>
                草稿尚未保存：{saveError.message}。请重试保存后再继续。
              </MessageBarBody>
            </MessageBar>
          )}
          {current === 0 && (
            <div className="field-stack">
              <Field
                label="品牌名称"
                required
                validationMessage={errors.brandName}
                validationState={errors.brandName ? "error" : undefined}
              >
                <Input
                  value={brandName}
                  onChange={(_, data) => {
                    recordEdited();
                    setBrandName(data.value);
                  }}
                  placeholder="例如：Northstar AI"
                />
              </Field>
              <div className="source-fields">
                {sources.map((source, index) => (
                  <div className="source-field" key={index}>
                    <Field
                      label={
                        index === 0
                          ? "初始资料（可选）"
                          : `初始资料 ${index + 1}`
                      }
                      hint={
                        index === 0
                          ? "自动识别 URL 或粘贴文本。对象与已有知识只保存引用，不会上传文件。"
                          : undefined
                      }
                      validationMessage={
                        index === 0 ? errors.sources : undefined
                      }
                      validationState={errors.sources ? "error" : undefined}
                    >
                      <Input
                        value={source.value}
                        onChange={(_, data) =>
                          updateSource(index, { value: data.value })
                        }
                        placeholder={
                          source.kind === "object" ||
                          source.kind === "knowledge_collection"
                            ? "输入已有对象或知识集合引用"
                            : "https://example.com，或粘贴 FAQ / 产品资料"
                        }
                      />
                    </Field>
                    <Field label="资料类型">
                      <Select
                        value={source.kind}
                        onChange={(_, data) =>
                          updateSource(index, {
                            kind: data.value as SourceDraft["kind"],
                          })
                        }
                      >
                        <option value="auto">自动识别 URL / 文本</option>
                        <option value="url">URL</option>
                        <option value="text">粘贴文本</option>
                        <option value="object">已有对象引用</option>
                        <option value="knowledge_collection">
                          已有知识集合引用
                        </option>
                      </Select>
                    </Field>
                    <Field label="可见性">
                      <Select
                        value={source.visibility}
                        onChange={(_, data) =>
                          updateSource(index, {
                            visibility: data.value as SourceVisibility,
                          })
                        }
                      >
                        <option value="public">公开资料</option>
                        <option value="internal">内部资料</option>
                      </Select>
                    </Field>
                    {(source.kind === "object" ||
                      source.kind === "knowledge_collection") && (
                      <>
                        <Field label="版本引用（可选）">
                          <Input
                            value={source.versionRef}
                            onChange={(_, data) =>
                              updateSource(index, { versionRef: data.value })
                            }
                          />
                        </Field>
                        <Field label="内容哈希（可选）">
                          <Input
                            value={source.contentHash}
                            onChange={(_, data) =>
                              updateSource(index, { contentHash: data.value })
                            }
                          />
                        </Field>
                      </>
                    )}
                    {sources.length > 1 && (
                      <Button
                        appearance="subtle"
                        icon={<DeleteRegular />}
                        aria-label={`删除初始资料 ${index + 1}`}
                        onClick={() => {
                          recordEdited();
                          setSources((previous) =>
                            previous.filter(
                              (_, sourceIndex) => sourceIndex !== index,
                            ),
                          );
                        }}
                      />
                    )}
                  </div>
                ))}
                <Button
                  appearance="subtle"
                  icon={<AddRegular />}
                  disabled={sources.length >= 100}
                  onClick={() => {
                    recordEdited();
                    setSources((previous) => [
                      ...previous,
                      {
                        kind: "auto",
                        value: "",
                        visibility: "public",
                        versionRef: "",
                        contentHash: "",
                      },
                    ]);
                  }}
                >
                  添加资料引用
                </Button>
              </div>
              <MessageBar intent="info">
                <MessageBarBody>
                  文件上传 API 尚未接入，因此此处不会假装上传文件。已填写的
                  URL、文本或引用会作为初始资料配置保存，并在启动时冻结。
                </MessageBarBody>
              </MessageBar>
            </div>
          )}
          {current === 1 && (
            <div className="field-grid">
              <Field label="产品（可选）">
                <Input
                  value={productName}
                  onChange={(_, data) => {
                    recordEdited();
                    setProductName(data.value);
                  }}
                />
              </Field>
              <Field
                label="市场"
                required
                validationMessage={errors.market}
                validationState={errors.market ? "error" : undefined}
              >
                <Input
                  value={market}
                  onChange={(_, data) => {
                    recordEdited();
                    setMarket(data.value);
                    if (data.value.trim() === "中国大陆")
                      setBudgetCurrency("CNY");
                  }}
                />
              </Field>
              <Field
                label="语言"
                required
                validationMessage={errors.language}
                validationState={errors.language ? "error" : undefined}
              >
                <Input
                  value={language}
                  onChange={(_, data) => {
                    recordEdited();
                    setLanguage(data.value);
                  }}
                />
              </Field>
              <Field label="目标用户（可选）">
                <Input
                  value={targetAudience}
                  onChange={(_, data) => {
                    recordEdited();
                    setTargetAudience(data.value);
                  }}
                />
              </Field>
              <Field
                className="field-span-all"
                label="优化目标"
                required
                validationMessage={errors.objective}
                validationState={errors.objective ? "error" : undefined}
              >
                <Input
                  value={objective}
                  onChange={(_, data) => {
                    recordEdited();
                    setObjective(data.value);
                  }}
                />
              </Field>
              <Field
                className="field-span-all"
                label="竞品（可选，逗号或换行分隔）"
                validationMessage={errors.competitors}
                validationState={errors.competitors ? "error" : undefined}
              >
                <Input
                  value={competitorsText}
                  onChange={(_, data) => {
                    recordEdited();
                    setCompetitorsText(data.value);
                  }}
                  placeholder="最多 5 个竞品"
                />
              </Field>
            </div>
          )}
          {current === 2 && (
            <div className="field-grid">
              <Field
                label="资源模式"
                required
                validationMessage={errors.resourceMode}
                validationState={errors.resourceMode ? "error" : undefined}
              >
                <Select
                  value={resourceMode}
                  onChange={(_, data) => {
                    recordEdited();
                    setResourceMode(data.value as ResourceMode);
                  }}
                >
                  <option value="own">客户自有资源</option>
                  <option value="platform">总部资源</option>
                  <option value="mixed">混合资源</option>
                </Select>
              </Field>
              <Field
                label="预算币种"
                required
                validationMessage={errors.budgetCurrency}
                validationState={errors.budgetCurrency ? "error" : undefined}
              >
                <Input
                  value={budgetCurrency}
                  onChange={(_, data) => {
                    recordEdited();
                    setBudgetCurrency(data.value.toUpperCase());
                  }}
                  maxLength={3}
                />
              </Field>
              <Field
                label="月度预算"
                required
                hint="以主币种输入，最多两位小数；将以最小货币单位保存。"
                validationMessage={errors.monthlyBudget}
                validationState={errors.monthlyBudget ? "error" : undefined}
              >
                <Input
                  inputMode="decimal"
                  value={monthlyBudget}
                  onChange={(_, data) => {
                    recordEdited();
                    setMonthlyBudget(data.value);
                  }}
                />
              </Field>
              <Field
                label="测量预留（%）"
                required
                validationMessage={errors.reservePercent}
                validationState={errors.reservePercent ? "error" : undefined}
              >
                <Input
                  inputMode="numeric"
                  value={reservePercent}
                  onChange={(_, data) => {
                    recordEdited();
                    setReservePercent(data.value);
                  }}
                />
              </Field>
              <div className="field-span-all">
                <p className="eyebrow">高级设置</p>
              </div>
              <Field
                label="项目时区（IANA）"
                required
                validationMessage={errors.reportTimezone}
                validationState={errors.reportTimezone ? "error" : undefined}
              >
                <Input
                  value={reportTimezone}
                  onChange={(_, data) => {
                    recordEdited();
                    setReportTimezone(data.value);
                  }}
                  placeholder="Asia/Shanghai"
                />
              </Field>
              <Field label="周报日">
                <Select
                  value={reportWeekday}
                  onChange={(_, data) => {
                    recordEdited();
                    setReportWeekday(data.value);
                  }}
                >
                  {weekDays.map(([value, label]) => (
                    <option key={value} value={value}>
                      {label}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field
                label="周报本地时间"
                validationMessage={errors.reportLocalTime}
                validationState={errors.reportLocalTime ? "error" : undefined}
              >
                <Input
                  type="time"
                  value={reportLocalTime}
                  onChange={(_, data) => {
                    recordEdited();
                    setReportLocalTime(data.value);
                  }}
                />
              </Field>
              <Field label="统计截止日">
                <Select
                  value={cutoffWeekday}
                  onChange={(_, data) => {
                    recordEdited();
                    setCutoffWeekday(data.value);
                  }}
                >
                  {weekDays.map(([value, label]) => (
                    <option key={value} value={value}>
                      {label}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field
                label="统计截止本地时间"
                validationMessage={errors.cutoffLocalTime}
                validationState={errors.cutoffLocalTime ? "error" : undefined}
              >
                <Input
                  type="time"
                  value={cutoffLocalTime}
                  onChange={(_, data) => {
                    recordEdited();
                    setCutoffLocalTime(data.value);
                  }}
                />
              </Field>
              <div className="field-span-all">
                <MessageBar intent="info">
                  <MessageBarBody>
                    文档范围：全部有效产品；市场与语言按当前设置；问题簇待知识规划后确定。发布范围：全部适用平台，能力快照后确定；每个平台使用一个账号。
                  </MessageBarBody>
                </MessageBar>
              </div>
              {estimateWaiting && (
                <MessageBar intent="info" aria-live="polite">
                  <MessageBarBody>
                    正在按当前配置计算资源覆盖和预算…
                  </MessageBarBody>
                </MessageBar>
              )}
              {estimate.isError && (
                <MessageBar intent="error" aria-live="polite">
                  <MessageBarBody>
                    估算暂时无法加载：{estimate.error.message}
                    。请重试估算后再启动。
                  </MessageBarBody>
                </MessageBar>
              )}
              {estimate.data && <EstimatePanel estimate={estimate.data} />}
              {saveState === "error" && saveError && (
                <MessageBar intent="error" aria-live="polite">
                  <MessageBarBody>
                    草稿尚未保存：{saveError.message}。请重试保存后再启动。
                  </MessageBarBody>
                </MessageBar>
              )}
              {startProject.isError && (
                <MessageBar intent="error" aria-live="polite">
                  <MessageBarBody>
                    草稿已保存，但启动暂时无法受理：{startProject.error.message}
                    。重试只会使用同一启动请求标识，不会创建第二个项目。
                  </MessageBarBody>
                </MessageBar>
              )}
            </div>
          )}
        </div>
        <footer className="setup-footer">
          <Button
            appearance="secondary"
            disabled={current === 0 || commandPending}
            onClick={() => setCurrent((value) => value - 1)}
          >
            上一步
          </Button>
          <Button
            appearance="secondary"
            disabled={commandPending || !draftInput}
            onClick={() => void saveDraft()}
          >
            {saveState === "saving"
              ? "正在保存…"
              : draft
                ? "保存草稿"
                : "保存为草稿"}
          </Button>
          <Button
            appearance="primary"
            disabled={estimateWaiting || commandPending}
            onClick={isLast ? finalAction : () => void nextStep()}
            icon={isLast ? <CheckmarkRegular /> : <ArrowRightRegular />}
          >
            {isLast ? finalButtonLabel : "下一步"}
          </Button>
        </footer>
      </Card>
    </div>
  );
}
