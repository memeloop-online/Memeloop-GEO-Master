import { createIdempotencyKey, apiFetch } from "./client";

export type QuestionPurpose = "optimization" | "frozen_evaluation";
export type QuestionSourceKind =
  "user_provided" | "sales_consultation" | "product" | "faq" | "generated";

export interface QuestionDraft {
  question_id?: string;
  text: string;
  intent: string;
  product_refs: string[];
  market: string;
  language: string;
  source: { kind: QuestionSourceKind; reference_id?: string };
  weight: number;
}

export interface QuestionRevision extends Omit<QuestionDraft, "question_id"> {
  id: string;
  question_id: string;
  purpose: QuestionPurpose;
  split_policy_version: string;
}

export interface QuestionSetSummary {
  id: string;
  name: string;
  current_version_id: string;
  current_revision: number;
  question_count: number;
  optimization_count: number;
  evaluation_count: number;
}

export interface QuestionSetVersionSummary {
  id: string;
  question_set_id: string;
  revision: number;
  parent_version_id: string | null;
  name: string;
  question_count: number;
  optimization_count: number;
  evaluation_count: number;
  split_policy_version: string;
  created_at: string;
}

export interface QuestionSetVersion extends QuestionSetVersionSummary {
  questions: QuestionRevision[];
  content_hash: string;
}

export interface QuestionSetPage {
  items: QuestionSetSummary[];
  next_cursor: string | null;
}

export interface QuestionSetVersionPage {
  items: QuestionSetVersionSummary[];
  next_cursor: number | null;
}

const base = (projectId: string) =>
  `/projects/${encodeURIComponent(projectId)}/question-sets`;
const scope = (tenantId: string, projectId: string) => ({
  tenantId,
  projectId,
});
const versionsPath = (projectId: string, setId: string) =>
  `${base(projectId)}/${encodeURIComponent(setId)}/versions`;

export function listQuestionSets(
  tenantId: string,
  projectId: string,
  after?: string,
) {
  return apiFetch<QuestionSetPage>(
    `${base(projectId)}${after ? `?after=${encodeURIComponent(after)}` : ""}`,
    scope(tenantId, projectId),
  );
}

/** Read all bounded pages so older immutable versions remain selectable. */
export async function listAllQuestionSets(
  tenantId: string,
  projectId: string,
): Promise<QuestionSetPage> {
  const items: QuestionSetSummary[] = [];
  let after: string | undefined;
  for (let page = 0; page < 100; page++) {
    const result = await listQuestionSets(tenantId, projectId, after);
    items.push(...result.items);
    if (!result.next_cursor) return { items, next_cursor: null };
    if (result.next_cursor === after) break;
    after = result.next_cursor;
  }
  throw new Error("问题集数量超过当前展示上限，请联系管理员。");
}

export function listQuestionSetVersions(
  tenantId: string,
  projectId: string,
  setId: string,
  afterRevision?: number,
) {
  return apiFetch<QuestionSetVersionPage>(
    `${versionsPath(projectId, setId)}${afterRevision ? `?after_revision=${afterRevision}` : ""}`,
    scope(tenantId, projectId),
  );
}

export async function listAllQuestionSetVersions(
  tenantId: string,
  projectId: string,
  setId: string,
): Promise<QuestionSetVersionPage> {
  const items: QuestionSetVersionSummary[] = [];
  let after: number | undefined;
  for (let page = 0; page < 100; page++) {
    const result = await listQuestionSetVersions(
      tenantId,
      projectId,
      setId,
      after,
    );
    items.push(...result.items);
    if (result.next_cursor === null) return { items, next_cursor: null };
    if (result.next_cursor === after) break;
    after = result.next_cursor;
  }
  throw new Error("版本历史超过当前展示上限，请联系管理员。");
}

export function getQuestionSetVersion(
  tenantId: string,
  projectId: string,
  setId: string,
  versionId: string,
) {
  return apiFetch<QuestionSetVersion>(
    `${versionsPath(projectId, setId)}/${encodeURIComponent(versionId)}`,
    scope(tenantId, projectId),
  );
}

export function createQuestionSet(
  tenantId: string,
  projectId: string,
  input: { idempotency_key: string; name: string; questions: QuestionDraft[] },
) {
  return apiFetch<QuestionSetVersion>(base(projectId), {
    ...scope(tenantId, projectId),
    method: "POST",
    idempotencyKey: input.idempotency_key,
    body: input,
  });
}

export function reviseQuestionSet(
  tenantId: string,
  projectId: string,
  setId: string,
  input: {
    idempotency_key: string;
    base_version_id: string;
    name: string;
    questions: QuestionDraft[];
  },
) {
  return apiFetch<QuestionSetVersion>(versionsPath(projectId, setId), {
    ...scope(tenantId, projectId),
    method: "POST",
    idempotencyKey: input.idempotency_key,
    body: input,
  });
}

export { createIdempotencyKey };
