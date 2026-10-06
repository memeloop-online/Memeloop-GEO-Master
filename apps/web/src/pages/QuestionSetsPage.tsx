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
  Tab,
  TabList,
  Textarea,
} from "@fluentui/react-components";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant, queryScopeFor } from "../auth/types";
import { ApiError } from "../api/client";
import { formatUiDate } from "../i18n";
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
import "./measurementMessages";
import "./QuestionSetsPage.css";

const sourceLabelKeys = {
  user_provided: "sourceUserProvided",
  sales_consultation: "sourceSalesConsultation",
  product: "sourceProduct",
  faq: "sourceFaq",
  generated: "sourceGenerated",
} as const;
const sourceLabelKey = (kind: unknown) =>
  typeof kind === "string" &&
  Object.prototype.hasOwnProperty.call(sourceLabelKeys, kind)
    ? sourceLabelKeys[kind as keyof typeof sourceLabelKeys]
    : "sourceUnknown";

type EditableQuestion = QuestionDraft & { localId: string };
type RevisionDraft = {
  baseVersionId: string;
  name: string;
  questions: EditableQuestion[];
  key: string;
};
type StoredDrafts = {
  name: string;
  lines: string;
  creating: boolean;
  createKey: string;
  drafts: Record<string, RevisionDraft>;
};
const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);
const isKey = (value: unknown): value is string =>
  typeof value === "string" &&
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
const isEditableQuestion = (value: unknown): value is EditableQuestion =>
  isRecord(value) &&
  typeof value.localId === "string" &&
  (value.question_id === undefined || typeof value.question_id === "string") &&
  typeof value.text === "string" &&
  typeof value.intent === "string" &&
  typeof value.market === "string" &&
  typeof value.language === "string" &&
  Array.isArray(value.product_refs) &&
  value.product_refs.every((item) => typeof item === "string") &&
  isRecord(value.source) &&
  [
    "user_provided",
    "sales_consultation",
    "product",
    "faq",
    "generated",
  ].includes(String(value.source.kind)) &&
  (value.source.reference_id === undefined ||
    typeof value.source.reference_id === "string") &&
  typeof value.weight === "number" &&
  Number.isFinite(value.weight);
const isRevisionDraft = (value: unknown): value is RevisionDraft =>
  isRecord(value) &&
  typeof value.baseVersionId === "string" &&
  typeof value.name === "string" &&
  isKey(value.key) &&
  Array.isArray(value.questions) &&
  value.questions.every(isEditableQuestion);
const readStoredDrafts = (value: unknown): StoredDrafts | null => {
  if (!isRecord(value)) return null;
  if (value.name !== undefined && typeof value.name !== "string") return null;
  if (value.lines !== undefined && typeof value.lines !== "string") return null;
  if (value.creating !== undefined && typeof value.creating !== "boolean")
    return null;
  if (value.drafts !== undefined && !isRecord(value.drafts)) return null;
  const drafts = value.drafts ?? {};
  if (!Object.values(drafts).every(isRevisionDraft)) return null;
  return {
    name: value.name ?? "",
    lines: value.lines ?? "",
    creating: value.creating ?? false,
    createKey: isKey(value.createKey) ? value.createKey : newId(),
    drafts: drafts as Record<string, RevisionDraft>,
  };
};
const tabs = ["measure", "records", "insights", "sets"] as const;
type MeasurementTab = (typeof tabs)[number];
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
  const { t } = useTranslation("measurement");
  const [params, setParams] = useSearchParams();
  const tab: MeasurementTab = tabs.includes(params.get("tab") as MeasurementTab)
    ? (params.get("tab") as MeasurementTab)
    : params.has("record") || params.has("planId")
      ? "records"
      : params.has("set") || params.has("questionSetId")
        ? "sets"
        : "measure";
  const setId = params.get("set") ?? params.get("questionSetId") ?? "";
  const requestedVersionId =
    params.get("version") ?? params.get("versionId") ?? "";
  const updateSelection = (values: Record<string, string | null>) => {
    setParams((previous) => {
      const next = new URLSearchParams(previous);
      for (const [key, value] of Object.entries(values)) {
        if (key === "set") next.delete("questionSetId");
        if (key === "version") next.delete("versionId");
        if (value) next.set(key, value);
        else next.delete(key);
      }
      return next;
    });
  };
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
  const draftStorageKey = `measurement-drafts:${scope?.userId}:${scope?.operatorId}:${tenantId}:${projectId}`;
  const sets = useQuery({
    queryKey: baseKey,
    queryFn: () => listAllQuestionSets(tenantId!, projectId!),
    enabled: Boolean(scope),
    retry: false,
  });
  const versions = useQuery({
    queryKey: [...baseKey, "versions", setId],
    queryFn: () => listAllQuestionSetVersions(tenantId!, projectId!, setId),
    enabled: Boolean(scope && setId),
    retry: false,
  });
  const selectedSet = sets.data?.items.find((item) => item.id === setId);
  const versionId = requestedVersionId || selectedSet?.current_version_id || "";
  const version = useQuery({
    queryKey: [...baseKey, "version", setId, versionId],
    queryFn: () =>
      getQuestionSetVersion(tenantId!, projectId!, setId, versionId),
    enabled: Boolean(scope && setId && versionId),
    retry: false,
  });
  const [name, setName] = useState("");
  const [lines, setLines] = useState("");
  const [drafts, setDrafts] = useState<Record<string, RevisionDraft>>({});
  const activeDraft = drafts[setId];
  const [creating, setCreating] = useState(false);
  const [createKey, setCreateKey] = useState(newId);
  const [loadedDraftScope, setLoadedDraftScope] = useState("");
  useEffect(() => {
    if (!scope || loadedDraftScope === draftStorageKey) return;
    try {
      const saved = readStoredDrafts(
        JSON.parse(sessionStorage.getItem(draftStorageKey) ?? "{}"),
      );
      setName(saved?.name ?? "");
      setLines(saved?.lines ?? "");
      setCreating(saved?.creating ?? false);
      setCreateKey(saved?.createKey ?? newId());
      setDrafts(saved?.drafts ?? {});
    } catch {
      setName("");
      setLines("");
      setCreating(false);
      setCreateKey(newId());
      setDrafts({});
    }
    setLoadedDraftScope(draftStorageKey);
  }, [scope, draftStorageKey, loadedDraftScope]);
  useEffect(() => {
    if (loadedDraftScope !== draftStorageKey) return;
    try {
      sessionStorage.setItem(
        draftStorageKey,
        JSON.stringify({ name, lines, creating, createKey, drafts }),
      );
    } catch {
      // Private browsing and storage quotas must not prevent editing.
    }
  }, [
    draftStorageKey,
    loadedDraftScope,
    name,
    lines,
    creating,
    createKey,
    drafts,
  ]);
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
      setCreating(false);
      updateSelection({
        tab: "sets",
        set: saved.question_set_id,
        version: saved.id,
      });
      void client.invalidateQueries({ queryKey: baseKey });
    },
  });
  const revise = useMutation({
    mutationFn: () =>
      reviseQuestionSet(tenantId!, projectId!, setId, {
        idempotency_key: activeDraft.key,
        base_version_id: activeDraft.baseVersionId,
        name: activeDraft.name.trim(),
        questions: activeDraft.questions.map(serverDraft),
      }),
    onSuccess: (saved) => {
      updateSelection({ version: saved.id });
      setDrafts((items) => {
        const next = { ...items };
        delete next[setId];
        return next;
      });
      void client.invalidateQueries({ queryKey: baseKey });
    },
  });
  const startEdit = () => {
    if (!version.data || version.data.id !== selectedSet?.current_version_id)
      return;
    setDrafts((items) => ({
      ...items,
      [setId]: {
        baseVersionId: version.data!.id,
        name: version.data!.name,
        questions: editableFrom(version.data!),
        key: newId(),
      },
    }));
  };
  const patchRevision = (update: (previous: RevisionDraft) => RevisionDraft) =>
    setDrafts((items) => ({
      ...items,
      [setId]: update(items[setId]),
    }));
  const patchDraft = (localId: string, update: Partial<EditableQuestion>) => {
    patchRevision((previous) => ({
      ...previous,
      key: newId(),
      questions: previous.questions.map((item) =>
        item.localId === localId ? { ...item, ...update } : item,
      ),
    }));
  };
  if (!tenantId || !projectId)
    return <ErrorState title="缺少项目" detail="请从项目内进入问题集。" />;
  return (
    <main className="workbench-page question-sets-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">独立测量</p>
          <h1>测量与洞察</h1>
          <p>{t("pageIntro")}</p>
        </div>
        <Link to={`/app/${tenantId}/${projectId}/publications`}>
          查看发布目标
        </Link>
      </section>
      <nav aria-label="测量页面" className="measurement-tabs">
        <TabList
          aria-label="测量页面"
          selectedValue={tab}
          onTabSelect={(_, data) =>
            updateSelection({ tab: String(data.value) })
          }
        >
          {tabs.map((entry) => (
            <Tab id={`measurement-tab-${entry}`} key={entry} value={entry}>
              {t(entry)}
            </Tab>
          ))}
        </TabList>
      </nav>
      <section
        role="tabpanel"
        aria-labelledby="measurement-tab-measure"
        hidden={tab !== "measure"}
        aria-label={t("measure")}
      >
        <StandaloneMeasurementPanel
          key={`${tenantId}/${projectId}`}
          tenantId={tenantId}
          projectId={projectId}
          canWrite={canWrite}
        />
        <details className="measurement-prediction">
          <summary>{t("prediction")}</summary>
          <TopicQuestionGenerator
            tenantId={tenantId}
            projectId={projectId}
            canWrite={canWrite}
          />
        </details>
      </section>
      <section
        role="tabpanel"
        aria-labelledby="measurement-tab-records"
        hidden={tab !== "records"}
        aria-label={t("records")}
      >
        <StandaloneMeasurementPanel
          key={`${tenantId}/${projectId}/records`}
          tenantId={tenantId}
          projectId={projectId}
          canWrite={canWrite}
          recordsOnly
        />
      </section>
      <section
        role="tabpanel"
        aria-labelledby="measurement-tab-insights"
        hidden={tab !== "insights"}
        aria-label={t("insights")}
      >
        <CitationInsightsPanel
          key={`${tenantId}/${projectId}/citations`}
          tenantId={tenantId}
          projectId={projectId}
        />
      </section>
      <section
        role="tabpanel"
        aria-labelledby="measurement-tab-sets"
        hidden={tab !== "sets"}
        aria-label={t("sets")}
      >
        <section aria-label="问题集与版本">
          <h2>{t("questionsHeading")}</h2>
          <p>{t("questionsIntro")}</p>
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
            detail="需要重复测量这些问题时，可以新建问题集。"
          />
        ) : null}
        {sets.data?.items.length ? (
          <section aria-label="已有问题集">
            <Field label="选择问题集">
              <Select
                value={setId}
                onChange={(_, data) => {
                  updateSelection({
                    tab: "sets",
                    set: data.value,
                    version: null,
                  });
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
          <Button onClick={() => setCreating((open) => !open)}>
            {creating ? t("hideNewSet") : t("newSet")}
          </Button>
        )}
        {canWrite && creating && (
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
              {create.isPending ? t("creatingSet") : t("createSet")}
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
            <section aria-label={t("versionHistory")}>
              <h2>{t("versionHistory")}</h2>
              <Field label={t("selectImmutableVersion")}>
                <Select
                  value={versionId}
                  onChange={(_, data) => {
                    updateSelection({ tab: "sets", version: data.value });
                  }}
                >
                  <option value="">{t("selectVersion")}</option>
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
              <section aria-label={t("versionDetails")}>
                <h2>
                  {version.data.name} · v{version.data.revision}
                </h2>
                <p>
                  {t("versionSummary", {
                    optimization: version.data.optimization_count,
                    evaluation: version.data.evaluation_count,
                    createdAt: formatUiDate(version.data.created_at),
                  })}
                </p>
                {version.data.optimization_count === 0 && (
                  <p role="status">{t("noOptimizationQuestions")}</p>
                )}
                {version.data.id !== selectedSet?.current_version_id && (
                  <p role="status">{t("historicalVersion")}</p>
                )}
                <ul style={{ paddingLeft: 24, overflowWrap: "anywhere" }}>
                  {version.data.questions.map((item) => (
                    <li key={item.id}>
                      <strong>{item.text}</strong>{" "}
                      <Badge appearance="outline">
                        {t(
                          item.purpose === "frozen_evaluation"
                            ? "purposeFrozenEvaluation"
                            : "purposeOptimization",
                        )}
                      </Badge>
                      <p>
                        {t("questionIntent", { value: item.intent })} ·{" "}
                        {t("questionProductReferences", {
                          value:
                            item.product_refs.join(
                              t("productReferenceSeparator"),
                            ) || t("noProductReferences"),
                        })}{" "}
                        · {t("questionMarket", { value: item.market })} ·{" "}
                        {t("questionLanguage", { value: item.language })} ·{" "}
                        {t("questionSource", {
                          value: t(sourceLabelKey(item.source.kind)),
                        })}{" "}
                        · {t("questionWeight", { value: item.weight })}
                      </p>
                    </li>
                  ))}
                </ul>
                {canWrite &&
                  version.data.id === selectedSet?.current_version_id &&
                  !activeDraft && (
                    <Button onClick={startEdit}>基于当前版本修订</Button>
                  )}
                {activeDraft && (
                  <Card style={{ maxWidth: "100%" }}>
                    <h3>修订为下一版本</h3>
                    <Field label="下一版本名称">
                      <Input
                        value={activeDraft.name}
                        onChange={(_, data) => {
                          patchRevision((previous) => ({
                            ...previous,
                            name: data.value,
                            key: newId(),
                          }));
                        }}
                      />
                    </Field>
                    {activeDraft.questions.map((item, index) => (
                      <div
                        key={item.localId}
                        className="question-sets-edit-row"
                      >
                        <Field label={`问题 ${index + 1}`}>
                          <Textarea
                            value={item.text}
                            onChange={(_, data) =>
                              patchDraft(item.localId, { text: data.value })
                            }
                          />
                        </Field>
                        <details className="question-sets-more-settings">
                          <summary
                            aria-label={`${t("moreSettings")} ${index + 1}`}
                          >
                            {t("moreSettings")}
                          </summary>
                          <div className="question-sets-secondary-fields">
                            <Field label={`问题 ${index + 1} 意图`}>
                              <Input
                                value={item.intent}
                                onChange={(_, data) =>
                                  patchDraft(item.localId, {
                                    intent: data.value,
                                  })
                                }
                              />
                            </Field>
                            <Field label={`问题 ${index + 1} 市场`}>
                              <Input
                                value={item.market}
                                onChange={(_, data) =>
                                  patchDraft(item.localId, {
                                    market: data.value,
                                  })
                                }
                              />
                            </Field>
                            <Field label={`问题 ${index + 1} 语言`}>
                              <Input
                                value={item.language}
                                onChange={(_, data) =>
                                  patchDraft(item.localId, {
                                    language: data.value,
                                  })
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
                          </div>
                        </details>
                        <Button
                          onClick={() => {
                            patchRevision((previous) => ({
                              ...previous,
                              key: newId(),
                              questions: previous.questions.filter(
                                (entry) => entry.localId !== item.localId,
                              ),
                            }));
                          }}
                        >
                          移除问题 {index + 1}
                        </Button>
                      </div>
                    ))}
                    <Button
                      disabled={activeDraft.questions.length >= 100}
                      onClick={() => {
                        patchRevision((previous) => ({
                          ...previous,
                          key: newId(),
                          questions: [
                            ...previous.questions,
                            toDraft("", newId()),
                          ],
                        }));
                      }}
                    >
                      添加问题
                    </Button>
                    <Button
                      appearance="primary"
                      disabled={
                        !activeDraft.name.trim() ||
                        activeDraft.questions.some(
                          (item) => !validDraft(item),
                        ) ||
                        revise.isPending
                      }
                      onClick={() => revise.mutate()}
                    >
                      {revise.isPending ? "正在保存…" : "保存为新版本"}
                    </Button>
                    <Button
                      onClick={() => {
                        setDrafts((items) => {
                          const next = { ...items };
                          delete next[setId];
                          return next;
                        });
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
      </section>
    </main>
  );
}
