import {
  Badge,
  Button,
  Card,
  CardHeader,
  MessageBar,
  MessageBarBody,
  Spinner,
} from "@fluentui/react-components";
import { ArrowSyncRegular } from "@fluentui/react-icons";
import { Link, useParams } from "react-router-dom";
import { ApiError } from "../api/client";
import {
  type DocumentManifestItem,
  useDocumentManifestQuery,
  usePlanDocumentManifestMutation,
} from "../api/documentManifests";
import { useKnowledgeReleaseQuery } from "../api/knowledge";
import { useProjectStartQuery } from "../api/projects";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant } from "../auth/types";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";

const branchLabels: Record<DocumentManifestItem["state"], string> = {
  planned: "已规划，待生成正文",
  blocked: "阻断",
  deferred: "延后",
  not_applicable: "不适用",
};

function PlanContent({
  tenantId,
  projectId,
  cycleId,
}: {
  tenantId: string;
  projectId: string;
  cycleId: string;
}) {
  const { session } = useAuth();
  const membership = membershipForTenant(session, tenantId);
  const mayPlan =
    membership?.role === "tenant_admin" || membership?.role === "member";
  const start = useProjectStartQuery(tenantId, projectId);
  const release = useKnowledgeReleaseQuery(tenantId, projectId);
  const manifest = useDocumentManifestQuery(
    tenantId,
    projectId,
    start.data?.document_manifest.manifest_id,
  );
  const plan = usePlanDocumentManifestMutation(tenantId, projectId);

  if (start.isPending) {
    return <LoadingState label="正在加载项目启动信息" />;
  }
  if (start.isError) {
    return (
      <ErrorState
        title="无法加载项目启动信息"
        detail={start.error.message}
        onRetry={() => void start.refetch()}
      />
    );
  }
  if (!start.data) {
    return (
      <EmptyState
        title="项目尚未启动"
        detail="启动项目后会创建本轮文档清单句柄；当前没有可规划的清单。"
      />
    );
  }
  if (cycleId !== "current" && cycleId !== start.data.cycle_id) {
    return (
      <ErrorState
        title="本轮计划不匹配"
        detail="此页面只支持当前项目已启动的周期；请从导航进入当前计划。"
      />
    );
  }

  const manifestHandle = start.data.document_manifest;
  const currentRelease = release.data;
  const result =
    manifest.data ??
    (plan.data?.manifest_id === manifestHandle.manifest_id
      ? plan.data
      : undefined);
  const sealed = result?.sealed || manifestHandle.sealed;
  const canSubmit =
    mayPlan &&
    Boolean(currentRelease) &&
    !manifest.isPending &&
    !manifest.isError &&
    !plan.isPending &&
    !sealed;
  return (
    <div className="workbench-page document-manifest-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">P07 · 当前计划</p>
          <h1>文档覆盖清单</h1>
          <p>
            本轮 {start.data.cycle_id} · 清单 {manifestHandle.manifest_id} ·
            知识版本{" "}
            {result?.knowledge_release_id ??
              currentRelease?.knowledge_release_id ??
              "尚未形成"}
          </p>
        </div>
        {sealed ? (
          <Button
            icon={<ArrowSyncRegular />}
            disabled={manifest.isFetching}
            onClick={() => void manifest.refetch()}
          >
            刷新文档清单
          </Button>
        ) : (
          <Button
            appearance="primary"
            disabled={!canSubmit}
            onClick={() => {
              if (!currentRelease) return;
              plan.mutate({
                manifest_id: manifestHandle.manifest_id,
                knowledge_release_id: currentRelease.knowledge_release_id,
              });
            }}
          >
            {plan.isPending ? "正在规划清单…" : "规划文档清单"}
          </Button>
        )}
      </section>

      {!mayPlan && (
        <MessageBar intent="warning">
          <MessageBarBody>
            当前成员仅可查看；规划清单需要项目编辑权限。
          </MessageBarBody>
        </MessageBar>
      )}
      {manifest.isPending && <Spinner label="正在读取持久文档清单" />}
      {manifest.isError && (
        <ErrorState
          title="无法读取文档清单"
          detail={manifest.error.message}
          onRetry={() => void manifest.refetch()}
        />
      )}
      {release.isError && !result && (
        <ErrorState
          title="无法加载知识版本"
          detail={release.error.message}
          onRetry={() => void release.refetch()}
        />
      )}
      {!release.isPending && !release.isError && !currentRelease && !result && (
        <EmptyState
          title="尚无可用知识版本"
          detail="请先导入并处理公开资料。知识版本形成后，才能规划此轮文档分支。"
        />
      )}
      {currentRelease &&
        !result &&
        !manifest.isPending &&
        !manifest.isError &&
        !plan.isPending &&
        !plan.isError && (
          <MessageBar intent="info">
            <MessageBarBody>
              启动时创建了清单句柄（{manifestHandle.state}，修订
              {manifestHandle.revision}）。点击规划才会提交当前知识版本；
              此处不会生成正文。
            </MessageBarBody>
          </MessageBar>
        )}
      {plan.isPending && <Spinner label="正在规划文档清单" />}
      {plan.isError && (
        <ErrorState
          title={
            plan.error instanceof ApiError && plan.error.status === 409
              ? "规划输入冲突"
              : plan.error instanceof ApiError && plan.error.status === 403
                ? "权限不足"
                : "文档清单规划失败"
          }
          detail={
            <>
              {plan.error.message}
              {plan.error instanceof ApiError && plan.error.status === 409
                ? "。请核对知识版本及启动后的项目配置；已有清单不会被覆盖。"
                : ""}
            </>
          }
          onRetry={
            canSubmit && currentRelease
              ? () => {
                  plan.mutate({
                    manifest_id: manifestHandle.manifest_id,
                    knowledge_release_id: currentRelease.knowledge_release_id,
                  });
                }
              : undefined
          }
        />
      )}
      {result && (
        <>
          <MessageBar intent="success">
            <MessageBarBody>
              清单修订 {result.revision} · {result.sealed ? "已封存" : "未封存"}{" "}
              · 规划器 {result.planner_version}
              。这是冻结的分支规划；正文执行状态在内容资产页单独记录。
            </MessageBarBody>
          </MessageBar>
          <p>
            <Link
              to={`/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/content?cycle_id=${encodeURIComponent(start.data.cycle_id)}`}
            >
              查看本轮内容资产与执行
            </Link>
          </p>
          <Card className="panel-card">
            <CardHeader
              header={<h2>覆盖账目</h2>}
              description={`计划总数 ${result.expected_count ?? "未知"} · 实际条目 ${result.coverage.total}`}
            />
            <p>
              待生成 {result.coverage.planned} · 阻断 {result.coverage.blocked}{" "}
              · 延后 {result.coverage.deferred} · 不适用{" "}
              {result.coverage.not_applicable}
            </p>
            <p>
              知识版本 {result.knowledge_release_id} · 范围指纹{" "}
              {result.scope_hash}
            </p>
          </Card>
          <section aria-label="文档分支" className="manifest-items">
            <h2>内容生产 · 全部分支</h2>
            {result.items.length === 0 ? (
              <p>清单没有文档分支。请核对项目的文档范围配置。</p>
            ) : (
              <ul>
                {result.items.map((item) => (
                  <li key={item.document_manifest_item_id}>
                    <Card className="panel-card">
                      <div className="manifest-item-heading">
                        <h3>{item.content_type}</h3>
                        <Badge
                          appearance="tint"
                          color={
                            item.state === "blocked" ? "danger" : "informative"
                          }
                        >
                          {branchLabels[item.state]}
                        </Badge>
                      </div>
                      <p>
                        {item.market} · {item.language} · 产品{" "}
                        {item.product_id ?? "项目级"}
                      </p>
                      <dl>
                        <dt>分支 ID</dt>
                        <dd>{item.document_manifest_item_id}</dd>
                        <dt>文档键</dt>
                        <dd>{item.document_key}</dd>
                        <dt>依赖指纹</dt>
                        <dd>{item.dependency_hash}</dd>
                        <dt>来源版本 ID</dt>
                        <dd>
                          {item.source_version_refs.length
                            ? item.source_version_refs.join("、")
                            : "无公开来源版本"}
                        </dd>
                        {item.block_reason && (
                          <>
                            <dt>未完成原因</dt>
                            <dd>{item.block_reason}</dd>
                          </>
                        )}
                      </dl>
                      <p>
                        此规划项不包含内容版本或发布结果；请到内容资产查看执行与正文。
                      </p>
                    </Card>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </>
      )}
    </div>
  );
}

export function DocumentManifestPage() {
  const { tenantId, projectId, id } = useParams();
  if (!tenantId || !projectId || !id) {
    return <ErrorState title="缺少项目或计划标识" />;
  }
  return (
    <PlanContent
      key={`${tenantId}/${projectId}/${id}`}
      tenantId={tenantId}
      projectId={projectId}
      cycleId={id}
    />
  );
}
