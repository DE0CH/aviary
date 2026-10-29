import { useCallback, useEffect, useRef, useState } from "react";
import type { DiagramColorScheme, MermaidRenderRequest } from "./render-model";
import { parseMermaidRuntimeMessage, type MermaidRuntimeRenderMessage } from "./runtime/messages";
import { MermaidRuntimeRequestDriver } from "./runtime/request-driver";

// The runtime page (Mermaid + KaTeX, 3.6 MB) is its own chunk, fetched the first time a reply has a diagram, never at
// page load (docs/paseo-ui.md "Loading").
let runtimeHtml: Promise<string> | null = null;
function loadRuntimeHtml(): Promise<string> {
  runtimeHtml ??= import("./runtime/html.gen").then((m) => m.mermaidRuntimeHtml);
  return runtimeHtml;
}

export interface MermaidRenderedMessage {
  revision: number;
  source: string;
  colorScheme: DiagramColorScheme;
  height: number;
  width: number;
}

interface MermaidIframeRuntimeProps {
  request: MermaidRenderRequest | null;
  onRendered: (message: MermaidRenderedMessage) => void;
  onRenderFailed: (revision: number) => void;
}

/** Sandboxed Mermaid renderer. Sizing and gestures belong to the surrounding viewport. */
export function MermaidIframeRuntime({
  request,
  onRendered,
  onRenderFailed,
}: MermaidIframeRuntimeProps) {
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const driverRef = useRef<MermaidRuntimeRequestDriver | null>(null);
  driverRef.current ??= new MermaidRuntimeRequestDriver();
  const [html, setHtml] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void loadRuntimeHtml().then((loaded) => {
      if (live) setHtml(loaded);
    });
    return () => {
      live = false;
    };
  }, []);

  const sendRequest = useCallback((current: MermaidRenderRequest | null) => {
    const target = iframeRef.current?.contentWindow;
    if (!current || !target) return;
    const message: MermaidRuntimeRenderMessage = {
      type: "render",
      revision: current.revision,
      source: current.source,
      colorScheme: current.colorScheme,
      interactive: false,
    };
    target.postMessage(message, "*");
  }, []);

  useEffect(() => {
    sendRequest(driverRef.current?.update(request) ?? null);
  }, [request, sendRequest]);

  useEffect(() => {
    function receiveMessage(event: MessageEvent): void {
      if (event.source !== iframeRef.current?.contentWindow) return;
      const message = parseMermaidRuntimeMessage(event.data);
      if (!message) return;
      if (message.type === "bridgeReady") {
        sendRequest(driverRef.current?.ready() ?? null);
        return;
      }
      if (message.type === "renderError") {
        onRenderFailed(message.revision);
        sendRequest(driverRef.current?.settled(message.revision, false) ?? null);
        return;
      }
      onRendered(message);
      sendRequest(driverRef.current?.settled(message.revision, true) ?? null);
    }
    window.addEventListener("message", receiveMessage);
    return () => window.removeEventListener("message", receiveMessage);
  }, [onRenderFailed, onRendered, sendRequest]);

  // `inert` (not just tabIndex) because the Modal focus trap focuses descendants
  // programmatically; a focused iframe swallows every keystroke, including Escape.
  if (html === null) return null;
  return (
    <iframe
      ref={iframeRef}
      title=""
      aria-hidden
      inert
      sandbox="allow-scripts"
      srcDoc={html}
      tabIndex={-1}
      style={iframeStyle}
    />
  );
}

const iframeStyle: React.CSSProperties = {
  display: "block",
  width: "100%",
  height: "100%",
  border: 0,
  pointerEvents: "none",
  background: "transparent",
};
