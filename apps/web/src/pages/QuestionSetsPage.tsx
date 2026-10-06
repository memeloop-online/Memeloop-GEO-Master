import { useEffect, useState } from "react";
import {
  Badge,
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Textarea,
} from "@fluentui/react-components";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant, queryScopeFor } from "../auth/types";
import { ApiError } from "../api/client";
import {
  createIdempotencyKey,
  createQuestionSet,
  getQuestionSetVersion,
  listAllQuestionSets,
  listAllQuestionSetVersions,
  reviseQuestionSet,
  type QuestionDraft,
  type QuestionSetVersion,
} from "../api/questions";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import { CitationInsightsPanel } from "./CitationInsightsPanel";
import { StandaloneMeasurementPanel } from "./StandaloneMeasurementPanel";
import { TopicQuestionGenerator } from "./TopicQuestionGenerator";
import "./QuestionSetsPage.css";

const purposeLabel = {
  optimization: "优化问题",
  frozen_evaluation: "冻结评估 · 不进入优化",
} as const;

type EditableQuestion = QuestionDraft & { localId: string };
const toDraft = (text: string, localId: string): EditableQuestion => ({
  localId,
  text,
  intent: "general",
  product_refs: [],
  market: "CN",
  language: "zh-CN",
  source: { kind: "user_provided" },
  weight: 1,
});
const editableFrom = (version: QuestionSetVersion): EditableQuestion[] =>
  version.questions.map((question) => ({
    question_id: question.question_id,
    localId: question.question_id,
    text: question.text,
    intent: question.intent,
    product_refs: question.product_refs,
    market: question.market,
    language: question.language,
    source: question.source,
    weight: question.weight,
  }));
const newId = () => createIdempotencyKey();
const validDraft = (entry: EditableQuestion) =>
  Boolean(
    entry.text.trim() &&
    entry.intent.trim() &&
    entry.market.trim() &&
    entry.language.trim(),
  ) &&
  entry.weight >= 1 &&
  Number.isSafeInteger(entry.weight);
const serverDraft = ({
  localId: _localId,
  ...item
}: EditableQuestion): QuestionDraft => ({
  ...item,
  text: item.text.trim(),
  intent: item.intent.trim(),
  market: item.market.trim(),
  language: item.language.trim(),
});
const errorText = (error: unknown) =>
  error instanceof Error ? error.message : "请求失败，请重试。";

export function QuestionSetsPage() {
  const { tenantId, projectId } = useParams();
  const { session } = useAuth();
  const client = useQueryClient();
  const membership = membershipForTenant(session, tenantId);
  const canWrite = Boolean(
    membership && ["tenant_admin", "member"].includes(membership.role),
  );
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  const baseKey = [
    "question-sets",
    scope?.userId,
    scope?.operatorId,
    tenantId,
    projectId,
  ];
  const sets = useQuery({
    queryKey: baseKey,
    queryFn: () => listAllQuestionSets(tenantId!, projectId!),
    enabled: Boolean(scope),
    retry: false,
  });
  const [setId, setSetId] = useState("");
  const [versionId, setVersionId] = useState("");
  const versions = useQuery({
    queryKey: [...baseKey, "versions", setId],
    queryFn: () => listAllQuestionSetVersions(tenantId!, projectId!, setId),
    enabled: Boolean(scope && setId),
    retry: false,
  });
  const selectedSet = sets.data?.items.find((item) => item.id === setId);
  useEffect(() => {
    if (selectedSet && !versionId) setVersionId(selectedSet.current_version_id);
  }, [selectedSet, versionId]);
  const version = useQuery({
    queryKey: [...baseKey, "version", setId, versionId],
    queryFn: () =>
      getQuestionSetVersion(tenantId!, projectId!, setId, versionId),
    enabled: Boolean(scope && setId && versionId),
    retry: false,
  });
  const [name, setName] = useState("");
  const [lines, setLines] = useState("");
  const [editing, setEditing] = useState(false);
  const [draftName, setDraftName] = useState("");
  const [draft, setDraft] = useState<EditableQuestion[]>([]);
  const [createKey, setCreateKey] = useState(newId);
  const [editKey, setEditKey] = useState(newId);
  const createLines = lines
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  const create = useMutation({
    mutationFn: () =>
      createQuestionSet(tenantId!, projectId!, {
        idempotency_key: createKey,
        name: name.trim(),
        questions: createLines.map((line) =>
          serverDraft(toDraft(line, newId())),
        ),
      }),
    onSuccess: (saved) => {
      setName("");
      setLines("");
      setCreateKey(newId());
      setSetId(saved.question_set_id);
      setVersionId(saved.id);
      void client.invalidateQueries({ queryKey: baseKey });
    },
  });
  const revise = useMutation({
    mutationFn: () =>
      reviseQuestionSet(tenantId!, projectId!, setId, {
        idempotency_key: editKey,
        base_version_id: versionId,
        name: draftName.trim(),
        questions: draft.map(serverDraft),
      }),
    onSuccess: (saved) => {
      setVersionId(saved.id);
      setEditing(false);
      setEditKey(newId());
      void client.invalidateQueries({ queryKey: baseKey });
    },
  });
  const startEdit = () => {
    if (!version.data || version.data.id !== selectedSet?.current_version_id)
      return;
    setDraft(editableFrom(version.data));
    setDraftName(version.data.name);
    setEditKey(newId());
    setEditing(true);
  };
  const patchDraft = (localId: string, update: Partial<EditableQuestion>) => {
    setEditKey(newId());
    setDraft((items) =>
      items.map((item) =>
        item.localId === localId ? { ...item, ...update } : item,
      ),
    );
  };
  if (!tenantId || !projectId)
    return <ErrorState title="缺少项目" detail="请从项目内进入问题集。" />;
  return (
    <main className="workbench-page question-sets-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">独立测量</p>
          <h1>测量与洞察</h1>
          <p>选择账号并提出任意问题即可独立测量，无需企业资料或优化周期。</p>
        </div>
        <Link to={`/app/${tenantId}/${projectId}/publications`}>
          查看发布目标
        </Link>
      </section>
      <TopicQuestionGenerator
        tenantId={tenantId}
        projectId={projectId}
        canWrite={canWrite}
      />
      <StandaloneMeasurementPanel
        key={`${tenantId}/${projectId}`}
        tenantId={tenantId}
        projectId={projectId}
        canWrite={canWrite}
      />
      <CitationInsightsPanel
        key={`${tenantId}/${projectId}/citations`}
        tenantId={tenantId}
        projectId={projectId}
      />
      <section aria-label="问题集与版本">
        <h2>问题集与版本</h2>
        <p>保存常用问题，按版本查看和调整；独立评估问题不会用于内容优化。</p>
      </section>
      {sets.isPending ? (
        <LoadingState label="正在读取问题集" />
      ) : sets.isError ? (
        <ErrorState
          title="问题集无法读取"
          detail={errorText(sets.error)}
          onRetry={() => void sets.refetch()}
        />
      ) : !sets.data?.items.length ? (
        <EmptyState
          title="尚无问题集"
          detail="创建一个命名问题集，逐行添加要测量的问题。"
        />
      ) : null}
      {sets.data?.items.length ? (
        <section aria-label="已有问题集">
          <Field label="选择问题集">
            <Select
              value={setId}
              onChange={(_, data) => {
                setSetId(data.value);
                setVersionId("");
                setEditing(false);
              }}
            >
              <option value="">选择一个问题集</option>
              {sets.data.items.map((item) => (
                <option key={item.id} value={item.id}>
                  {item.name} · v{item.current_revision}
                </option>
              ))}
            </Select>
          </Field>
        </section>
      ) : null}
      {canWrite && (
        <Card style={{ maxWidth: "100%", marginTop: 16 }}>
          <h2>创建问题集</h2>
          <Field label="问题集名称">
            <Input
              value={name}
              maxLength={200}
              onChange={(_, data) => {
                setName(data.value);
                setCreateKey(newId());
              }}
            />
          </Field>
          <Field
            label="每行一个问题"
            hint="可粘贴多行；保存后仍可创建新版本调整问题。"
          >
            <Textarea
              rows={6}
              value={lines}
              onChange={(_, data) => {
                setLines(data.value);
                setCreateKey(newId());
              }}
            />
          </Field>
          <Button
            appearance="primary"
            disabled={
              !name.trim() ||
              !createLines.length ||
              createLines.length > 100 ||
              create.isPending
            }
            onClick={() => create.mutate()}
          >
            {create.isPending ? "正在创建…" : "创建并封存 v1"}
          </Button>
          {create.isError && (
            <ErrorState
              title="问题集创建失败"
              detail={errorText(create.error)}
            />
          )}
        </Card>
      )}
      {!canWrite && (
        <p role="status">
          当前角色为只读；可以查看版本，但不能创建或修订问题集。
        </p>
      )}
      {setId &&
        (versions.isPending ? (
          <LoadingState label="正在读取版本历史" compact />
        ) : versions.isError ? (
          <ErrorState
            title="版本历史无法读取"
            detail={errorText(versions.error)}
            onRetry={() => void versions.refetch()}
          />
        ) : (
          <section aria-label="版本历史">
            <h2>版本历史</h2>
            <Field label="选择不可变版本">
              <Select
                value={versionId}
                onChange={(_, data) => {
                  setVersionId(data.value);
                  setEditing(false);
                }}
              >
                <option value="">选择版本</option>
                {versions.data?.items.map((item) => (
                  <option key={item.id} value={item.id}>
                    v{item.revision} · {item.name}
                  </option>
                ))}
              </Select>
            </Field>
          </section>
        ))}
      {versionId &&
        (version.isPending ? (
          <LoadingState label="正在读取不可变版本" compact />
        ) : version.isError ? (
          <ErrorState
            title="版本详情无法读取"
            detail={errorText(version.error)}
            onRetry={() => void version.refetch()}
          />
        ) : (
          version.data && (
            <section aria-label="问题集版本详情">
              <h2>
                {version.data.name} · v{version.data.revision}
              </h2>
              <p>
                优化 {version.data.optimization_count} · 冻结评估{" "}
                {version.data.evaluation_count}
                {" · "}划分规则 {version.data.split_policy_version} · 创建于{" "}
                {new Date(version.data.created_at).toLocaleString("zh-CN")}
              </p>
              {version.data.optimization_count === 0 && (
                <p role="status">
                  当前版本没有优化问题；冻结评估问题不进入内容优化。
                </p>
              )}
              {version.data.id !== selectedSet?.current_version_id && (
                <p role="status">
                  这是历史不可变版本；要编辑请切换到当前版本。
                </p>
              )}
              <ul style={{ paddingLeft: 24, overflowWrap: "anywhere" }}>
                {version.data.questions.map((item) => (
                  <li key={item.id}>
                    <strong>{item.text}</strong>{" "}
                    <Badge appearance="outline">
                      {purposeLabel[item.purpose]}
                    </Badge>
                    <p>
                      意图 {item.intent} · 产品引用{" "}
                      {item.product_refs.join("、") || "无"} · 市场{" "}
                      {item.market} · 语言 {item.language} · 来源{" "}
                      {item.source.kind} · 权重 {item.weight}
                    </p>
                  </li>
                ))}
              </ul>
              {canWrite &&
                version.data.id === selectedSet?.current_version_id &&
                !editing && (
                  <Button onClick={startEdit}>基于当前版本修订</Button>
                )}
              {editing && (
                <Card style={{ maxWidth: "100%" }}>
                  <h3>修订为下一版本</h3>
                  <Field label="下一版本名称">
                    <Input
                      value={draftName}
                      onChange={(_, data) => {
                        setDraftName(data.value);
                        setEditKey(newId());
                      }}
                    />
                  </Field>
                  {draft.map((item, index) => (
                    <div key={item.localId} className="question-sets-edit-row">
                      <Field label={`问题 ${index + 1}`}>
                        <Textarea
                          value={item.text}
                          onChange={(_, data) =>
                            patchDraft(item.localId, { text: data.value })
                          }
                        />
                      </Field>
                      <Field label={`问题 ${index + 1} 意图`}>
                        <Input
                          value={item.intent}
                          onChange={(_, data) =>
                            patchDraft(item.localId, { intent: data.value })
                          }
                        />
                      </Field>
                      <Field label={`问题 ${index + 1} 市场`}>
                        <Input
                          value={item.market}
                          onChange={(_, data) =>
                            patchDraft(item.localId, { market: data.value })
                          }
                        />
                      </Field>
                      <Field label={`问题 ${index + 1} 语言`}>
                        <Input
                          value={item.language}
                          onChange={(_, data) =>
                            patchDraft(item.localId, { language: data.value })
                          }
                        />
                      </Field>
                      <Field label={`问题 ${index + 1} 权重`}>
                        <Input
                          type="number"
                          min={1}
                          value={String(item.weight)}
                          onChange={(_, data) =>
                            patchDraft(item.localId, {
                              weight: Number(data.value),
                            })
                          }
                        />
                      </Field>
                      <Button
                        onClick={() => {
                          setEditKey(newId());
                          setDraft((items) =>
                            items.filter(
                              (entry) => entry.localId !== item.localId,
                            ),
                          );
                        }}
                      >
                        移除问题 {index + 1}
                      </Button>
                    </div>
                  ))}
                  <Button
                    disabled={draft.length >= 100}
                    onClick={() => {
                      setEditKey(newId());
                      setDraft((items) => [...items, toDraft("", newId())]);
                    }}
                  >
                    添加问题
                  </Button>
                  <Button
                    appearance="primary"
                    disabled={
                      !draftName.trim() ||
                      draft.some((item) => !validDraft(item)) ||
                      revise.isPending
                    }
                    onClick={() => revise.mutate()}
                  >
                    {revise.isPending ? "正在保存…" : "保存为新版本"}
                  </Button>
                  <Button
                    onClick={() => {
                      setEditing(false);
                      revise.reset();
                    }}
                  >
                    取消修订
                  </Button>
                  {revise.isError && (
                    <MessageBar intent="error">
                      <MessageBarBody>
                        {revise.error instanceof ApiError &&
                        revise.error.status === 409
                          ? "版本已变化或问题身份冲突；草稿已保留。刷新版本历史后核对，再选择如何修改。"
                          : `修订失败：${errorText(revise.error)}。草稿已保留。`}
                        <Button
                          onClick={() => {
                            void sets.refetch();
                            void versions.refetch();
                          }}
                        >
                          刷新版本历史
                        </Button>
                      </MessageBarBody>
                    </MessageBar>
                  )}
                </Card>
              )}
            </section>
          )
        ))}
    </main>
  );
}
