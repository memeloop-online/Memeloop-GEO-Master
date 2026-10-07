import { createContext, useContext, useEffect, useState } from "react";
import { Button } from "@fluentui/react-components";
import { NodeViewWrapper, type NodeViewProps } from "@tiptap/react";
import { useTranslation } from "react-i18next";
import {
  findContentMediaBinding,
  readContentMediaBytes,
  validMediaReference,
} from "../api/contentMedia";

export const MediaContext = createContext<{
  tenantId: string;
  projectId: string;
} | null>(null);

export function ContentMediaNode({ node, selected }: NodeViewProps) {
  const scope = useContext(MediaContext);
  const { t } = useTranslation();
  const reference = node.attrs;
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState(false);
  const [epoch, setEpoch] = useState(0);
  useEffect(() => {
    if (!scope || !validMediaReference(reference)) {
      setError(true);
      return;
    }
    const controller = new AbortController();
    let objectUrl: string | null = null;
    setUrl(null);
    setError(false);
    void (async () => {
      try {
        const binding = await findContentMediaBinding(
          scope.tenantId,
          scope.projectId,
          reference,
          controller.signal,
        );
        if (!binding) throw new Error("Image usage is unavailable");
        const blob = await readContentMediaBytes(
          scope.tenantId,
          scope.projectId,
          binding.binding_id,
          controller.signal,
        );
        if (!["image/png", "image/jpeg", "image/webp"].includes(blob.type))
          throw new Error("Image response has an unsupported type");
        if (controller.signal.aborted) return;
        objectUrl = URL.createObjectURL(blob);
        setUrl(objectUrl);
      } catch {
        if (!controller.signal.aborted) setError(true);
      }
    })();
    return () => {
      controller.abort();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
    // Key changes are represented by a new immutable media node.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    scope?.tenantId,
    scope?.projectId,
    reference.object_id,
    reference.object_version,
    reference.sha256,
    epoch,
  ]);

  return (
    <NodeViewWrapper
      className={`content-media-node${selected ? " is-selected" : ""}`}
    >
      <figure contentEditable={false}>
        {url ? (
          <img src={url} alt={reference.alt} />
        ) : (
          <div className="content-media-placeholder" role="status">
            {error
              ? t("generatedEditor.mediaUnavailable")
              : t("generatedEditor.mediaLoading")}
          </div>
        )}
        {reference.caption && <figcaption>{reference.caption}</figcaption>}
        {error && scope && validMediaReference(reference) && (
          <Button size="small" onClick={() => setEpoch((value) => value + 1)}>
            {t("generatedEditor.mediaRetry")}
          </Button>
        )}
      </figure>
    </NodeViewWrapper>
  );
}
