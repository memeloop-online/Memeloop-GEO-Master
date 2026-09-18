import {
  Button,
  Card,
  CardHeader,
  MessageBar,
  MessageBarBody,
  Spinner,
} from "@fluentui/react-components";
import {
  ArrowRightRegular,
  ArrowSyncRegular,
  DataUsageRegular,
} from "@fluentui/react-icons";
import { useParams } from "react-router-dom";
import {
  type ProjectOverview,
  useProjectOverviewQuery,
  useProjectStartQuery,
} from "../api/projects";
import { LoopProgress, type LoopStep } from "../components/LoopProgress";
import { StatusPill } from "../components/StatusPill";

function formatMinor(currency: string, minor: number) {
  const amount = minor / 100;
  try {
    return new Intl.NumberFormat("zh-CN", {
      style: "currency",
      currency,
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    }).format(amount);
  } catch {
    return `${currency} ${amount.toFixed(2)}`;
  }
}

function formatTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

function projectStatus(status: ProjectOverview["project"]["status"]) {
  const labels = {
    draft: "草稿",
    active: "已启动",
    paused: "已暂停",
    archived: "已归档",
  } as const;
  return labels[status];
}

function resourceModeLabel(
  mode: ProjectOverview["project"]["settings"]["resource_mode"],
) {
  return {
    own: "客户自有资源",
    platform: "总部资源",
    mixed: "混合资源",
  }[mode];
}

function buildLoop(overview: ProjectOverview): LoopStep[] {
  const sourceCount = overview.knowledge.source_count;
  const savedSources = overview.project.settings.initial_sources?.length ?? 0;
  const knowledge =
    overview.knowledge.status === "ready"
      ? {
          state: "complete" as const,
          detail: `${sourceCount} 个来源 · ${overview.knowledge.fact_count} 条事实`,
        }
      : overview.knowledge.status === "importing"
        ? {
            state: "active" as const,
            detail: `${sourceCount} 个来源正在导入`,
          }
        : {
            state: "queued" as const,
            detail:
              savedSources > 0
                ? `已冻结 ${savedSources} 个来源，W03 才会导入`
                : "—",
          };
  const benchmark =
    overview.benchmark.status === "ready"
      ? {
          state: "complete" as const,
          detail: `${overview.benchmark.effective_samples ?? "—"} / ${overview.benchmark.planned_samples} 有效样本`,
        }
      : overview.benchmark.status === "running"
        ? {
            state: "active" as const,
            detail: `正在建立基线 · ${overview.benchmark.effective_samples ?? "—"} / ${overview.benchmark.planned_samples}`,
          }
        : {
            state: "queued" as const,
            detail: "尚未建立",
          };
  const cycle =
    overview.cycle.status === "running"
      ? { state: "active" as const, detail: "正在运行" }
      : overview.cycle.status === "paused"
        ? { state: "blocked" as const, detail: "已暂停后续执行" }
        : { state: "queued" as const, detail: "—" };

  return [
    { id: "knowledge", label: "知识", ...knowledge },
    {
      id: "questions",
      label: "问题",
      state: overview.benchmark.question_count > 0 ? "active" : "queued",
      detail:
        overview.benchmark.question_count > 0
          ? `${overview.benchmark.question_count} 个问题`
          : "—",
    },
    { id: "baseline", label: "基线", ...benchmark },
    { id: "strategy", label: "策略", state: "queued", detail: "—" },
    { id: "content", label: "内容", state: "queued", detail: "—" },
    { id: "checks", label: "检查", state: "queued", detail: "—" },
    { id: "schedule", label: "调度", state: "queued", detail: "—" },
    {
      id: "publish",
      label: "发布",
      state: overview.content.published_count > 0 ? "complete" : "queued",
      detail:
        overview.content.published_count > 0
          ? `${overview.content.published_count} 个已发布`
          : "—",
    },
    {
      id: "verify",
      label: "验证",
      state: overview.content.verified_count > 0 ? "complete" : "queued",
      detail:
        overview.content.verified_count > 0
          ? `${overview.content.verified_count} 个已验证`
          : "—",
    },
    { id: "remeasure", label: "复测", state: "queued", detail: "—" },
    { id: "optimize", label: "优化", ...cycle },
  ];
}

function cyclePill(overview: ProjectOverview) {
  if (overview.cycle.status === "running") {
    return <StatusPill status="active" text="持续运行" />;
  }
  if (overview.cycle.status === "paused") {
    return <StatusPill status="blocked" text="已暂停" />;
  }
  return (
    <StatusPill
      status="queued"
      text={overview.project.status === "active" ? "等待知识处理" : "尚未启动"}
    />
  );
}

function actionHref(base: string, href: string) {
  if (href.startsWith("/")) return href;
  return `${base}/${href.replace(/^\/+/, "")}`;
}

export function OverviewPage() {
  const { tenantId, projectId } = useParams();
  const { data, isPending, isError, refetch, isFetching } =
    useProjectOverviewQuery(tenantId, projectId);
  const {
    data: startAcceptance,
    isFetching: isStartFetching,
    refetch: refetchStart,
  } = useProjectStartQuery(tenantId, projectId);

  if (isPending) {
    return (
      <div className="page-loading">
        <Spinner label="正在加载项目总览" />
      </div>
    );
  }

  if (isError || !data) {
    return (
      <MessageBar intent="error">
        <MessageBarBody>
          项目总览暂时无法加载。请检查连接后重试。
        </MessageBarBody>
        <Button appearance="subtle" onClick={() => void refetch()}>
          重试
        </Button>
      </MessageBar>
    );
  }

  const base = `/app/${tenantId}/${projectId}`;
  const savedSources = data.project.settings.initial_sources?.length ?? 0;
  const baselineNotStarted = data.benchmark.status === "not_started";
  const sourceNotice =
    data.knowledge.status === "empty" && savedSources > 0
      ? `已保存并冻结 ${savedSources} 个知识来源；W03 才会开始导入资料，目前没有已解析资料。`
      : data.knowledge.status === "empty"
        ? "还没有企业知识。导入产品资料、官网内容或常见问题，系统将提取可追溯事实。"
        : null;

  return (
    <div className="overview-page">
      <section className="page-hero overview-hero">
        <div>
          <p className="eyebrow">P02 · 项目总览</p>
          <h1>{data.project.display_name}</h1>
          <p>
            {data.project.settings.market} · {data.project.settings.language} ·{" "}
            {projectStatus(data.project.status)}
          </p>
        </div>
        <div className="hero-actions">
          <div className="budget">
            <span>项目状态</span>
            <strong>{projectStatus(data.project.status)}</strong>
            <small>更新于 {formatTime(data.updated_at)}</small>
          </div>
          <Button
            appearance="secondary"
            icon={<ArrowSyncRegular />}
            disabled={isFetching || isStartFetching}
            onClick={() => {
              void refetch();
              void refetchStart();
            }}
          >
            刷新
          </Button>
        </div>
      </section>

      {data.cycle.status === "paused" && (
        <MessageBar intent="warning" className="persistent-notice">
          <MessageBarBody>
            自动优化已暂停：新的外部请求不会开始；在途请求仍可能完成并产生费用。
          </MessageBarBody>
        </MessageBar>
      )}
      {startAcceptance && (
        <MessageBar intent="success" className="persistent-notice">
          <MessageBarBody>
            项目已启动（受理操作 {startAcceptance.operation_id}
            ）。文档与分发清单骨架已创建，尚未封存并等待知识处理；这不代表资料已经解析、基线已经建立或计划正在运行。
            配置修订 {startAcceptance.config_revision_id} · 文档清单{" "}
            {startAcceptance.document_manifest.manifest_id} · 分发清单{" "}
            {startAcceptance.distribution_manifest.manifest_id}
          </MessageBarBody>
        </MessageBar>
      )}
      {sourceNotice && (
        <MessageBar intent="info" className="persistent-notice">
          <MessageBarBody>{sourceNotice}</MessageBarBody>
        </MessageBar>
      )}

      <LoopProgress
        steps={buildLoop(data)}
        status={
          data.cycle.status === "running"
            ? "active"
            : data.cycle.status === "paused"
              ? "blocked"
              : "queued"
        }
        statusText={
          data.cycle.status === "running"
            ? "持续运行"
            : data.cycle.status === "paused"
              ? "已暂停"
              : data.project.status === "active"
                ? "等待知识处理"
                : "尚未启动"
        }
      />

      <section className="metric-grid" aria-label="项目关键指标">
        <Card className="metric-card">
          <span>已解析知识来源</span>
          <strong>{data.knowledge.source_count}</strong>
          <small>
            {data.knowledge.status === "ready"
              ? `${data.knowledge.fact_count} 条事实可用`
              : (sourceNotice ?? "—")}
          </small>
        </Card>
        <Card className="metric-card">
          <span>有效 / 计划样本</span>
          <strong>
            {data.benchmark.effective_samples ?? "—"} /{" "}
            {data.benchmark.planned_samples}
          </strong>
          <small>
            {baselineNotStarted ? "尚未建立" : data.benchmark.status}
          </small>
        </Card>
        <Card className="metric-card">
          <span>已发布资产</span>
          <strong>{data.content.published_count}</strong>
          <small>
            {data.content.verified_count} 已验证 · {data.content.blocked_count}{" "}
            已阻断
          </small>
        </Card>
        <Card className="metric-card">
          <span>本期已结算成本</span>
          <strong>
            {formatMinor(data.cost.currency, data.cost.settled_minor)}
          </strong>
          <small>
            已预留 {formatMinor(data.cost.currency, data.cost.reserved_minor)}
          </small>
        </Card>
      </section>

      <div className="overview-columns">
        <section className="column-main" aria-label="基线和下一步">
          <Card className="panel-card">
            <CardHeader
              header={
                <div>
                  <h2>基线与测量</h2>
                  <p>按有效样本展示；缺测始终显示为“—”，不会按 0 计算。</p>
                </div>
              }
              action={
                <Button
                  as="a"
                  href={`${base}/measurement`}
                  appearance="subtle"
                  icon={<DataUsageRegular />}
                >
                  查看测量
                </Button>
              }
            />
            <div className="overview-summary-list">
              <div>
                <span>问题</span>
                <strong>{data.benchmark.question_count}</strong>
              </div>
              <div>
                <span>计划样本</span>
                <strong>{data.benchmark.planned_samples}</strong>
              </div>
              <div>
                <span>有效样本</span>
                <strong>{data.benchmark.effective_samples ?? "—"}</strong>
              </div>
              <div>
                <span>状态</span>
                <strong>
                  {baselineNotStarted ? "尚未建立" : data.benchmark.status}
                </strong>
              </div>
            </div>
          </Card>
          <Card className="panel-card">
            <CardHeader
              header={
                <div>
                  <h2>当前闭环状态</h2>
                  <p>只显示当前服务已返回的动作与状态。</p>
                </div>
              }
              action={cyclePill(data)}
            />
            {data.next_action ? (
              <div className="overview-next-action">
                <div>
                  <span>下一步</span>
                  <strong>{data.next_action.label}</strong>
                </div>
                <Button
                  as="a"
                  href={actionHref(base, data.next_action.href)}
                  appearance="secondary"
                  icon={<ArrowRightRegular />}
                >
                  查看下一步
                </Button>
              </div>
            ) : (
              <div className="overview-empty-state">
                <strong>
                  {baselineNotStarted ? "等待知识处理" : "暂无可执行动作"}
                </strong>
                <p>—</p>
              </div>
            )}
          </Card>
        </section>
        <aside className="column-side" aria-label="项目配置与成本">
          <Card className="panel-card">
            <CardHeader
              header={
                <div>
                  <h2>项目配置</h2>
                  <p>已保存的真实项目元数据。</p>
                </div>
              }
            />
            <dl className="project-meta-list">
              <div>
                <dt>产品</dt>
                <dd>{data.project.settings.product_name || "—"}</dd>
              </div>
              <div>
                <dt>资源模式</dt>
                <dd>
                  {resourceModeLabel(data.project.settings.resource_mode)}
                </dd>
              </div>
              <div>
                <dt>月度预算</dt>
                <dd>
                  {formatMinor(
                    data.project.settings.budget_currency,
                    data.project.settings.monthly_budget_minor,
                  )}
                </dd>
              </div>
              <div>
                <dt>测量预留</dt>
                <dd>{data.project.settings.monitoring_reserve_percent}%</dd>
              </div>
              <div>
                <dt>竞品</dt>
                <dd>
                  {data.project.settings.competitors.length > 0
                    ? data.project.settings.competitors.join("、")
                    : "—"}
                </dd>
              </div>
            </dl>
          </Card>
          <Card className="panel-card">
            <CardHeader
              header={
                <div>
                  <h2>成本状态</h2>
                  <p>预留与已结算成本分别展示。</p>
                </div>
              }
            />
            <div className="overview-summary-list compact">
              <div>
                <span>已预留</span>
                <strong>
                  {formatMinor(data.cost.currency, data.cost.reserved_minor)}
                </strong>
              </div>
              <div>
                <span>已结算</span>
                <strong>
                  {formatMinor(data.cost.currency, data.cost.settled_minor)}
                </strong>
              </div>
            </div>
          </Card>
        </aside>
      </div>
    </div>
  );
}
