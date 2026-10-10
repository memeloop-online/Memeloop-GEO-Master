import { useEffect, useRef, useState } from "react";
import { Button, Spinner } from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import type RFB from "@novnc/novnc";
import type { DesktopAuthorization } from "../api/channels";

function desktopUrl(authorization: DesktopAuthorization): string {
  const { websocket_path: path, protocol } = authorization;
  if (
    !path.startsWith("/api/v1/") ||
    path.startsWith("//") ||
    !/^geo-desktop\.[A-Za-z0-9._~-]+$/.test(protocol)
  ) {
    throw new Error("Invalid remote connection");
  }
  const url = new URL(path, window.location.origin);
  if (
    url.origin !== window.location.origin ||
    !/^\/api\/v1\/(?:operator\/)?channel-login-sessions\/[^/]+\/desktop$/.test(
      url.pathname,
    ) ||
    [...url.searchParams.keys()].some(
      (key) => key !== "tenant_id" && key !== "project_id",
    )
  ) {
    throw new Error("Invalid remote connection");
  }
  url.protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return url.href;
}

export function RemoteDesktop({
  authorize,
  active,
}: {
  authorize: () => Promise<DesktopAuthorization>;
  active: boolean;
}) {
  const { t } = useTranslation();
  const target = useRef<HTMLDivElement>(null);
  const connection = useRef<RFB | null>(null);
  const generation = useRef(0);
  const [state, setState] = useState<
    "connecting" | "connected" | "disconnected"
  >("disconnected");
  const [error, setError] = useState("");

  async function connect() {
    const attempt = ++generation.current;
    connection.current?.disconnect();
    connection.current = null;
    setState("connecting");
    setError("");
    try {
      // Keep the desktop stack out of the initial workspace/chat bundle.
      const { default: RemoteFramebuffer } = await import("@novnc/novnc");
      if (generation.current !== attempt) return;
      const authorization = await authorize();
      if (generation.current !== attempt || !target.current) return;
      const rfb = new RemoteFramebuffer(
        target.current,
        desktopUrl(authorization),
        {
          wsProtocols: [authorization.protocol],
        },
      );
      connection.current = rfb;
      rfb.scaleViewport = true;
      rfb.resizeSession = true;
      rfb.addEventListener("connect", () => {
        if (generation.current === attempt) setState("connected");
      });
      rfb.addEventListener("disconnect", () => {
        if (generation.current === attempt) {
          connection.current = null;
          setState("disconnected");
        }
      });
      rfb.addEventListener("securityfailure", () => {
        if (generation.current === attempt) setError(t("remoteDesktop.failed"));
      });
    } catch {
      if (generation.current === attempt) {
        setState("disconnected");
        setError(t("remoteDesktop.failed"));
      }
    }
  }

  useEffect(() => {
    if (active) void connect();
    return () => {
      ++generation.current;
      connection.current?.disconnect();
      connection.current = null;
    };
    // A new authorization is issued only on mounting or an explicit reconnect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  async function paste() {
    try {
      const text = await navigator.clipboard.readText();
      if (text && connection.current)
        connection.current.clipboardPasteFrom(text);
    } catch {
      setError(t("remoteDesktop.clipboardUnavailable"));
    }
  }

  return (
    <div
      className="channel-desktop"
      role="group"
      aria-label={t("remoteDesktop.screen")}
    >
      <div className="channel-row">
        {state === "connecting" && (
          <Spinner size="tiny" label={t("remoteDesktop.connecting")} />
        )}
        {state === "connected" && <span>{t("remoteDesktop.connected")}</span>}
        {state === "disconnected" && (
          <Button onClick={() => void connect()} disabled={!active}>
            {t("remoteDesktop.reconnect")}
          </Button>
        )}
        <Button
          disabled={state !== "connected"}
          onClick={() => connection.current?.focus()}
        >
          {t("remoteDesktop.focus")}
        </Button>
        <Button disabled={state !== "connected"} onClick={() => void paste()}>
          {t("remoteDesktop.paste")}
        </Button>
      </div>
      {error && <p role="alert">{error}</p>}
      <div ref={target} className="channel-desktop-viewport" />
    </div>
  );
}
