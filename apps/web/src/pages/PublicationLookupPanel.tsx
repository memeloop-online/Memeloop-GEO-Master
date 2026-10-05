import { Button } from "@fluentui/react-components";
import { useInfiniteQuery } from "@tanstack/react-query";
import {
  getPublicationLookup,
  type PublicationLookupPage,
} from "../api/channelJobs";
import { useAuth } from "../auth/AuthProvider";
import { ErrorState } from "../components/AsyncState";

export function safePublicUrl(value: string | null): string | null {
  if (!value || value.length > 256) return null;
  try {
    const url = new URL(value);
    if (
      url.protocol !== "https:" ||
      !["www.zhihu.com", "zhuanlan.zhihu.com"].includes(url.hostname) ||
      url.port ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      !/^\/p\/[0-9]+$/.test(url.pathname) ||
      url.href !== value
    )
      return null;
    return value;
  } catch {
    return null;
  }
}

export function safeOriginalPublicUrl(value: string | null): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    return (url.protocol === "https:" || url.protocol === "http:") &&
      !url.username &&
      !url.password
      ? url.href
      : null;
  } catch {
    return null;
  }
}

const lookupErrors: Record<string, string> = {
  candidate_missing: "没有可核对的公开链接",
  candidate_invalid: "公开链接不符合渠道规则",
  target_mismatch: "原发布目标无法匹配",
  connector_version_missing: "缺少原连接器版本",
  connector_version_invalid: "原连接器版本不可用",
  binding_missing: "缺少原账号与出口绑定",
  binding_unavailable: "原账号与出口绑定暂不可用",
  runner_unavailable: "查回服务暂不可用",
  account_busy: "账号正忙，稍后自动查回",
  account_reservation_failed: "账号暂不可用于查回",
  account_or_network_unavailable: "原账号或指定出口暂不可用",
  connector_version_mismatch: "连接器版本与原发送不一致",
  lookup_preflight_expired: "查回准备已过期，等待自动重试",
  readback_unverified: "公开资产尚未核实",
  lookup_unavailable: "公开读回暂不可用",
};
const lookupError = (code: string) => lookupErrors[code] ?? "查回暂未确认";

const dateTime = (value: string | null) =>
  value ? new Date(value).toLocaleString("zh-CN") : "未安排";
const errorText = (value: unknown) =>
  value instanceof Error ? value.message : "读取失败，请重试。";

/** Only mount this for a publication whose original attempt is unknown or has no receipt. */
export function PublicationLookupPanel({
  tenantId,
  projectId,
  targetId,
}: {
  tenantId: string;
  projectId: string;
  targetId: string;
}) {
  const { session } = useAuth();
  const lookup = useInfiniteQuery({
    queryKey: [
      "publication-lookup",
      session?.user.id,
      session?.operator.id,
      tenantId,
      projectId,
      targetId,
    ],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam, signal }) =>
      getPublicationLookup(tenantId, projectId, targetId, pageParam, signal),
    getNextPageParam: (last) => last.next_before ?? undefined,
    retry: false,
  });
  const first = lookup.data?.pages[0];
  const observationsByExecution = new Map<
    string,
    PublicationLookupPage["observations"][number]
  >();
  for (const item of lookup.data?.pages.flatMap((page) => page.observations) ??
    []) {
    if (!observationsByExecution.has(item.execution_id))
      observationsByExecution.set(item.execution_id, item);
  }
  const observations = Array.from(observationsByExecution.values());

  return (
    <section className="publication-lookup" aria-label="公开资产查回观察">
      <div className="channel-job-target-heading">
        <h4>公开资产查回观察</h4>
        <Button
          appearance="subtle"
          disabled={lookup.isFetching}
          onClick={() => void lookup.refetch()}
        >
          刷新查回
        </Button>
      </div>
      <p>
        这里只记录只读观察；即使发现资产，也不能证明原发送成功。原发布结果仍为未知，系统不会因此重发。
      </p>
      {lookup.isPending && <p role="status">正在读取查回记录…</p>}
      {lookup.isError && !first && (
        <ErrorState
          title="查回记录无法读取"
          detail={errorText(lookup.error)}
          onRetry={() => void lookup.refetch()}
        />
      )}
      {first && (
        <>
          {!first.job ? (
            <p role="status">尚未安排自动查回；原发送结果仍待核对。</p>
          ) : (
            <p role="status">
              已查 {first.job.query_count} 次
              {first.job.in_progress
                ? " · 正在查回"
                : ` · 下次查回 ${dateTime(first.job.next_due_at)}`}
              {first.job.last_error_code &&
                ` · 最近情况：${lookupError(first.job.last_error_code)}`}
            </p>
          )}
          {observations.length === 0 ? (
            <p>暂无公开资产观察记录。</p>
          ) : (
            <ul className="publication-lookup-list">
              {observations.map((item) => {
                const url = safePublicUrl(item.public_url);
                return (
                  <li key={item.execution_id}>
                    {item.finding === "asset_observed"
                      ? "观察到公开资产（非原发送成功凭据）"
                      : "本次未能确认资产"}
                    {" · "}观察 {dateTime(item.observed_at)} · 收到{" "}
                    {dateTime(item.received_at)}
                    {item.error_code && ` · ${lookupError(item.error_code)}`}
                    {url && (
                      <>
                        {" · "}
                        <a href={url} target="_blank" rel="noopener noreferrer">
                          查看公开资产
                        </a>
                      </>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
          {lookup.isFetchNextPageError && (
            <ErrorState
              title="更早的记录无法读取"
              detail={errorText(lookup.error)}
              onRetry={() => void lookup.fetchNextPage()}
            />
          )}
          {lookup.isRefetchError && (
            <ErrorState
              title="更新查回记录失败，仍显示已读取记录"
              detail={errorText(lookup.error)}
              onRetry={() => void lookup.refetch()}
            />
          )}
          {lookup.hasNextPage && (
            <Button
              disabled={lookup.isFetchingNextPage}
              onClick={() => void lookup.fetchNextPage()}
            >
              {lookup.isFetchingNextPage ? "正在加载…" : "加载更早记录"}
            </Button>
          )}
          {lookup.isRefetching && <p role="status">正在更新查回记录…</p>}
        </>
      )}
    </section>
  );
}
