import { useEffect, useState } from "react";
import { Button, Input } from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import {
  uploadAgentAttachment,
  type AgentAttachmentReference,
} from "../api/agent";
import {
  bindContentMedia,
  listContentMedia,
  mediaKeyFromAttachment,
  readContentMediaBytes,
  type ContentMediaBinding,
  type MediaReference,
} from "../api/contentMedia";
import { createIdempotencyKey } from "../api/client";

interface Props {
  tenantId: string;
  projectId: string;
  onSelect: (reference: MediaReference) => void;
}

function filenameAlt(name: string, fallback: string) {
  const stem = name
    .replace(/\.[^.]+$/, "")
    .replace(/[\u0000-\u001f\u007f]/gu, "")
    .trim();
  return stem.slice(0, 200) || fallback;
}

function MediaThumbnail({
  binding,
  tenantId,
  projectId,
}: {
  binding: ContentMediaBinding;
  tenantId: string;
  projectId: string;
}) {
  const [element, setElement] = useState<HTMLSpanElement | null>(null);
  const [visible, setVisible] = useState(false);
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!element) return;
    if (typeof IntersectionObserver === "undefined") {
      setVisible(true);
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setVisible(true);
          observer.disconnect();
        }
      },
      { rootMargin: "100px" },
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [element]);
  useEffect(() => {
    if (!visible) return;
    const controller = new AbortController();
    let objectUrl: string | null = null;
    void (async () => {
      try {
        const blob = await readContentMediaBytes(
          tenantId,
          projectId,
          binding.binding_id,
          controller.signal,
        );
        if (
          !["image/png", "image/jpeg", "image/webp"].includes(blob.type) ||
          blob.type !== binding.image.media_type ||
          controller.signal.aborted
        )
          return;
        objectUrl = URL.createObjectURL(blob);
        setUrl(objectUrl);
      } catch {
        // The accessible dimensions remain available if the preview cannot load.
      }
    })();
    return () => {
      controller.abort();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [
    binding.binding_id,
    binding.image.media_type,
    projectId,
    tenantId,
    visible,
  ]);
  return (
    <span
      ref={setElement}
      className="content-media-thumbnail"
      aria-hidden="true"
    >
      {url && <img src={url} alt="" />}
    </span>
  );
}

export function ContentMediaPicker({ tenantId, projectId, onSelect }: Props) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [items, setItems] = useState<ContentMediaBinding[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [file, setFile] = useState<File | null>(null);
  const [attachment, setAttachment] = useState<AgentAttachmentReference | null>(
    null,
  );
  const [alt, setAlt] = useState("");
  const [caption, setCaption] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [selected, setSelected] = useState<ContentMediaBinding | null>(null);

  const load = async (after?: string) => {
    try {
      setError("");
      const page = await listContentMedia(tenantId, projectId, after);
      setItems((previous) =>
        after ? [...previous, ...page.items] : page.items,
      );
      setCursor(page.next_cursor);
    } catch {
      setError(t("generatedEditor.mediaListError"));
    }
  };
  useEffect(() => {
    if (open) void load();
    // Scope changes remount the owning editor. Loading on open avoids background requests.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, tenantId, projectId]);

  const insert = (binding: ContentMediaBinding) => {
    const reference = {
      ...binding.image.key,
      alt: alt.trim(),
      caption,
    };
    if (!reference.alt) {
      setError(t("generatedEditor.mediaAltRequired"));
      return;
    }
    if (binding.state !== "active") {
      setError(t("generatedEditor.mediaUnavailable"));
      return;
    }
    onSelect(reference);
    setOpen(false);
    setSelected(null);
    setFile(null);
    setAttachment(null);
    setError("");
  };

  const upload = async () => {
    if (!file || busy) return;
    setBusy(true);
    setError("");
    let uploaded = attachment;
    try {
      if (!uploaded) {
        uploaded = await uploadAgentAttachment(tenantId, projectId, file, {
          createKey: createIdempotencyKey(),
          completeKey: createIdempotencyKey(),
        });
        setAttachment(uploaded);
      }
      // Never insert or persist a media node until the verified usage binding exists.
      const binding = await bindContentMedia(
        tenantId,
        projectId,
        mediaKeyFromAttachment(uploaded),
      );
      setItems((previous) =>
        previous.some((item) => item.binding_id === binding.binding_id)
          ? previous
          : [binding, ...previous],
      );
      insert(binding);
    } catch {
      setError(t("generatedEditor.mediaBindError"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="content-media-control">
      <Button
        aria-expanded={open}
        aria-controls="content-media-picker"
        disabled={busy}
        onClick={() => setOpen((value) => !value)}
      >
        {t("generatedEditor.mediaInsert")}
      </Button>
      {open && (
        <div
          id="content-media-picker"
          role="group"
          aria-label={t("generatedEditor.mediaInsert")}
          className="content-media-picker"
        >
          <h3>{t("generatedEditor.mediaInsert")}</h3>
          <label>
            {t("generatedEditor.mediaUpload")}
            <input
              type="file"
              accept="image/png,image/jpeg,image/webp"
              disabled={busy}
              onChange={(event) => {
                if (busy) return;
                const next = event.currentTarget.files?.[0] ?? null;
                setFile(next);
                setAttachment(null);
                setSelected(null);
                setAlt(
                  next
                    ? filenameAlt(
                        next.name,
                        t("generatedEditor.mediaDefaultAlt"),
                      )
                    : "",
                );
                setCaption("");
                setError("");
              }}
            />
          </label>
          <label>
            {t("generatedEditor.mediaAlt")}
            <Input
              value={alt}
              disabled={busy}
              onChange={(_, data) => {
                if (!busy) setAlt(data.value);
              }}
            />
          </label>
          <label>
            {t("generatedEditor.mediaCaption")}
            <Input
              value={caption}
              disabled={busy}
              onChange={(_, data) => {
                if (!busy) setCaption(data.value);
              }}
            />
          </label>
          {file && (
            <Button disabled={busy} onClick={() => void upload()}>
              {busy
                ? t("generatedEditor.mediaWorking")
                : attachment
                  ? t("generatedEditor.mediaRetryBinding")
                  : t("generatedEditor.mediaUploadInsert")}
            </Button>
          )}
          {attachment && (
            <p>
              {t("generatedEditor.mediaAttachmentKept", {
                name: attachment.filename,
              })}
            </p>
          )}
          <p>{t("generatedEditor.mediaExisting")}</p>
          {items.length === 0 && <p>{t("generatedEditor.mediaEmpty")}</p>}
          <ul className="content-media-items">
            {items
              .filter((item) => item.state === "active")
              .map((item) => (
                <li key={item.binding_id}>
                  <Button
                    disabled={busy}
                    appearance={
                      selected?.binding_id === item.binding_id
                        ? "primary"
                        : "subtle"
                    }
                    onClick={() => {
                      if (busy) return;
                      setFile(null);
                      setAttachment(null);
                      setSelected(item);
                      setAlt(t("generatedEditor.mediaDefaultAlt"));
                      setCaption("");
                    }}
                  >
                    <MediaThumbnail
                      binding={item}
                      tenantId={tenantId}
                      projectId={projectId}
                    />
                    {t("generatedEditor.mediaDimensions", {
                      width: item.image.width,
                      height: item.image.height,
                      type: item.image.media_type,
                    })}
                  </Button>
                </li>
              ))}
          </ul>
          {selected && (
            <Button disabled={busy} onClick={() => insert(selected)}>
              {t("generatedEditor.mediaInsertSelected")}
            </Button>
          )}
          {cursor && (
            <Button disabled={busy} onClick={() => void load(cursor)}>
              {t("generatedEditor.mediaMore")}
            </Button>
          )}
          {error && <p role="alert">{error}</p>}
          {error && !file && (
            <Button disabled={busy} onClick={() => void load()}>
              {t("generatedEditor.mediaRetry")}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}
