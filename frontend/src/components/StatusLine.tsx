import React from "react";
import { Box, Text } from "ink";
import { PRIMARY, TEXT_DIM, TEXT_MUTED, WARNING } from "../theme.js";
import { Byline } from "./chrome/Byline.js";
import { KeyboardShortcutHint } from "./chrome/KeyboardShortcutHint.js";

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
    <Box flexDirection="column" marginTop={1}>
      <Text color={TEXT_DIM}>
        <Byline>
          <Text color={PRIMARY} bold>
            prism
          </Text>
          {sessionLabel ? <Text>{`session:${sessionLabel}`}</Text> : null}
          <Text>{`${messageCount} msgs`}</Text>
          {toolCount !== undefined ? <Text>{`${toolCount} tools`}</Text> : null}
          {sessionMode ? <Text>{`mode:${sessionMode}`}</Text> : null}
          {planLabel ? <Text color={TEXT_MUTED}>{planLabel}</Text> : null}
          {resumed ? <Text color={TEXT_MUTED}>resumed</Text> : null}
          <Text color={approvalPending ? WARNING : TEXT_MUTED}>{activityLabel}</Text>
          {activeViewTitle ? <Text>{`view:${activeViewTitle.toLowerCase()}`}</Text> : null}
          {autoApprove ? <Text color={WARNING}>auto-approve</Text> : null}
        </Byline>
      </Text>
      <Text color={TEXT_DIM}>
        <Byline>
          <KeyboardShortcutHint shortcut="enter" action="send" />
          <KeyboardShortcutHint shortcut="/" action="command" />
          <KeyboardShortcutHint shortcut="ctrl+c" action="exit" />
        </Byline>
      </Text>
    </Box>
  );
}
