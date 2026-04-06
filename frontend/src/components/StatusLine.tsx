import React from "react";
import { Box, Text } from "ink";
import {
  ACCENT,
  PRIMARY,
  SECONDARY,
  SUCCESS,
  TEXT_DIM,
  TEXT_MUTED,
  WARNING,
} from "../theme.js";
import { Byline } from "./chrome/Byline.js";
import { KeyboardShortcutHint } from "./chrome/KeyboardShortcutHint.js";
import { Pill } from "./chrome/Pill.js";

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
  model?: string;
  projectRoot?: string;
}

function shortSessionId(sessionId?: string): string | null {
  if (!sessionId) return null;
  const parts = sessionId.split("_");
  return parts[parts.length - 1] ?? sessionId;
}

function shortProjectName(projectRoot?: string): string | null {
  if (!projectRoot) return null;
  const normalized = projectRoot.replace(/\/+$/, "");
  const parts = normalized.split("/");
  return parts[parts.length - 1] || normalized;
}

function shortModelLabel(model?: string): string | null {
  if (!model) return null;
  if (model.length <= 26) return model;
  return `${model.slice(0, 23)}...`;
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
  model,
  projectRoot,
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
  const projectLabel = shortProjectName(projectRoot);
  const modelLabel = shortModelLabel(model);
  const modeLabel = sessionMode ?? "chat";
  const activityColor = approvalPending
    ? WARNING
    : turnActive
      ? SUCCESS
      : TEXT_MUTED;

  return (
    <Box flexDirection="column" marginTop={1}>
      <Box flexDirection="column">
        <Text color={TEXT_DIM}>
          <Byline>
            <Pill label="PRISM" color={PRIMARY} active bold />
            {projectLabel ? <Pill label={projectLabel} color={SECONDARY} /> : null}
            {sessionLabel ? <Pill label={`session ${sessionLabel}`} color={TEXT_MUTED} /> : null}
            {modelLabel ? <Pill label={modelLabel} color={PRIMARY} /> : null}
            {resumed ? <Pill label="resumed" color={TEXT_MUTED} /> : null}
          </Byline>
        </Text>
        <Text color={TEXT_DIM}>
          <Byline>
            <Pill label={modeLabel} color={modeLabel === "plan" ? ACCENT : SECONDARY} />
            {planLabel ? <Pill label={planLabel} color={ACCENT} /> : null}
            <Pill label={activityLabel} color={activityColor} />
            <Pill label={`${messageCount} msgs`} color={TEXT_MUTED} />
            {toolCount !== undefined ? (
              <Pill label={`${toolCount} tools`} color={TEXT_MUTED} />
            ) : null}
            {activeViewTitle ? (
              <Pill label={`view ${activeViewTitle.toLowerCase()}`} color={SECONDARY} />
            ) : null}
            {autoApprove ? <Pill label="auto-approve" color={WARNING} /> : null}
          </Byline>
        </Text>
      </Box>
      <Text color={TEXT_DIM}>
        <Byline>
          <KeyboardShortcutHint shortcut="enter" action="send" />
          <KeyboardShortcutHint shortcut="/" action="command" />
          <KeyboardShortcutHint shortcut="esc" action="close view" />
          <KeyboardShortcutHint shortcut="ctrl+c" action="exit" />
        </Byline>
      </Text>
    </Box>
  );
}
