import React from "react";
import { Box, Text } from "ink";
import { PRIMARY, TEXT_DIM, TEXT_MUTED, WARNING } from "../theme.js";

interface Props {
  autoApprove: boolean;
  messageCount: number;
  hasPlan: boolean;
  sessionMode?: string;
  planStatus?: string;
  sessionId?: string;
  toolCount?: number;
  resumed?: boolean;
  approvalPending?: boolean;
  turnActive?: boolean;
  activeViewTitle?: string;
}

function shortSessionId(sessionId?: string): string | null {
  if (!sessionId) return null;
  const parts = sessionId.split("_");
  return parts[parts.length - 1] ?? sessionId;
}

export function StatusLine({
  autoApprove,
  messageCount,
  hasPlan,
  sessionMode,
  planStatus,
  sessionId,
  toolCount,
  resumed,
  approvalPending,
  turnActive,
  activeViewTitle,
}: Props) {
  const planLabel =
    planStatus === "approved"
      ? "approved plan"
      : planStatus === "rejected"
        ? "plan rejected"
          : hasPlan || planStatus === "draft"
          ? "plan active"
          : null;
  const sessionLabel = shortSessionId(sessionId);
  const activityLabel = approvalPending
    ? "approval"
    : turnActive
      ? "running"
      : "idle";

  return (
    <Box gap={2} flexWrap="wrap">
      <Text color={PRIMARY} bold>prism</Text>
      {sessionLabel ? <Text color={TEXT_DIM}>{`session:${sessionLabel}`}</Text> : null}
      <Text color={TEXT_DIM}>{`${messageCount} msgs`}</Text>
      {toolCount !== undefined ? <Text color={TEXT_DIM}>{`${toolCount} tools`}</Text> : null}
      {sessionMode ? <Text color={TEXT_DIM}>{`mode:${sessionMode}`}</Text> : null}
      {planLabel ? <Text color={TEXT_MUTED}>{planLabel}</Text> : null}
      {resumed ? <Text color={TEXT_MUTED}>resumed</Text> : null}
      <Text color={approvalPending ? WARNING : TEXT_MUTED}>{activityLabel}</Text>
      {activeViewTitle ? (
        <Text color={TEXT_DIM}>{`view:${activeViewTitle.toLowerCase()}`}</Text>
      ) : null}
      {autoApprove ? <Text color={WARNING}>auto-approve</Text> : null}
      <Text color={TEXT_DIM}>/ commands</Text>
    </Box>
  );
}
