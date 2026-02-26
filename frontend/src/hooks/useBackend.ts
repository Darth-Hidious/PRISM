import { useState, useEffect, useCallback, useRef } from "react";
import { BackendClient } from "../bridge/client.js";

export interface BackendEvent {
  method: string;
  params: Record<string, any>;
}

export function useBackend(pythonPath: string) {
  const clientRef = useRef<BackendClient | null>(null);
  const [ready, setReady] = useState(false);
  const [events, setEvents] = useState<BackendEvent[]>([]);

  useEffect(() => {
    const client = new BackendClient(pythonPath);
    clientRef.current = client;

    client.on("event", (event: BackendEvent) => {
      setEvents((prev) => [...prev, event]);
    });

    client.request("init", {}).then(() => setReady(true));

    return () => client.destroy();
  }, [pythonPath]);

  const sendMessage = useCallback((text: string) => {
    clientRef.current?.send("input.message", { text });
  }, []);

  const sendCommand = useCallback((command: string) => {
    clientRef.current?.send("input.command", { command });
  }, []);

  const sendPromptResponse = useCallback(
    (promptType: string, response: string) => {
      clientRef.current?.send("input.prompt_response", {
        prompt_type: promptType,
        response,
      });
    },
    [],
  );

  return { ready, events, sendMessage, sendCommand, sendPromptResponse };
}
