import { useMemo, useState } from "react";
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
  type ProjectEstimate,
  type ResourceMode,
  type SourceVisibility,
  useCreateProjectMutation,
  useProjectEstimateQuery,
  useStartProjectMutation,
} from "../api/projects";

const steps = [
  {
    title: "品牌与来源",
    helper:
      "品牌名与至少一个知识来源为必填项。来源仅会保存并在启动时冻结，W03 才会实际导入。",
  },
  {
    title: "市场与目标",
    helper: "选择产品、市场、语言与目标用户；最多添加 5 个竞品。",
  },
  {
    title: "资源与预算",
    helper: "选择资源模式、预算币种与月度预算；默认预留 20% 用于测量。",
  },
  {
    title: "检查并启动",
    helper:
      "先计算资源与预算区间，再创建草稿并受理启动 Operation；W03 才会导入已冻结的资料。",
  },
];

type SourceDraft = { value: string; visibility: SourceVisibility };
type FieldErrors = Record<string, string>;

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

function formatRange(currency: string, minimum: number, maximum: number) {
  return `${formatMinor(currency, minimum)} – ${formatMinor(currency, maximum)}`;
}

function sourceValues(sources: SourceDraft[]): InitialSource[] {
  return sources
    .map((source) => ({ ...source, value: source.value.trim() }))
    .filter((source) => source.value.length > 0)
    .map((source) => ({
      kind: isUrl(source.value) ? "url" : "text",
      value: source.value,
      visibility: source.visibility,
    }));
}

function EstimatePanel({ estimate }: { estimate: ProjectEstimate }) {
  const currency = estimate.currency;
  return (
    <section className="estimate-panel" aria-label="资源与预算估算">
      <div>
        <p className="eyebrow">服务端估算</p>
        <h3>资源与预算估算</h3>
        <p>按当前配置计算；更改配置后会自动重新估算。</p>
      </div>
      <div className="estimate-grid">
        <div>
          <span>请求月度预算</span>
          <strong>
            {formatMinor(currency, estimate.requested_monthly_budget_minor)}
          </strong>
        </div>
        <div>
          <span>测量预留</span>
          <strong>
            {formatMinor(currency, estimate.monitoring_reserve_minor)}
          </strong>
        </div>
        <div>
          <span>第一阶段区间</span>
          <strong>
            {formatRange(
              currency,
              estimate.phase_one.minimum_minor,
              estimate.phase_one.maximum_minor,
            )}
          </strong>
        </div>
        <div>
          <span>第二阶段区间</span>
          <strong>
            {formatRange(
              currency,
              estimate.phase_two.minimum_minor,
              estimate.phase_two.maximum_minor,
            )}
          </strong>
        </div>
        <div className="estimate-total">
          <span>预计总资源成本区间</span>
          <strong>
            {formatRange(
              currency,
              estimate.total.minimum_minor,
              estimate.total.maximum_minor,
            )}
          </strong>
        </div>
      </div>
      <dl className="estimate-coverage">
        <div>
          <dt>来源</dt>
          <dd>{estimate.coverage.source_count}</dd>
        </div>
        <div>
          <dt>首轮文档</dt>
          <dd>{estimate.coverage.document_count}</dd>
        </div>
        <div>
          <dt>文档 × 平台目标</dt>
          <dd>{estimate.coverage.document_platform_target_count}</dd>
        </div>
        <div>
          <dt>计划测量样本</dt>
          <dd>{estimate.coverage.measurement_sample_count}</dd>
        </div>
      </dl>
      <div className="estimate-notes">
        <div>
          <strong>计算依据</strong>
          <ul>
            {estimate.basis.map((item) => (
              <li key={item}>{item}</li>
            ))}
          </ul>
        </div>
        <MessageBar intent="warning">
          <MessageBarBody>
            这是资源与预算估算，不是曝光、引用、转化、收入或任何效果承诺。
            {estimate.assumptions.length > 0
              ? ` ${estimate.assumptions.join("；")}`
              : ""}
          </MessageBarBody>
        </MessageBar>
      </div>
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
    { value: "", visibility: "public" },
  ]);
  const [productName, setProductName] = useState("");
  const [market, setMarket] = useState("中国大陆");
  const [language, setLanguage] = useState("简体中文");
  const [targetAudience, setTargetAudience] = useState("");
  const [competitorsText, setCompetitorsText] = useState("");
  const [resourceMode, setResourceMode] = useState<ResourceMode>("mixed");
  const [budgetCurrency, setBudgetCurrency] = useState("CNY");
  const [monthlyBudget, setMonthlyBudget] = useState("");
  const [reservePercent, setReservePercent] = useState("20");
  const [errors, setErrors] = useState<FieldErrors>({});
  const [createdProjectId, setCreatedProjectId] = useState<string | null>(null);
  const [createSubmissionKey, setCreateSubmissionKey] = useState<string | null>(
    null,
  );
  const [startSubmissionKey, setStartSubmissionKey] = useState<string | null>(
    null,
  );

  const isLast = current === steps.length - 1;
  const isDraftCreated = createdProjectId !== null;
  const commandPending = createProject.isPending || startProject.isPending;
  const competitors = useMemo(
    () =>
      competitorsText
        .split(/[\n,，]/)
        .map((competitor) => competitor.trim())
        .filter(Boolean),
    [competitorsText],
  );

  function errorsForStep(step: number): FieldErrors {
    const nextErrors: FieldErrors = {};
    if (step === 0) {
      if (brandName.trim().length < 2) {
        nextErrors.brandName = "请输入至少 2 个字符的品牌名称。";
      }
      const savedSources = sourceValues(sources);
      if (savedSources.length === 0) {
        nextErrors.sources = "请至少添加一个知识来源。";
      }
      if (sources.some((source) => source.value.trim().length > 4_000)) {
        nextErrors.sources = "单个知识来源最多可输入 4,000 个字符。";
      }
    }
    if (step === 1) {
      if (!productName.trim()) nextErrors.productName = "请输入目标产品。";
      if (!market.trim()) nextErrors.market = "请输入目标市场。";
      if (!language.trim()) nextErrors.language = "请输入内容语言。";
      if (!targetAudience.trim()) {
        nextErrors.targetAudience = "请输入目标用户。";
      }
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
  const estimateInput = useMemo<CreateProjectInput | undefined>(() => {
    if (!isLast || !configurationIsValid) return undefined;
    return {
      slug: slugify(brandName),
      display_name: brandName.trim(),
      settings: {
        brand_name: brandName.trim(),
        product_name: productName.trim(),
        market: market.trim(),
        language: language.trim(),
        competitors,
        resource_mode: resourceMode,
        monthly_budget_minor: toMinorUnits(monthlyBudget)!,
        budget_currency: budgetCurrency.trim().toUpperCase(),
        monitoring_reserve_percent: Number(reservePercent),
        target_audience: targetAudience.trim(),
        initial_sources: sourceValues(sources),
      },
    };
  }, [
    brandName,
    budgetCurrency,
    competitors,
    configurationIsValid,
    isLast,
    language,
    market,
    monthlyBudget,
    productName,
    reservePercent,
    resourceMode,
    sources,
    targetAudience,
  ]);
  const estimate = useProjectEstimateQuery(tenantId, estimateInput);

  function clearSubmissionState() {
    if (isDraftCreated) return;
    setCreateSubmissionKey(null);
    setStartSubmissionKey(null);
    if (createProject.isError) createProject.reset();
    if (startProject.isError) startProject.reset();
  }

  function updateSource(index: number, next: Partial<SourceDraft>) {
    clearSubmissionState();
    setSources((previous) =>
      previous.map((source, sourceIndex) =>
        sourceIndex === index ? { ...source, ...next } : source,
      ),
    );
  }

  function nextStep() {
    if (validateStep(current)) setCurrent((value) => value + 1);
  }

  function moveToStep(index: number) {
    if (isDraftCreated || index === current) return;
    if (index < current) {
      setCurrent(index);
      return;
    }
    for (let step = current; step < index; step += 1) {
      if (!validateStep(step)) return;
    }
    setCurrent(index);
  }

  async function requestStart(projectId: string, idempotencyKey: string) {
    try {
      const operation = await startProject.mutateAsync({
        projectId,
        idempotencyKey,
      });
      navigate(`/app/${tenantId}/${projectId}/overview`, {
        replace: true,
        state: {
          projectStartAcceptance: {
            projectId,
            operation,
            acceptedAt: new Date().toISOString(),
          },
        },
      });
    } catch {
      // The draft id and this stable key stay in state, so retry only starts it.
    }
  }

  async function submit() {
    if (!tenantId) return;
    if (createdProjectId) {
      const retryStartKey = startSubmissionKey ?? createIdempotencyKey();
      setStartSubmissionKey(retryStartKey);
      await requestStart(createdProjectId, retryStartKey);
      return;
    }
    if (!configurationIsValid || !estimateInput || !estimate.data) return;

    const createKey = createSubmissionKey ?? createIdempotencyKey();
    const startKey = startSubmissionKey ?? createIdempotencyKey();
    setCreateSubmissionKey(createKey);
    setStartSubmissionKey(startKey);
    try {
      const project = await createProject.mutateAsync({
        input: estimateInput,
        idempotencyKey: createKey,
      });
      setCreatedProjectId(project.id);
      await requestStart(project.id, startKey);
    } catch {
      // Reusing createKey lets a response-loss retry recover the same draft.
    }
  }

  function finalAction() {
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

  const commandError = startProject.isError
    ? startProject.error
    : createProject.isError
      ? createProject.error
      : undefined;
  const estimateWaiting = isLast && (estimate.isPending || estimate.isFetching);
  const finalButtonLabel = estimateWaiting
    ? "正在计算估算…"
    : estimate.isError || !estimate.data
      ? "重试估算"
      : commandPending
        ? createProject.isPending
          ? "正在创建草稿…"
          : "正在受理启动…"
        : isDraftCreated
          ? "重试启动"
          : "创建草稿并启动";

  return (
    <div className="setup-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">P01 · 首次配置</p>
          <h1>创建并启动项目</h1>
          <p>配置 → 服务端估算 → 创建草稿 → 受理启动 Operation。</p>
        </div>
      </section>
      <Card className="setup-card">
        <div className="setup-steps">
          {steps.map((step, index) => (
            <button
              type="button"
              key={step.title}
              disabled={isDraftCreated}
              onClick={() => moveToStep(index)}
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
                    clearSubmissionState();
                    setBrandName(data.value);
                  }}
                  placeholder="例如：Northstar AI"
                />
              </Field>
              <div className="source-fields">
                {sources.map((source, index) => (
                  <div className="source-field" key={index}>
                    <Field
                      label={index === 0 ? "知识来源" : `知识来源 ${index + 1}`}
                      required={index === 0}
                      hint={
                        index === 0
                          ? "网页会识别为 URL；FAQ 或产品资料摘要会按文本保存。"
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
                        placeholder="https://example.com，或粘贴 FAQ / 产品资料"
                      />
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
                    {sources.length > 1 && (
                      <Button
                        appearance="subtle"
                        icon={<DeleteRegular />}
                        aria-label={`删除知识来源 ${index + 1}`}
                        onClick={() => {
                          clearSubmissionState();
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
                    clearSubmissionState();
                    setSources((previous) => [
                      ...previous,
                      { value: "", visibility: "public" },
                    ]);
                  }}
                >
                  添加知识来源
                </Button>
              </div>
              <MessageBar intent="info">
                <MessageBarBody>
                  来源只会保存并在启动时冻结。W03
                  才会导入，不会在本阶段伪称资料已经解析。
                </MessageBarBody>
              </MessageBar>
            </div>
          )}
          {current === 1 && (
            <div className="field-grid">
              <Field
                label="目标产品"
                required
                validationMessage={errors.productName}
                validationState={errors.productName ? "error" : undefined}
              >
                <Input
                  value={productName}
                  onChange={(_, data) => {
                    clearSubmissionState();
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
                    clearSubmissionState();
                    setMarket(data.value);
                    if (data.value.trim() === "中国大陆") {
                      setBudgetCurrency("CNY");
                    }
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
                    clearSubmissionState();
                    setLanguage(data.value);
                  }}
                />
              </Field>
              <Field
                label="目标用户"
                required
                validationMessage={errors.targetAudience}
                validationState={errors.targetAudience ? "error" : undefined}
              >
                <Input
                  value={targetAudience}
                  onChange={(_, data) => {
                    clearSubmissionState();
                    setTargetAudience(data.value);
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
                    clearSubmissionState();
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
                    clearSubmissionState();
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
                    clearSubmissionState();
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
                    clearSubmissionState();
                    setMonthlyBudget(data.value);
                  }}
                  placeholder="例如：60000.00"
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
                    clearSubmissionState();
                    setReservePercent(data.value);
                  }}
                />
              </Field>
            </div>
          )}
          {current === 3 && (
            <div className="review-grid">
              <div>
                <span>品牌</span>
                <strong>{brandName}</strong>
              </div>
              <div>
                <span>待冻结知识来源</span>
                <strong>{sourceValues(sources).length} 个</strong>
              </div>
              <div>
                <span>市场与语言</span>
                <strong>
                  {market} · {language}
                </strong>
              </div>
              <div>
                <span>资源与月度预算</span>
                <strong>
                  {resourceMode} · {budgetCurrency} {monthlyBudget}
                </strong>
              </div>
              <div>
                <span>测量预留</span>
                <strong>{reservePercent}%</strong>
              </div>
              {estimateWaiting && (
                <MessageBar intent="info" aria-live="polite">
                  <MessageBarBody>
                    正在按当前配置计算资源覆盖和预算区间…
                  </MessageBarBody>
                </MessageBar>
              )}
              {estimate.isError && (
                <MessageBar intent="error" aria-live="polite">
                  <MessageBarBody>
                    估算暂时无法加载：{estimate.error.message}
                    。请重试估算后再创建项目。
                  </MessageBarBody>
                </MessageBar>
              )}
              {estimate.data && <EstimatePanel estimate={estimate.data} />}
              <MessageBar intent="info">
                <MessageBarBody>
                  项目草稿创建后，启动 Operation 会冻结本次配置与来源；W03
                  才会导入资料。没有导入或测量结果时，总览会诚实显示“—”或等待状态。
                </MessageBarBody>
              </MessageBar>
              {commandError && (
                <MessageBar intent="error" aria-live="polite">
                  <MessageBarBody>
                    {isDraftCreated
                      ? `草稿已创建（${createdProjectId}），但启动 Operation 暂时无法受理：${commandError.message}。重试只会重新请求启动，不会创建第二个项目。`
                      : `项目草稿暂时无法创建：${commandError.message}。请重试；系统会复用本次创建标识。`}
                  </MessageBarBody>
                </MessageBar>
              )}
            </div>
          )}
        </div>
        <footer className="setup-footer">
          <Button
            appearance="secondary"
            disabled={current === 0 || commandPending || isDraftCreated}
            onClick={() => setCurrent((value) => value - 1)}
          >
            上一步
          </Button>
          <Button
            appearance="primary"
            disabled={estimateWaiting || commandPending}
            onClick={isLast ? finalAction : nextStep}
            icon={isLast ? <CheckmarkRegular /> : <ArrowRightRegular />}
          >
            {isLast ? finalButtonLabel : "下一步"}
          </Button>
        </footer>
      </Card>
    </div>
  );
}
