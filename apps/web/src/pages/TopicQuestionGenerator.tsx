import { useRef, useState } from "react";
import {
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
} from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { createAgentConversation, postAgentMessage } from "../api/agent";
import { createIdempotencyKey } from "../api/client";
import { useProjectQuery } from "../api/projects";
import "../i18n";
import "./topicQuestionMessages";

interface PendingPrediction {
  conversationKey: string;
  messageKey: string;
  content: string;
  conversationId?: string;
}

export function TopicQuestionGenerator({
  tenantId,
  projectId,
  canWrite,
}: {
  tenantId: string;
  projectId: string;
  canWrite: boolean;
}) {
  const { t } = useTranslation("topicQuestions");
  const navigate = useNavigate();
  const project = useProjectQuery(tenantId, projectId);
  const [topic, setTopic] = useState("");
  const [market, setMarket] = useState("");
  const [language, setLanguage] = useState("");
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const inFlight = useRef(false);
  const pending = useRef<PendingPrediction | null>(null);

  async function submit() {
    if (!canWrite || !topic.trim() || inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setFailed(false);
    try {
      // The topic is sent as user content only. It cannot alter the agent's
      // system prompt or turn a generated suggestion into observed evidence.
      pending.current ??= {
        conversationKey: createIdempotencyKey(),
        messageKey: createIdempotencyKey(),
        content: [
          t("instruction"),
          t("topicData", { value: JSON.stringify(topic.trim()) }),
          t("marketData", {
            value: JSON.stringify(
              market.trim() || project.data?.settings.market?.trim() || "CN",
            ),
          }),
          t("languageData", {
            value: JSON.stringify(
              language.trim() ||
                project.data?.settings.language?.trim() ||
                "zh-CN",
            ),
          }),
          t("routeData", {
            value: `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/measurement`,
          }),
        ].join("\n"),
      };
      const request = pending.current;
      if (!request.conversationId) {
        const conversation = await createAgentConversation(
          tenantId,
          projectId,
          {},
          request.conversationKey,
        );
        request.conversationId = conversation.id;
      }
      await postAgentMessage(
        tenantId,
        projectId,
        request.conversationId,
        { content: request.content },
        request.messageKey,
      );
      navigate(
        `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/chat/${encodeURIComponent(request.conversationId)}`,
      );
    } catch {
      setFailed(true);
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  }

  return (
    <Card aria-label={t("title")}>
      <h2>{t("title")}</h2>
      <p>{t("description")}</p>
      {canWrite ? (
        <>
          <Field label={t("topic")}>
            <Input
              value={topic}
              placeholder={t("placeholder")}
              maxLength={400}
              disabled={busy || Boolean(pending.current)}
              onChange={(_, data) => setTopic(data.value)}
            />
          </Field>
          <Field label={t("market")} hint={t("marketHint")}>
            <Input
              value={market}
              maxLength={80}
              disabled={busy || Boolean(pending.current)}
              onChange={(_, data) => setMarket(data.value)}
            />
          </Field>
          <Field label={t("language")} hint={t("languageHint")}>
            <Input
              value={language}
              maxLength={80}
              disabled={busy || Boolean(pending.current)}
              onChange={(_, data) => setLanguage(data.value)}
            />
          </Field>
          <Button
            appearance="primary"
            disabled={!topic.trim() || busy}
            onClick={() => void submit()}
          >
            {busy ? t("submitting") : failed ? t("retry") : t("submit")}
          </Button>
          {failed && (
            <MessageBar intent="warning">
              <MessageBarBody>{t("error")}</MessageBarBody>
            </MessageBar>
          )}
        </>
      ) : (
        <p role="status">{t("noAccess")}</p>
      )}
    </Card>
  );
}
