import { useState } from "react";
import {
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Text,
} from "@fluentui/react-components";
import { EditRegular } from "@fluentui/react-icons";
import { useTranslation } from "react-i18next";
import { ApiError } from "../api/client";
import {
  measurementFocusNames,
  measurementFocusPatch,
} from "../api/measurementFocus";
import { useProjectQuery, useUpdateProjectMutation } from "../api/projects";
import "../i18n/measurementFocus";
import "./MeasurementFocusPanel.css";

export function MeasurementFocusPanel({
  tenantId,
  projectId,
  canWrite,
  compact = false,
}: {
  tenantId: string;
  projectId: string;
  canWrite: boolean;
  compact?: boolean;
}) {
  const { t } = useTranslation("measurementFocus");
  const project = useProjectQuery(tenantId, projectId);
  const update = useUpdateProjectMutation(tenantId, projectId);
  const [draft, setDraft] = useState<{
    revision: number;
    brand: string;
    product: string;
  } | null>(null);
  const [error, setError] = useState<"saveFailed" | "conflict" | null>(null);
  const focus = measurementFocusNames(project.data?.settings)[0];

  function edit() {
    if (!project.data) return;
    setError(null);
    setDraft({
      revision: project.data.revision,
      brand: project.data.settings.brand_name,
      product: project.data.settings.product_name ?? "",
    });
  }

  async function save() {
    if (!draft || update.isPending) return;
    setError(null);
    try {
      await update.mutateAsync(
        measurementFocusPatch(draft.revision, draft.brand, draft.product),
      );
      setDraft(null);
    } catch (failure) {
      const conflict =
        failure instanceof ApiError &&
        (failure.status === 409 || failure.status === 412);
      setError(conflict ? "conflict" : "saveFailed");
      if (conflict) void project.refetch();
    }
  }

  return (
    <Card
      aria-label={t("title")}
      className={`measurement-focus${compact ? " measurement-focus--compact" : ""}`}
      appearance={compact ? "subtle" : "filled"}
    >
      <div className="measurement-focus-heading">
        <div className="measurement-focus-name">
          <Text size={200}>{t("title")}</Text>
          {project.data && (
            <Text weight="semibold" size={compact ? 300 : 500}>
              {focus || t(compact ? "emptyCompact" : "empty")}
            </Text>
          )}
        </div>
        {canWrite && project.data && !draft && (
          <Button
            appearance={compact ? "subtle" : "secondary"}
            icon={compact ? <EditRegular /> : undefined}
            onClick={edit}
          >
            {t(focus ? "edit" : "add")}
          </Button>
        )}
      </div>
      {!project.data && project.isPending && <p>{t("loading")}</p>}
      {!project.data && project.isError && (
        <MessageBar intent="error">
          <MessageBarBody>
            {t("loadFailed")}{" "}
            <Button onClick={() => void project.refetch()}>{t("retry")}</Button>
          </MessageBarBody>
        </MessageBar>
      )}
      {project.data && !focus && !draft && <Text>{t("optional")}</Text>}
      {!!project.data?.settings.competitors.length && (
        <Text size={200}>
          {t("comparison", {
            names: project.data.settings.competitors.join(" · "),
          })}
        </Text>
      )}
      {draft && (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void save();
          }}
          className="measurement-focus-form"
        >
          <Field label={t("brand")}>
            <Input
              value={draft.brand}
              maxLength={200}
              disabled={update.isPending}
              onChange={(_, data) => setDraft({ ...draft, brand: data.value })}
            />
          </Field>
          <Field label={t("product")} hint={t("productHint")}>
            <Input
              value={draft.product}
              maxLength={200}
              disabled={update.isPending}
              onChange={(_, data) =>
                setDraft({ ...draft, product: data.value })
              }
            />
          </Field>
          {error && (
            <MessageBar intent="error">
              <MessageBarBody>{t(error)}</MessageBarBody>
            </MessageBar>
          )}
          <div style={{ display: "flex", gap: 8 }}>
            <Button
              appearance="primary"
              type="submit"
              disabled={update.isPending || error === "conflict"}
            >
              {t(update.isPending ? "saving" : "save")}
            </Button>
            <Button
              disabled={update.isPending}
              onClick={() => {
                setDraft(null);
                setError(null);
              }}
            >
              {t("cancel")}
            </Button>
          </div>
        </form>
      )}
    </Card>
  );
}
