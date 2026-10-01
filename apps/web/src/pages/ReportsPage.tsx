import {
  Badge,
  Button,
  Card,
  MessageBar,
  MessageBarBody,
} from "@fluentui/react-components";
import { ArrowSyncRegular } from "@fluentui/react-icons";
import { Link, useParams } from "react-router-dom";
import {
  useReportEvidenceQuery,
  useReportQuery,
  useReportsQuery,
  type ReportCoverage,
  type ReportEvidenceReference,
  type ReportSnapshot,
} from "../api/reports";
import { ApiError } from "../api/client";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import { downloadReportCsv } from "./reportsCsv";
import "./ReportsPage.css";

const manifestLabels = {
  document: "文档清单",
  distribution: "分发清单",
  measurement: "测量清单",
};

const coverageLabels: Record<string, string> = {
  planned: "已规划",
  blocked: "阻断",
  deferred: "延后",
  not_applicable: "不适用",
  published: "已发布",
  verified: "已验证",
  unknown: "结果未知",
  failed: "失败",
  missing: "缺测",
  pending: "待完成",
  unmaterialized: "尚未形成目标",
  observed: "有效观察",
  refused: "拒答",
  not_mentioned: "未提及",
};

function dateTime(value: string | null, timezone: string): string {
  if (!value) return "未记录";
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) return value;
  const options: Intl.DateTimeFormatOptions = {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  };
  try {
    return new Intl.DateTimeFormat("zh-CN", {
      ...options,
      timeZone: timezone,
    }).format(parsed);
  } catch (error) {
    if (!(error instanceof RangeError)) throw error;
    return `${new Intl.DateTimeFormat("zh-CN", {
      ...options,
      timeZone: "UTC",
    }).format(parsed)} UTC（报告时区无效）`;
  }
}

function coverageName(key: string) {
  return coverageLabels[key] ?? key.replaceAll("_", " ");
}

function CoveragePanel({
  title,
  coverage,
  note,
}: {
  title: string;
  coverage: ReportCoverage;
  note: string;
}) {
  const available = coverage.availability === "available";
  return (
    <Card className="report-panel">
      <h2>{title}</h2>
      <p>{note}</p>
      <p>
        清单状态：
        {coverage.availability === "available"
          ? "可用"
          : coverage.availability === "unsealed"
            ? "未封存"
            : "不可用"}
        {" · "}计划分母{" "}
        {coverage.expected_count === null ? "未知" : coverage.expected_count}
        {" · "}已观察 {coverage.observed_count}
      </p>
      {!available && (
        <MessageBar intent="warning">
          <MessageBarBody>
            {coverage.reason || "输入尚未形成可用于此快照的封存清单。"}
            本报告不将缺失输入计为失败、未提及或零分。
          </MessageBarBody>
        </MessageBar>
      )}
      {Object.keys(coverage.counts).length > 0 ? (
        <table className="report-table">
          <thead>
            <tr>
              <th scope="col">状态</th>
              <th scope="col">数量</th>
            </tr>
          </thead>
          <tbody>
            {Object.entries(coverage.counts).map(([key, count]) => (
              <tr key={key}>
                <th scope="row">{coverageName(key)}</th>
                <td>{count}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="report-muted">此输入没有可展示的分类计数。</p>
      )}
    </Card>
  );
}

function EvidenceItem({
  item,
  timezone,
}: {
  item: ReportEvidenceReference;
  timezone: string;
}) {
  return (
    <li id={`evidence-${encodeURIComponent(item.evidence_id)}`}>
      <article className="report-evidence">
        <h3>{item.summary}</h3>
        <dl>
          <dt>证据类型</dt>
          <dd>{item.kind}</dd>
          <dt>资源 ID</dt>
          <dd>{item.resource_id}</dd>
          <dt>资源版本</dt>
          <dd>{item.resource_version ?? "未记录"}</dd>
          <dt>发生时间</dt>
          <dd>{dateTime(item.occurred_at, timezone)}</dd>
          <dt>接收时间</dt>
          <dd>{dateTime(item.received_at, timezone)}</dd>
          <dt>证据 ID</dt>
          <dd>{item.evidence_id}</dd>
        </dl>
      </article>
    </li>
  );
}

function SnapshotDetail({
  snapshot,
  evidence,
  evidenceError,
  evidenceLoading,
  onEvidenceRetry,
  onRefresh,
  refreshing,
}: {
  snapshot: ReportSnapshot;
  evidence: ReportEvidenceReference[];
  evidenceError: Error | null;
  evidenceLoading: boolean;
  onEvidenceRetry: () => void;
  onRefresh: () => void;
  refreshing: boolean;
}) {
  const evidenceIds = new Set(evidence.map((item) => item.evidence_id));
  return (
    <div className="workbench-page reports-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">P14 · 不可变周报快照</p>
          <h1>
            周报 ·{" "}
            {dateTime(
              snapshot.report_window_start_at,
              snapshot.report_timezone,
            )}
          </h1>
          <p>
            {dateTime(
              snapshot.report_window_start_at,
              snapshot.report_timezone,
            )}{" "}
            —{" "}
            {dateTime(snapshot.report_window_end_at, snapshot.report_timezone)}
            {" · "}时区 {snapshot.report_timezone}
          </p>
        </div>
        <div className="report-actions">
          <Button onClick={() => downloadReportCsv(snapshot)}>
            下载覆盖与证据 CSV
          </Button>
          <Button
            icon={<ArrowSyncRegular />}
            disabled={refreshing}
            onClick={onRefresh}
          >
            刷新快照
          </Button>
        </div>
      </section>
      <Link to="../reports">← 返回报告列表</Link>
      <MessageBar intent={snapshot.status === "partial" ? "warning" : "info"}>
        <MessageBarBody>
          {snapshot.status === "partial" ? "部分覆盖" : "完整覆盖"}
          ：这是截止 {dateTime(
            snapshot.cutoff_at,
            snapshot.report_timezone,
          )}{" "}
          的固定快照，后续证据不会静默改写本版本。生成于{" "}
          {dateTime(snapshot.generated_at, snapshot.report_timezone)}。
        </MessageBarBody>
      </MessageBar>
      <Card className="report-panel">
        <h2>快照范围与版本</h2>
        <dl className="report-metadata">
          <dt>报告 ID</dt>
          <dd>{snapshot.report_id}</dd>
          <dt>周期 ID</dt>
          <dd>{snapshot.cycle_id}</dd>
          <dt>修订</dt>
          <dd>{snapshot.revision}</dd>
          <dt>更正前版本</dt>
          <dd>{snapshot.correction_of ?? "无"}</dd>
          <dt>Reduce 版本</dt>
          <dd>{snapshot.reducer_version}</dd>
          <dt>输入摘要</dt>
          <dd>{snapshot.input_hash}</dd>
          <dt>证据水位</dt>
          <dd>{dateTime(snapshot.evidence_as_of, snapshot.report_timezone)}</dd>
        </dl>
        <h3>输入清单版本</h3>
        {snapshot.input_manifest_versions.length ? (
          <ul>
            {snapshot.input_manifest_versions.map((item) => (
              <li key={`${item.kind}/${item.manifest_id}/${item.revision}`}>
                {manifestLabels[item.kind]} {item.manifest_id} · 修订{" "}
                {item.revision} · {item.sealed ? "已封存" : "未封存"} · 计划{" "}
                {item.expected_count ?? "未知"}
              </li>
            ))}
          </ul>
        ) : (
          <p>未提供输入清单版本；不能将覆盖范围视为完整。</p>
        )}
      </Card>
      <section className="report-coverage" aria-label="三类独立覆盖">
        <CoveragePanel
          title="文档覆盖与资料缺口"
          coverage={snapshot.documents}
          note="规划分支不等于已生成主文档。阻断与延后保留在计划分母内。"
        />
        <CoveragePanel
          title="发布目标覆盖"
          coverage={snapshot.publications}
          note="发布回执与公开验证分别计数；结果未知仍需查回，不视作失败。"
        />
        <CoveragePanel
          title="AI 渠道测量覆盖"
          coverage={snapshot.measurements}
          note="采集失败、拒答与未提及不同。不同问题集、平台、观测面和市场不能合并成趋势。"
        />
      </section>
      <section className="report-section" aria-label="平台与测量比较组">
        <h2>独立覆盖组</h2>
        <div className="report-coverage">
          <Card className="report-panel">
            <h3>发布平台</h3>
            {snapshot.publication_groups.length ? (
              snapshot.publication_groups.map((group) => (
                <div key={group.platform_id} className="report-group">
                  <h4>{group.platform_id}</h4>
                  <p>
                    {group.coverage.availability === "available"
                      ? "可用"
                      : group.coverage.reason || "不可用"}
                    {" · "}计划 {group.coverage.expected_count ?? "未知"}
                    {" · "}已观察 {group.coverage.observed_count}
                  </p>
                  <p>
                    {Object.entries(group.coverage.counts)
                      .map(([key, count]) => `${coverageName(key)} ${count}`)
                      .join(" · ") || "无分类计数"}
                  </p>
                  {group.coverage.reason && (
                    <p className="report-muted">{group.coverage.reason}</p>
                  )}
                </div>
              ))
            ) : (
              <p>没有可展示的逐平台发布组；不能推断各平台的状态。</p>
            )}
          </Card>
          <Card className="report-panel">
            <h3>AI 测量比较口径</h3>
            {snapshot.measurement_groups.length ? (
              snapshot.measurement_groups.map((group) => (
                <div key={group.comparison_key} className="report-group">
                  <h4>{group.comparison_key}</h4>
                  <p>
                    {group.coverage.availability === "available"
                      ? "可用"
                      : group.coverage.reason || "不可用"}
                    {" · "}计划 {group.coverage.expected_count ?? "未知"}
                    {" · "}已观察 {group.coverage.observed_count}
                  </p>
                  <p>
                    {Object.entries(group.coverage.counts)
                      .map(([key, count]) => `${coverageName(key)} ${count}`)
                      .join(" · ") || "无分类计数"}
                  </p>
                  {group.coverage.reason && (
                    <p className="report-muted">{group.coverage.reason}</p>
                  )}
                </div>
              ))
            ) : (
              <p>没有可展示的独立 AI 渠道组；不能推断提及、引用或趋势。</p>
            )}
          </Card>
        </div>
        <p>
          比较组仅按冻结口径分别呈现，不跨问题集、平台、观测面、市场或采样协议汇总为趋势。
          本周无全量复测时，也不将旧样本标为本周结果。
        </p>
      </section>
      <section className="report-section" aria-label="结论">
        <h2>结论与证据</h2>
        {snapshot.findings.length ? (
          <ul className="report-findings">
            {snapshot.findings.map((finding) => (
              <li key={finding.finding_id}>
                <Card className="report-panel">
                  <h3>{finding.summary}</h3>
                  <p>类型：{finding.kind}</p>
                  {finding.insufficient_reason && (
                    <p>数据不足：{finding.insufficient_reason}</p>
                  )}
                  {finding.evidence_ids.length ? (
                    <ul>
                      {finding.evidence_ids.map((id) => (
                        <li key={id}>
                          {evidenceIds.has(id) ? (
                            <a href={`#evidence-${encodeURIComponent(id)}`}>
                              查看原始证据 {id}
                            </a>
                          ) : evidenceLoading || evidenceError ? (
                            <>证据 {id} 的明细暂不可用；请重试证据读取</>
                          ) : (
                            <>证据 {id} 未包含在当前快照中</>
                          )}
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p>此结论没有证据引用；不将其作为已验证效果。</p>
                  )}
                </Card>
              </li>
            ))}
          </ul>
        ) : (
          <p>本快照没有可追溯的结论；不推断效果变化。</p>
        )}
      </section>
      <section className="report-section" aria-label="证据明细">
        <h2>证据明细</h2>
        {evidenceLoading && <LoadingState label="正在读取证据明细" compact />}
        {evidenceError && (
          <ErrorState
            title="无法读取证据明细"
            detail={evidenceError.message}
            onRetry={onEvidenceRetry}
          />
        )}
        {!evidenceLoading && !evidenceError && evidence.length ? (
          <ul className="report-evidence-list">
            {evidence.map((item) => (
              <EvidenceItem
                key={item.evidence_id}
                item={item}
                timezone={snapshot.report_timezone}
              />
            ))}
          </ul>
        ) : !evidenceLoading && !evidenceError ? (
          <p>当前快照未包含原始证据引用。</p>
        ) : null}
      </section>
      <MessageBar intent="info">
        <MessageBarBody>
          此 CSV 仅导出快照已有的覆盖、结论与证据。资产明细、费用、下一轮动作及
          PDF 尚未由此接口提供；页面不会生成替代数据或将本快照称作完整商业报告。
        </MessageBarBody>
      </MessageBar>
    </div>
  );
}

function ReportListPage({
  tenantId,
  projectId,
}: {
  tenantId: string;
  projectId: string;
}) {
  const query = useReportsQuery(tenantId, projectId);
  if (query.isPending) return <LoadingState label="正在读取周报快照" />;
  if (query.isError) {
    return (
      <ErrorState
        title={
          query.error instanceof ApiError && query.error.status === 403
            ? "权限不足"
            : "无法加载报告"
        }
        detail={query.error.message}
        onRetry={() => void query.refetch()}
      />
    );
  }
  return (
    <div className="workbench-page reports-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">P14 · 效果报告</p>
          <h1>每周报告</h1>
          <p>按生成时的输入与数据截止封存；迟到证据需显式更正版本。</p>
        </div>
        <Button
          icon={<ArrowSyncRegular />}
          disabled={query.isFetching}
          onClick={() => void query.refetch()}
        >
          刷新列表
        </Button>
      </section>
      {query.data.items.length === 0 ? (
        <EmptyState
          title="尚无周报快照"
          detail="截至目前没有已生成的报告。周期截止或输入到齐后，系统才会保存固定快照；这里不会用演示数据填充。"
        />
      ) : (
        <ul className="report-list">
          {query.data.items.map((snapshot) => (
            <li key={snapshot.report_id}>
              <Card className="report-panel">
                <div className="report-list-heading">
                  <h2>
                    <Link to={encodeURIComponent(snapshot.report_id)}>
                      {dateTime(
                        snapshot.report_window_start_at,
                        snapshot.report_timezone,
                      )}{" "}
                      —{" "}
                      {dateTime(
                        snapshot.report_window_end_at,
                        snapshot.report_timezone,
                      )}
                    </Link>
                  </h2>
                  <Badge
                    appearance="tint"
                    color={
                      snapshot.status === "partial" ? "warning" : "success"
                    }
                  >
                    {snapshot.status === "partial" ? "部分覆盖" : "完整覆盖"}
                  </Badge>
                </div>
                <p>
                  周期 {snapshot.cycle_id} · 修订 {snapshot.revision} · 截止{" "}
                  {dateTime(snapshot.cutoff_at, snapshot.report_timezone)}
                </p>
                {snapshot.correction_of && (
                  <p>显式更正版本，原报告 {snapshot.correction_of} 仍保留。</p>
                )}
              </Card>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ReportDetailPage({
  tenantId,
  projectId,
  reportId,
}: {
  tenantId: string;
  projectId: string;
  reportId: string;
}) {
  const query = useReportQuery(tenantId, projectId, reportId);
  const evidence = useReportEvidenceQuery(tenantId, projectId, reportId);
  if (query.isPending) return <LoadingState label="正在读取封存的报告版本" />;
  if (query.isError) {
    return (
      <ErrorState
        title={
          query.error instanceof ApiError && query.error.status === 403
            ? "权限不足"
            : query.error instanceof ApiError && query.error.status === 404
              ? "未找到报告"
              : "无法加载报告"
        }
        detail={query.error.message}
        onRetry={() => void query.refetch()}
      />
    );
  }
  if (query.data.project_id !== projectId) {
    return (
      <ErrorState
        title="报告不属于当前项目"
        detail="请从当前项目的报告列表打开。"
      />
    );
  }
  return (
    <SnapshotDetail
      snapshot={query.data}
      evidence={evidence.data?.items ?? []}
      evidenceError={evidence.isError ? evidence.error : null}
      evidenceLoading={evidence.isPending}
      onEvidenceRetry={() => void evidence.refetch()}
      refreshing={query.isFetching}
      onRefresh={() => void query.refetch()}
    />
  );
}

export function ReportsPage() {
  const { tenantId, projectId, id } = useParams();
  if (!tenantId || !projectId) {
    return <ErrorState title="缺少项目标识" />;
  }
  return id ? (
    <ReportDetailPage
      key={`${tenantId}/${projectId}/${id}`}
      tenantId={tenantId}
      projectId={projectId}
      reportId={id}
    />
  ) : (
    <ReportListPage
      key={`${tenantId}/${projectId}`}
      tenantId={tenantId}
      projectId={projectId}
    />
  );
}
