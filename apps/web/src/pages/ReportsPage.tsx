import { useLayoutEffect, useRef, useState } from "react";
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
  useCurrentReportCycleQuery,
  useReportEvidenceQuery,
  useReportPreviewQuery,
  useReportQuery,
  useReportsQuery,
  type ReportCoverage,
  type ReportEvidenceReference,
  type ReportProjection,
  type ReportSnapshot,
} from "../api/reports";
import { ApiError } from "../api/client";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import { downloadReportCsv } from "./reportsCsv";
import { ReportSupplementaryMeasurements } from "./ReportSupplementaryMeasurements";
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
  cancelled: "已取消",
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
        {" · "}计划总数{" "}
        {coverage.expected_count === null ? "未知" : coverage.expected_count}
        {" · "}已观察 {coverage.observed_count}
      </p>
      {!available && (
        <MessageBar intent="warning">
          <MessageBarBody>
            {coverage.reason || "本期计划尚未确定，暂无法统计完整覆盖情况。"}
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
        <p className="report-muted">暂无分类统计。</p>
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
  savedSnapshot,
  evidence,
  evidenceError,
  evidenceLoading,
  onEvidenceRetry,
  onRefresh,
  refreshing,
}: {
  snapshot: ReportProjection;
  savedSnapshot?: ReportSnapshot;
  evidence: ReportEvidenceReference[];
  evidenceError: Error | null;
  evidenceLoading: boolean;
  onEvidenceRetry: () => void;
  onRefresh: () => void;
  refreshing: boolean;
}) {
  const evidenceIds = new Set(evidence.map((item) => item.evidence_id));
  const Container = savedSnapshot ? "div" : "section";
  const [pdfExporting, setPdfExporting] = useState(false);
  const [pdfError, setPdfError] = useState(false);
  const pdfController = useRef<AbortController | null>(null);

  useLayoutEffect(() => {
    return () => {
      pdfController.current?.abort();
      pdfController.current = null;
    };
  }, [savedSnapshot?.report_id, savedSnapshot?.input_hash]);

  async function exportPdf() {
    if (!savedSnapshot || pdfController.current) return;
    const controller = new AbortController();
    pdfController.current = controller;
    setPdfError(false);
    setPdfExporting(true);
    try {
      const { downloadReportPdf } = await import("./reportsPdf");
      if (controller.signal.aborted) return;
      await downloadReportPdf(savedSnapshot, { signal: controller.signal });
    } catch {
      if (!controller.signal.aborted) setPdfError(true);
    } finally {
      if (pdfController.current === controller) {
        pdfController.current = null;
        if (!controller.signal.aborted) setPdfExporting(false);
      }
    }
  }

  return (
    <Container
      className={
        savedSnapshot
          ? "workbench-page reports-page"
          : "reports-page report-preview"
      }
      aria-label={savedSnapshot ? undefined : "临时报告预览"}
    >
      {savedSnapshot ? (
        <>
          <section className="page-hero">
            <div>
              <p className="eyebrow">正式周报</p>
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
                {dateTime(
                  snapshot.report_window_end_at,
                  snapshot.report_timezone,
                )}
                {" · "}时区 {snapshot.report_timezone}
              </p>
            </div>
            <div className="report-actions">
              <Button onClick={() => downloadReportCsv(savedSnapshot)}>
                下载覆盖与证据 CSV
              </Button>
              <Button disabled={pdfExporting} onClick={() => void exportPdf()}>
                {pdfExporting ? "正在生成 PDF…" : "下载覆盖与证据 PDF"}
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
          {pdfError && (
            <MessageBar intent="error">
              <MessageBarBody>
                PDF 生成失败，未下载文件。可重试下载。
              </MessageBarBody>
            </MessageBar>
          )}
          <Link to="../reports">← 返回报告列表</Link>
          <MessageBar
            intent={snapshot.status === "partial" ? "warning" : "info"}
          >
            <MessageBarBody>
              {snapshot.status === "partial" ? "部分覆盖" : "完整覆盖"}
              ：这是截止{" "}
              {dateTime(snapshot.cutoff_at, snapshot.report_timezone)}{" "}
              的报告，后续更新将另存为新版本。生成于{" "}
              {dateTime(snapshot.generated_at, snapshot.report_timezone)}。
            </MessageBarBody>
          </MessageBar>
        </>
      ) : (
        <>
          <div className="report-list-heading">
            <h2>当前周期报告预览</h2>
            <Button
              icon={<ArrowSyncRegular />}
              disabled={refreshing}
              onClick={onRefresh}
            >
              刷新预览
            </Button>
          </div>
          <MessageBar intent="warning">
            <MessageBarBody>
              临时预览 · 未保存为正式周报（
              {snapshot.status === "partial" ? "部分覆盖" : "完整覆盖"}
              ）。按本期计划与已收集的数据统计，刷新后可能变化。
            </MessageBarBody>
          </MessageBar>
          <dl className="report-metadata">
            <dt>生成时间</dt>
            <dd>{dateTime(snapshot.generated_at, snapshot.report_timezone)}</dd>
            <dt>数据更新至</dt>
            <dd>
              {dateTime(snapshot.evidence_as_of, snapshot.report_timezone)}
            </dd>
            <dt>本期截止时间</dt>
            <dd>{dateTime(snapshot.cutoff_at, snapshot.report_timezone)}</dd>
          </dl>
        </>
      )}
      <Card className="report-panel">
        <h2>{savedSnapshot ? "报告范围与版本" : "预览范围与版本"}</h2>
        <p>
          {dateTime(snapshot.report_window_start_at, snapshot.report_timezone)}{" "}
          — {dateTime(snapshot.report_window_end_at, snapshot.report_timezone)}
          {" · "}时区 {snapshot.report_timezone}
        </p>
        <dl className="report-metadata">
          {savedSnapshot && (
            <>
              <dt>报告 ID</dt>
              <dd>{savedSnapshot.report_id}</dd>
            </>
          )}
          <dt>周期 ID</dt>
          <dd>{snapshot.cycle_id}</dd>
          {savedSnapshot && (
            <>
              <dt>修订</dt>
              <dd>{savedSnapshot.revision}</dd>
              <dt>更正前版本</dt>
              <dd>{savedSnapshot.correction_of ?? "无"}</dd>
            </>
          )}
          {savedSnapshot && (
            <>
              <dt>数据更新至</dt>
              <dd>
                {dateTime(snapshot.evidence_as_of, snapshot.report_timezone)}
              </dd>
            </>
          )}
        </dl>
        <h3>本期计划版本</h3>
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
          <p>缺少计划版本信息，覆盖范围待确认。</p>
        )}
      </Card>
      <section className="report-coverage" aria-label="三类独立覆盖">
        <CoveragePanel
          title="文档覆盖与资料缺口"
          coverage={snapshot.documents}
          note="显示本期计划内容及未完成原因，包含受阻和延后项。"
        />
        <CoveragePanel
          title="发布目标覆盖"
          coverage={snapshot.publications}
          note="分别查看发布结果、公开验证和查回记录；发现公开资产后，原发送结果仍可能待核对。"
        />
        <CoveragePanel
          title="AI 渠道测量覆盖"
          coverage={snapshot.measurements}
          note="有效回答、缺测和拒答分别统计；缺测不计为未提及。"
        />
      </section>
      <section className="report-section" aria-label="平台与测量比较组">
        <h2>分平台与测量口径</h2>
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
              <p>暂无分平台发布数据。</p>
            )}
          </Card>
          <Card className="report-panel">
            <h3>AI 测量比较口径</h3>
            {snapshot.measurement_groups.length ? (
              snapshot.measurement_groups.map((group) => (
                <div key={group.comparison_key} className="report-group">
                  <h4>{group.comparison_key}</h4>
                  <p>
                    问题用途：
                    {group.purpose === "optimization"
                      ? "优化问题"
                      : group.purpose === "frozen_evaluation"
                        ? "冻结评估（不进入优化）"
                        : "旧数据未分类（不进入优化）"}
                  </p>
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
              <p>暂无 AI 渠道测量数据。</p>
            )}
          </Card>
        </div>
        <p>
          测量结果按问题集、平台、观测面、市场和采样协议分别展示。历史样本保留原测量时间。
        </p>
      </section>
      {!!snapshot.supplementary_measurements?.length && (
        <ReportSupplementaryMeasurements
          items={snapshot.supplementary_measurements}
          timezone={snapshot.report_timezone}
        />
      )}
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
                              {savedSnapshot ? "查看快照证据" : "查看预览证据"}{" "}
                              {id}
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
                    <p>此结论暂无来源支持，效果待验证。</p>
                  )}
                </Card>
              </li>
            ))}
          </ul>
        ) : (
          <p>
            本{savedSnapshot ? "快照" : "预览"}
            暂无可用结论。
          </p>
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
          <p>当前{savedSnapshot ? "快照" : "预览"}未包含证据引用。</p>
        ) : null}
      </section>
      {savedSnapshot && (
        <MessageBar intent="info">
          <MessageBarBody>
            CSV 和 PDF
            包含本版报告的覆盖统计、结论与证据，暂不包含费用、资产明细和下一轮计划。
          </MessageBarBody>
        </MessageBar>
      )}
    </Container>
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
  const cycle = useCurrentReportCycleQuery(tenantId, projectId);
  const preview = useReportPreviewQuery(
    tenantId,
    projectId,
    cycle.data?.cycle_id,
  );
  return (
    <div className="workbench-page reports-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">效果报告</p>
          <h1>每周报告</h1>
          <p>查看本期进展预览与历史周报，下载覆盖统计和来源证据。</p>
        </div>
        <Button
          icon={<ArrowSyncRegular />}
          disabled={query.isFetching}
          onClick={() => void query.refetch()}
        >
          刷新列表
        </Button>
      </section>
      {cycle.isPending ? (
        <LoadingState label="正在读取当前周期" compact />
      ) : cycle.isError ? (
        <ErrorState
          title={
            cycle.error instanceof ApiError && cycle.error.status === 403
              ? "权限不足"
              : "无法读取当前周期"
          }
          detail={cycle.error.message}
          onRetry={() => void cycle.refetch()}
        />
      ) : !cycle.data ? (
        <EmptyState
          title="暂无活动周期"
          detail="项目尚无可供预览的当前周期；正式周报快照仍可在下方查看。"
          action={
            <Button
              icon={<ArrowSyncRegular />}
              onClick={() => void cycle.refetch()}
            >
              检查当前周期
            </Button>
          }
        />
      ) : preview.isPending ? (
        <LoadingState label="正在生成临时报告预览" compact />
      ) : preview.isError ? (
        <ErrorState
          title={
            preview.error instanceof ApiError && preview.error.status === 403
              ? "权限不足"
              : "无法加载临时预览"
          }
          detail={preview.error.message}
          onRetry={() => void preview.refetch()}
        />
      ) : preview.data.project_id !== projectId ||
        preview.data.cycle_id !== cycle.data.cycle_id ||
        preview.data.kind !== "preview" ? (
        <ErrorState
          title="预览不属于当前周期"
          detail="请刷新当前周期后重试。"
          onRetry={() => void cycle.refetch()}
        />
      ) : (
        <SnapshotDetail
          snapshot={preview.data}
          evidence={preview.data.evidence}
          evidenceError={null}
          evidenceLoading={false}
          onEvidenceRetry={() => {}}
          refreshing={preview.isFetching}
          onRefresh={() => void preview.refetch()}
        />
      )}
      <section aria-label="已保存的正式周报">
        <h2>已保存的正式周报</h2>
        {query.isPending ? (
          <LoadingState label="正在读取周报快照" compact />
        ) : query.isError ? (
          <ErrorState
            title={
              query.error instanceof ApiError && query.error.status === 403
                ? "权限不足"
                : "无法加载报告"
            }
            detail={query.error.message}
            onRetry={() => void query.refetch()}
          />
        ) : query.data.items.length === 0 ? (
          <EmptyState
            title="尚无周报快照"
            detail="周期截止或数据收集完成后，将生成正式周报。你可以先查看上方预览。"
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
                    <p>更正版本，原报告 {snapshot.correction_of} 仍可查看。</p>
                  )}
                </Card>
              </li>
            ))}
          </ul>
        )}
      </section>
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
      savedSnapshot={query.data}
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
