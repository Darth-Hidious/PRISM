import { useEffect, useRef, useCallback, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";

// ── WsEvent types (must match server WsEvent enum) ───────────────

interface NodeStatusUpdate {
  type: "NodeStatusUpdate";
  data: { uptime_secs: number; services: { name: string; port: number; healthy: boolean }[] };
}

interface MeshPeerChange {
  type: "MeshPeerChange";
  data: { action: string; node_id: string; name: string };
}

interface AuditEntry {
  type: "AuditEntry";
  data: { timestamp: string; user: string; action: string };
}

export type WsEvent = NodeStatusUpdate | MeshPeerChange | AuditEntry;

// ── Hook ──────────────────────────────────────────────────────────

export interface UseWebSocketOptions {
  token: string;
  /** Override the WebSocket URL (defaults to ws://localhost:7327/ws). */
  url?: string;
  /** Called for every received event. */
  onEvent?: (event: WsEvent) => void;
}

/**
 * Connect to the PRISM node WebSocket for live dashboard updates.
 *
 * Automatically reconnects on disconnect (with exponential back-off up to 30s).
 * Invalidates relevant React Query caches when events arrive so pages
 * refresh without polling.
 */
export function useWebSocket({ token, url, onEvent }: UseWebSocketOptions) {
  const queryClient = useQueryClient();
  const wsRef = useRef<WebSocket | null>(null);
  const [connected, setConnected] = useState(false);
  const reconnectTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const backoff = useRef(1000);

  const connect = useCallback(() => {
    const wsUrl = url ?? `ws://${window.location.host}/ws?token=${encodeURIComponent(token)}`;
    const ws = new WebSocket(wsUrl);
    wsRef.current = ws;

    ws.onopen = () => {
      setConnected(true);
      backoff.current = 1000;
    };

    ws.onmessage = (e) => {
      try {
        const event: WsEvent = JSON.parse(e.data);
        onEvent?.(event);

        // Invalidate relevant query caches so pages auto-refresh.
        switch (event.type) {
          case "NodeStatusUpdate":
            queryClient.invalidateQueries({ queryKey: ["nodeInfo"] });
            break;
          case "MeshPeerChange":
            queryClient.invalidateQueries({ queryKey: ["meshNodes"] });
            break;
          case "AuditEntry":
            queryClient.invalidateQueries({ queryKey: ["auditLog"] });
            break;
        }
      } catch {
        // Ignore non-JSON messages.
      }
    };

    ws.onclose = () => {
      setConnected(false);
      wsRef.current = null;
      // Reconnect with exponential back-off (max 30s).
      reconnectTimer.current = setTimeout(() => {
        backoff.current = Math.min(backoff.current * 2, 30_000);
        connect();
      }, backoff.current);
    };

    ws.onerror = () => {
      ws.close();
    };
  }, [token, url, onEvent, queryClient]);

  useEffect(() => {
    if (!token) return;
    connect();
    return () => {
      clearTimeout(reconnectTimer.current);
      wsRef.current?.close();
    };
  }, [connect, token]);

  return { connected };
}
