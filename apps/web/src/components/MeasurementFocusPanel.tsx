import { useState } from "react";
import {
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Textarea,
  Text,
} from "@fluentui/react-components";
import { EditRegular } from "@fluentui/react-icons";
import { useTranslation } from "react-i18next";
import { ApiError } from "../api/client";
import {
  measurementFocusTargets,
  measurementFocusPatch,
} from "../api/measurementFocus";
import { useProjectQuery, useUpdateProjectMutation } from "../api/projects";
import type { MeasurementTarget } from "../api/projects";
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
    targets: MeasurementTarget[];
  } | null>(null);
  const [error, setError] = useState<"saveFailed" | "conflict" | null>(null);
  const targets = measurementFocusTargets(project.data?.settings);

  function changeTarget(index: number, patch: Partial<MeasurementTarget>) {
    if (!draft) return;
    setDraft({
      ...draft,
      targets: draft.targets.map((target, position) =>
        position === index ? { ...target, ...patch } : target,
      ),
    });
  }

  function edit() {
    if (!project.data) return;
    setError(null);
    setDraft({
      revision: project.data.revision,
      targets: targets.map((target) => ({
        ...target,
        aliases: [...target.aliases],
      })),
    });
  }

  async function save() {
    if (!draft || update.isPending) return;
    setError(null);
    try {
      await update.mutateAsync(
        measurementFocusPatch(draft.revision, draft.targets),
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
              {targets.length
                ? targets
                    .map(
                      (target) =>
                        `${target.name} · ${t(`kind_${target.kind}`)}`,
                    )
                    .join(" / ")
                : t(compact ? "emptyCompact" : "empty")}
            </Text>
          )}
        </div>
        {canWrite && project.data && !draft && (
          <Button
            appearance={compact ? "subtle" : "secondary"}
            icon={compact ? <EditRegular /> : undefined}
            onClick={edit}
          >
            {t(targets.length ? "edit" : "add")}
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
      {project.data && !targets.length && !draft && (
        <Text>{t("optional")}</Text>
      )}
      {project.data && <Text size={200}>{t("viewingScope")}</Text>}
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
          {draft.targets.map((target, index) => (
            <fieldset key={index} className="measurement-focus-target">
              <legend>{t("targetNumber", { number: index + 1 })}</legend>
              <Field label={t("name")} required>
                <Input
                  value={target.name}
                  required
                  disabled={update.isPending}
                  onChange={(_, data) =>
                    changeTarget(index, { name: data.value })
                  }
                />
              </Field>
              <Field label={t("kind")}>
                <Select
                  value={target.kind}
                  disabled={update.isPending}
                  onChange={(_, data) =>
                    changeTarget(index, {
                      kind: data.value as MeasurementTarget["kind"],
                    })
                  }
                >
                  {(
                    [
                      "product",
                      "restaurant",
                      "brand",
                      "service",
                      "other",
                    ] as const
                  ).map((kind) => (
                    <option key={kind} value={kind}>
                      {t(`kind_${kind}`)}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field label={t("aliases")} hint={t("aliasesHint")}>
                <Textarea
                  value={target.aliases.join("\n")}
                  disabled={update.isPending}
                  onChange={(_, data) =>
                    changeTarget(index, { aliases: data.value.split("\n") })
                  }
                />
              </Field>
              <Button
                disabled={update.isPending}
                onClick={() =>
                  setDraft({
                    ...draft,
                    targets: draft.targets.filter(
                      (_, position) => position !== index,
                    ),
                  })
                }
              >
                {t("removeTarget", { number: index + 1 })}
              </Button>
            </fieldset>
          ))}
          <Button
            disabled={update.isPending}
            onClick={() =>
              setDraft({
                ...draft,
                targets: [
                  ...draft.targets,
                  { name: "", kind: "product", aliases: [] },
                ],
              })
            }
          >
            {t("addTarget")}
          </Button>
          {error && (
            <MessageBar intent="error">
              <MessageBarBody>{t(error)}</MessageBarBody>
            </MessageBar>
          )}
          <div style={{ display: "flex", gap: 8 }}>
            <Button
              appearance="primary"
              type="submit"
              disabled={
                update.isPending ||
                error === "conflict" ||
                draft.targets.some((target) => !target.name.trim())
              }
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
