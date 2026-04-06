import { useState, useEffect, useCallback, useRef } from "react";
import { BackendClient } from "../bridge/client.js";

export interface BackendEvent {
  method: string;
  params: Record<string, any>;
}

export function useBackend(
  pythonPath: string,
  backendBin?: string,
  autoApprove = false,
  resume?: string,
) {
  const clientRef = useRef<BackendClient | null>(null);
  const [ready, setReady] = useState(false);
  const [events, setEvents] = useState<BackendEvent[]>([]);

  useEffect(() => {
    const client = new BackendClient(pythonPath, backendBin);
    clientRef.current = client;

    client.on("event", (event: BackendEvent) => {
      // Keep raw protocol events append-only here. App-level folding happens in
      // the root renderer so alternative frontends can reuse the same stream.
      setEvents((prev) => [...prev, event]);
    });

    client
      .request("init", {
        auto_approve: autoApprove,
        resume: resume ?? "",
      })
      .then(() => setReady(true));

    return () => client.destroy();
  }, [pythonPath, backendBin, autoApprove, resume]);

  const sendMessage = useCallback((text: string) => {
    clientRef.current?.send("input.message", { text });
  }, []);

  const sendCommand = useCallback((command: string) => {
    clientRef.current?.send("input.command", { command });
  }, []);

  const sendPromptResponse = useCallback(
    (promptType: string, response: string, toolName?: string) => {
      clientRef.current?.send("input.prompt_response", {
        prompt_type: promptType,
        response,
        tool_name: toolName,
      });
    },
    [],
  );

  const sendModelSelect = useCallback((modelId: string) => {
    clientRef.current?.send("input.model_select", { model_id: modelId });
  }, []);

  return { ready, events, sendMessage, sendCommand, sendPromptResponse, sendModelSelect };
}
