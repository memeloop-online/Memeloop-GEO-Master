import { useState } from "react";
import {
  Badge,
  Button,
  Card,
  MessageBar,
  MessageBarBody,
  Textarea,
} from "@fluentui/react-components";
import {
  ArrowRightRegular,
  DocumentAddRegular,
  SearchRegular,
} from "@fluentui/react-icons";
import { Link, useParams } from "react-router-dom";
import {
  useAskKnowledgeMutation,
  useKnowledgeCapabilitiesQuery,
  useKnowledgeReleaseQuery,
} from "../api/knowledge";
import { ErrorState, EmptyState, LoadingState } from "../components/AsyncState";
import { KnowledgeCapabilitiesNotice } from "./KnowledgePage";
import { KnowledgeLocator } from "../components/KnowledgeLocator";

const examples = [
  "产品 A 和 B 有什么区别？",
  "哪些产品适合高温环境？",
  "当前有效价格是什么？",
  "哪些关键产品资料尚未提供？",
];

export function KnowledgeAskPage() {
  const { tenantId, projectId } = useParams();
  const [question, setQuestion] = useState("");
  const release = useKnowledgeReleaseQuery(tenantId, projectId);
  const capabilities = useKnowledgeCapabilitiesQuery(tenantId, projectId);
  const ask = useAskKnowledgeMutation(tenantId, projectId);
  const evidenceOnly =
    capabilities.data !== undefined && !capabilities.data.llm.available;
  const result = ask.data;

  function submit() {
    if (!question.trim() || !release.data?.knowledge_release_id) return;
    ask.mutate({
      query: question.trim(),
      knowledge_release_id: release.data.knowledge_release_id,
      purpose: "public",
      limit: 12,
    });
  }

  if (release.isPending && !release.data) {
    return <LoadingState label="正在确定当前知识版本" />;
  }
  if (release.isError) {
    return (
      <div className="knowledge-ask-page">
        <ErrorState
          title="无法读取当前知识版本"
          detail="问答必须绑定到一个明确的知识版本，因此当前不会发送无版本请求。"
          onRetry={() => void release.refetch()}
        />
      </div>
    );
  }
  if (!release.data) {
    return (
      <div className="knowledge-ask-page">
        <EmptyState
          title="还没有可问答的知识版本"
          detail="请先导入并完成至少一份资料的处理。系统不会用上传中的文件或空索引伪造答案。"
          action={
            <Link to="../knowledge">
              <Button appearance="primary">前往资料中心</Button>
            </Link>
          }
        />
      </div>
    );
  }

  return (
    <div className="knowledge-ask-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">P05 · 知识问答</p>
          <h1>向项目知识提问</h1>
          <p>
            当前绑定知识版本 #{release.data.sequence}。每个回答最多使用 12
            条可定位证据。
          </p>
        </div>
        <Button
          appearance="secondary"
          icon={<DocumentAddRegular />}
          disabled
          title="W05/W06 内容简报和编辑器尚未接入；系统不会假装已生成文章。"
        >
          生成文章
        </Button>
      </section>
      {capabilities.isError ? (
        <MessageBar intent="warning">
          <MessageBarBody>
            无法确认 LLM 能力。提交后服务端会如实返回可用模式或能力缺失。
          </MessageBarBody>
        </MessageBar>
      ) : (
        <KnowledgeCapabilitiesNotice capabilities={capabilities.data} />
      )}
      {evidenceOnly && (
        <MessageBar intent="info">
          <MessageBarBody>
            <b>证据摘录模式</b>
            <span>
              LLM
              问答未配置。系统仅返回检索到的证据与资料缺口，不会生成看似确定的回答。
            </span>
          </MessageBarBody>
        </MessageBar>
      )}
      <section className="knowledge-ask-layout">
        <Card className="ask-main-card">
          <label htmlFor="knowledge-question">你的问题</label>
          <Textarea
            id="knowledge-question"
            resize="vertical"
            value={question}
            onChange={(_, data) => setQuestion(data.value)}
            placeholder="例如：此产品在中国大陆的当前有效价格是多少？"
          />
          <div className="ask-actions">
            <Button
              appearance="primary"
              icon={<SearchRegular />}
              disabled={!question.trim() || ask.isPending}
              onClick={submit}
            >
              {ask.isPending ? "正在检索…" : evidenceOnly ? "查找证据" : "提问"}
            </Button>
            <small>只检索当前知识版本，不使用其他项目或未处理资料。</small>
          </div>
          {ask.isError && (
            <ErrorState
              title="本次问答未完成"
              detail={ask.error.message}
              onRetry={submit}
            />
          )}
          {!result && !ask.isPending && (
            <section className="ask-examples">
              <b>示例问题</b>
              <div>
                {examples.map((example) => (
                  <Button
                    key={example}
                    appearance="subtle"
                    onClick={() => setQuestion(example)}
                  >
                    {example}
                    <ArrowRightRegular />
                  </Button>
                ))}
              </div>
            </section>
          )}
          {result && (
            <section className="ask-result" aria-live="polite">
              <div className="ask-result-heading">
                <div>
                  <p className="eyebrow">
                    {result.mode === "evidence_only" ? "证据摘录模式" : "回答"}
                  </p>
                  <h2>
                    {result.status === "insufficient_evidence"
                      ? "当前资料没有这项信息"
                      : result.status === "conflicted"
                        ? "资料存在冲突，无法给出单一结论"
                        : result.mode === "evidence_only"
                          ? "已找到匹配证据"
                          : "基于当前资料的回答"}
                  </h2>
                </div>
                <Badge
                  appearance={result.status === "answered" ? "filled" : "tint"}
                  color={result.status === "answered" ? "success" : "warning"}
                >
                  {result.status === "answered"
                    ? "有证据"
                    : result.status === "conflicted"
                      ? "存在冲突"
                      : "证据不足"}
                </Badge>
              </div>
              {result.answer && result.status === "answered" && (
                <p className="ask-answer">
                  {result.mode === "evidence_only" && <b>证据摘录：</b>}
                  {result.answer}
                </p>
              )}
              {result.conflicts?.length ? (
                <MessageBar intent="warning">
                  <MessageBarBody>{result.conflicts.join("；")}</MessageBarBody>
                </MessageBar>
              ) : null}
              {result.capability_missing?.length ? (
                <MessageBar intent="warning">
                  <MessageBarBody>
                    本次问答缺少以下能力：{result.capability_missing.join("；")}
                    。结果不会被补成完整回答。
                  </MessageBarBody>
                </MessageBar>
              ) : null}
              {result.missing.length > 0 && (
                <section className="ask-missing">
                  <b>资料缺口</b>
                  <ul>
                    {result.missing.map((item) => (
                      <li key={item}>{item}</li>
                    ))}
                  </ul>
                </section>
              )}
            </section>
          )}
        </Card>
        <Card className="ask-evidence-card">
          <div className="knowledge-panel-heading">
            <div>
              <h2>来源证据</h2>
              <p>最多 12 条；点击可打开来源详情。</p>
            </div>
            {result && (
              <Badge appearance="tint">{result.evidence.length} / 12</Badge>
            )}
          </div>
          {!result ? (
            <p className="source-empty-inline">
              提交问题后，证据会显示在这里。
            </p>
          ) : result.evidence.length === 0 ? (
            <p className="source-empty-inline">
              当前没有足以支持这个问题的证据，系统不会补造引用。
            </p>
          ) : (
            <ol className="ask-evidence-list">
              {result.evidence.slice(0, 12).map((evidence, index) => (
                <li key={`${evidence.source_id}-${evidence.chunk_id ?? index}`}>
                  <Link to={`../knowledge/sources/${evidence.source_id}`}>
                    <b>
                      [{index + 1}]{" "}
                      {evidence.source_name ?? evidence.label ?? "打开来源资料"}
                    </b>
                    <KnowledgeLocator locator={evidence.locator} />
                    {(evidence.quote ?? evidence.text) && (
                      <small>“{evidence.quote ?? evidence.text}”</small>
                    )}
                  </Link>
                </li>
              ))}
            </ol>
          )}
        </Card>
      </section>
      <p className="knowledge-disabled-note">
        “生成文章”将在 W05/W06
        接入版本化内容简报与编辑器后可用；当前保持禁用，避免创建不存在的内容任务。
      </p>
    </div>
  );
}
