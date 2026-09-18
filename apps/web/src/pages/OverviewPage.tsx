import { useQuery } from "@tanstack/react-query";
import {
  Button,
  Card,
  CardHeader,
  DataGrid,
  DataGridBody,
  DataGridCell,
  DataGridHeader,
  DataGridHeaderCell,
  DataGridRow,
  MessageBar,
  MessageBarBody,
  Spinner,
  TableCellLayout,
  createTableColumn,
  type TableColumnDefinition,
} from "@fluentui/react-components";
import {
  ArrowTrendingRegular,
  PlayRegular,
  ArrowSyncRegular,
} from "@fluentui/react-icons";
import { useParams } from "react-router-dom";
import { getOverviewDemo, type OverviewData } from "../data/demo";
import { LoopProgress } from "../components/LoopProgress";
import { StatusPill } from "../components/StatusPill";

type Run = OverviewData["runs"][number];

const runColumns: TableColumnDefinition<Run>[] = [
  createTableColumn<Run>({
    columnId: "name",
    renderHeaderCell: () => "最近运行",
    renderCell: (run) => (
      <TableCellLayout description={run.detail}>{run.name}</TableCellLayout>
    ),
  }),
  createTableColumn<Run>({
    columnId: "status",
    renderHeaderCell: () => "状态",
    renderCell: (run) => <StatusPill status={run.status} />,
  }),
  createTableColumn<Run>({
    columnId: "at",
    renderHeaderCell: () => "时间",
    renderCell: (run) => run.at,
  }),
];

export function OverviewPage() {
  const { tenantId, projectId } = useParams();
  const { data, isPending, isError, refetch, isFetching } = useQuery({
    queryKey: ["overview", tenantId, projectId],
    queryFn: getOverviewDemo,
  });
  if (isPending)
    return (
      <div className="page-loading">
        <Spinner label="正在加载项目总览" />
      </div>
    );
  if (isError || !data)
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
  const base = `/app/${tenantId}/${projectId}`;
  return (
    <div className="overview-page">
      <section className="page-hero overview-hero">
        <div>
          <p className="eyebrow">P02 · 项目总览</p>
          <h1>{data.project.name}</h1>
          <p>
            {data.project.market}　·　观察期 {data.project.period}
          </p>
        </div>
        <div className="hero-actions">
          <div className="budget">
            <span>预算余额</span>
            <strong>{data.project.budgetLeft}</strong>
            <small>{data.project.budgetDetail}</small>
          </div>
          <Button appearance="primary" icon={<PlayRegular />}>
            启动自动优化
          </Button>
        </div>
      </section>

      <MessageBar intent="warning" className="persistent-notice">
        <MessageBarBody>
          <b>1 项内容已阻断：</b>
          定价事实存在冲突，系统已停止其后续外部请求；其他已排期任务不受影响。
        </MessageBarBody>
        <Button as="a" href={`${base}/content`} appearance="subtle">
          查看内容
        </Button>
      </MessageBar>

      <LoopProgress steps={data.loop} />

      <section className="metric-grid" aria-label="项目关键指标">
        {data.metrics.map((metric) => (
          <Card
            key={metric.label}
            className={`metric-card tone-${metric.tone ?? "neutral"}`}
          >
            <span>{metric.label}</span>
            <strong>{metric.value}</strong>
            <small>{metric.detail}</small>
          </Card>
        ))}
      </section>

      <div className="overview-columns">
        <section className="column-main" aria-label="趋势与最近运行">
          <Card className="panel-card">
            <CardHeader
              header={
                <div>
                  <h2>分平台趋势与观测面</h2>
                  <p>指标按观测面隔离，不混合计算。</p>
                </div>
              }
              action={
                <Button
                  as="a"
                  href={`${base}/measurement`}
                  appearance="subtle"
                  icon={<ArrowTrendingRegular />}
                >
                  查看测量
                </Button>
              }
            />
            <div className="trend-list">
              {data.trends.map((trend) => (
                <TrendRow key={trend.platform} {...trend} />
              ))}
            </div>
          </Card>
          <Card className="panel-card runs-card">
            <CardHeader
              header={
                <div>
                  <h2>最近运行、阻断与待确认</h2>
                  <p>外部响应中断时先对账，避免重复发布。</p>
                </div>
              }
              action={
                <Button
                  as="a"
                  href={`${base}/publications`}
                  appearance="subtle"
                >
                  查看全部
                </Button>
              }
            />
            <DataGrid
              items={data.runs}
              columns={runColumns}
              getRowId={(run) => run.id}
              size="small"
              className="runs-table"
            >
              <DataGridHeader>
                <DataGridRow>
                  {({ renderHeaderCell }) => (
                    <DataGridHeaderCell>
                      {renderHeaderCell()}
                    </DataGridHeaderCell>
                  )}
                </DataGridRow>
              </DataGridHeader>
              <DataGridBody<Run>>
                {({ item, rowId }) => (
                  <DataGridRow<Run> key={rowId}>
                    {({ renderCell }) => (
                      <DataGridCell>{renderCell(item)}</DataGridCell>
                    )}
                  </DataGridRow>
                )}
              </DataGridBody>
            </DataGrid>
          </Card>
        </section>
        <aside className="column-side" aria-label="当前机会与资源状态">
          <Card className="panel-card">
            <CardHeader
              header={
                <div>
                  <h2>当前机会与建议动作</h2>
                  <p>按业务权重、差距、可行动性与成本排序。</p>
                </div>
              }
            />
            <div className="opportunity-list">
              {data.opportunities.map((item) => (
                <div className="opportunity" key={item.title}>
                  <div>
                    <StatusPill
                      status={
                        item.priority === "high"
                          ? "blocked"
                          : item.priority === "medium"
                            ? "active"
                            : "queued"
                      }
                      text={
                        item.priority === "high"
                          ? "优先处理"
                          : item.priority === "medium"
                            ? "处理中"
                            : "待排期"
                      }
                    />
                    <h3>{item.title}</h3>
                    <p>{item.detail}</p>
                  </div>
                  <Button
                    as="a"
                    href={`${base}/campaigns`}
                    appearance="secondary"
                  >
                    {item.action}
                  </Button>
                </div>
              ))}
            </div>
          </Card>
          <Card className="panel-card root-cause-card">
            <CardHeader
              header={
                <div>
                  <h2>根因聚合、预算与资源</h2>
                  <p>同一根因聚合显示，避免逐条告警。</p>
                </div>
              }
              action={
                <Button
                  appearance="subtle"
                  icon={<ArrowSyncRegular />}
                  disabled={isFetching}
                >
                  刷新
                </Button>
              }
            />
            <div className="cause-list">
              {data.causes.map((cause) => (
                <div className="cause" key={cause.label}>
                  <span className="cause-count">{cause.count}</span>
                  <div>
                    <b>{cause.label}</b>
                    <small>{cause.detail}</small>
                  </div>
                </div>
              ))}
            </div>
            <div className="resource-summary">
              <span>资源池健康</span>
              <StatusPill status="complete" text="全部可用" />
            </div>
          </Card>
        </aside>
      </div>
    </div>
  );
}

function TrendRow({
  platform,
  surface,
  rate,
  change,
  sample,
  points,
}: OverviewData["trends"][number]) {
  return (
    <div className="trend-row">
      <div className="trend-label">
        <b>{platform}</b>
        <span>{surface}</span>
      </div>
      <Sparkline points={points} />
      <div className="trend-rate">
        <strong>{rate}</strong>
        <span className={change.startsWith("+") ? "positive" : ""}>
          {change}
        </span>
        <small>{sample}</small>
      </div>
    </div>
  );
}

function Sparkline({ points }: { points: number[] }) {
  if (points.length === 0)
    return <div className="sparkline no-data">正在建立基线</div>;
  const width = 150;
  const height = 42;
  const max = Math.max(...points);
  const min = Math.min(...points);
  const range = max - min || 1;
  const d = points
    .map(
      (point, index) =>
        `${index === 0 ? "M" : "L"} ${index * (width / (points.length - 1))} ${height - ((point - min) / range) * 29 - 6}`,
    )
    .join(" ");
  return (
    <svg
      className="sparkline"
      viewBox={`0 0 ${width} ${height}`}
      role="img"
      aria-label="趋势上升"
    >
      <path
        d={d}
        fill="none"
        stroke="currentColor"
        strokeWidth="2.5"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}
