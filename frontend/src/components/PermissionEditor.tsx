import React from "react";
import { Box, Text, useInput } from "ink";
import {
  ACCENT,
  PRIMARY,
  SECONDARY,
  SUCCESS,
  TEXT,
  TEXT_DIM,
  TEXT_MUTED,
  WARNING,
} from "../theme.js";
import { Byline } from "./chrome/Byline.js";
import { KeyboardShortcutHint } from "./chrome/KeyboardShortcutHint.js";
import { Pane } from "./chrome/Pane.js";
import { Pill } from "./chrome/Pill.js";

export interface PermissionTool {
  name: string;
  permission_mode: string;
  requires_approval: boolean;
  description: string;
  source?: string;
  source_detail?: string;
  current_behavior: string;
}

interface PermissionGroup {
  id: string;
  title: string;
  tone: string;
  tools: PermissionTool[];
}

interface Props {
  mode: string;
  autoApproved: PermissionTool[];
  blocked: PermissionTool[];
  approvalRequired: PermissionTool[];
  readOnly: PermissionTool[];
  workspaceWrite: PermissionTool[];
  fullAccess: PermissionTool[];
  allowOverrides: string[];
  denyOverrides: string[];
  notice?: string;
  onCommand: (command: string) => void;
  onClose: () => void;
}

function tabTone(id: string): string {
  switch (id) {
    case "blocked":
      return WARNING;
    case "approval":
      return WARNING;
    case "write":
      return ACCENT;
    case "full":
      return WARNING;
    default:
      return SECONDARY;
  }
}

function summarizeTool(tool: PermissionTool): string {
  const parts = [tool.permission_mode];
  if (tool.requires_approval) parts.push("approval");
  if (tool.current_behavior === "auto-approved") parts.push("auto");
  if (tool.current_behavior === "blocked") parts.push("blocked");
  if (tool.source) {
    parts.push(tool.source_detail ? `${tool.source}:${tool.source_detail}` : tool.source);
  }
  return parts.join(" · ");
}

export function PermissionEditor({
  mode,
  autoApproved,
  blocked,
  approvalRequired,
  readOnly,
  workspaceWrite,
  fullAccess,
  allowOverrides,
  denyOverrides,
  notice,
  onCommand,
  onClose,
}: Props) {
  const groups = React.useMemo<PermissionGroup[]>(
    () => [
      { id: "auto", title: "Auto", tone: tabTone("auto"), tools: autoApproved },
      { id: "blocked", title: "Blocked", tone: tabTone("blocked"), tools: blocked },
      {
        id: "approval",
        title: "Approval",
        tone: tabTone("approval"),
        tools: approvalRequired,
      },
      { id: "read", title: "Read", tone: tabTone("read"), tools: readOnly },
      { id: "write", title: "Write", tone: tabTone("write"), tools: workspaceWrite },
      { id: "full", title: "Full", tone: tabTone("full"), tools: fullAccess },
    ],
    [approvalRequired, autoApproved, blocked, fullAccess, readOnly, workspaceWrite],
  );

  const [groupIndex, setGroupIndex] = React.useState(() => (blocked.length > 0 ? 1 : 0));
  const [toolIndex, setToolIndex] = React.useState(0);
  const activeGroup = groups[groupIndex] ?? groups[0]!;
  const activeTool = activeGroup.tools[toolIndex];

  React.useEffect(() => {
    if (toolIndex >= activeGroup.tools.length) {
      setToolIndex(Math.max(0, activeGroup.tools.length - 1));
    }
  }, [activeGroup.tools.length, toolIndex]);

  const runToolCommand = React.useCallback(
    (action: "allow" | "deny" | "ask") => {
      if (!activeTool) return;
      onCommand(`/permissions ${action} ${activeTool.name}`);
    },
    [activeTool, onCommand],
  );

  useInput((input, key) => {
    if (key.escape || input === "q" || input === "Q") {
      onClose();
      return;
    }

    if (key.leftArrow || (key.shift && key.tab)) {
      setGroupIndex((index) => (index - 1 + groups.length) % groups.length);
      return;
    }

    if (key.rightArrow || key.tab) {
      setGroupIndex((index) => (index + 1) % groups.length);
      return;
    }

    if (key.upArrow && activeGroup.tools.length > 0) {
      setToolIndex((index) => (index - 1 + activeGroup.tools.length) % activeGroup.tools.length);
      return;
    }

    if (key.downArrow && activeGroup.tools.length > 0) {
      setToolIndex((index) => (index + 1) % activeGroup.tools.length);
      return;
    }

    if (input === "a" || input === "A") {
      runToolCommand("allow");
      return;
    }

    if (input === "b" || input === "B") {
      runToolCommand("deny");
      return;
    }

    if (input === "r" || input === "R") {
      if (activeTool) {
        runToolCommand("ask");
      } else {
        onCommand("/permissions reset");
      }
      return;
    }

    const numeric = Number.parseInt(input, 10);
    if (!Number.isNaN(numeric) && numeric >= 1 && numeric <= groups.length) {
      setGroupIndex(numeric - 1);
    }
  });

  const summary = (
    <Box flexDirection="column">
      <Text color={TEXT_DIM}>
        <Byline>
          <Pill label={`mode ${mode}`} color={SECONDARY} />
          <Pill label={`${autoApproved.length} auto`} color={SUCCESS} />
          <Pill label={`${blocked.length} blocked`} color={WARNING} />
          <Pill label={`${approvalRequired.length} gated`} color={WARNING} />
          <Pill label={`${allowOverrides.length} allow overrides`} color={TEXT_MUTED} />
          <Pill label={`${denyOverrides.length} deny overrides`} color={TEXT_MUTED} />
        </Byline>
      </Text>
      <Box marginTop={1} flexDirection="column">
        <Text color={TEXT_MUTED}>
          Session overrides layer on top of the current mode. Plan mode can still block
          write and execution tools even if you allow them here.
        </Text>
      </Box>
      <Box marginTop={1} flexDirection="column">
        <Text color={TEXT}>
          {activeTool ? activeTool.name : "No tools in this tab"}
        </Text>
        {activeTool ? (
          <>
            <Text color={TEXT_DIM}>{summarizeTool(activeTool)}</Text>
            <Text color={TEXT_MUTED}>{activeTool.description}</Text>
          </>
        ) : (
          <Text color={TEXT_DIM}>Press <Text bold>r</Text> to clear all session overrides.</Text>
        )}
      </Box>
      <Box marginTop={1} flexDirection="column">
        <Text color={TEXT}>Session overrides</Text>
        <Text color={TEXT_DIM}>
          allow: {allowOverrides.length > 0 ? allowOverrides.join(", ") : "(none)"}
        </Text>
        <Text color={TEXT_DIM}>
          deny: {denyOverrides.length > 0 ? denyOverrides.join(", ") : "(none)"}
        </Text>
      </Box>
      {notice ? (
        <Box marginTop={1}>
          <Text color={SUCCESS}>{notice}</Text>
        </Box>
      ) : null}
    </Box>
  );

  return (
    <Pane
      color={WARNING}
      title="Permissions"
      subtitle={activeTool ? activeTool.name : activeGroup.title}
      footer={
        <Text color={TEXT_DIM}>
          <Byline>
            <KeyboardShortcutHint shortcut="tab/shift+tab" action="switch tabs" />
            <KeyboardShortcutHint shortcut="↑/↓" action="move" />
            <KeyboardShortcutHint shortcut="a" action="allow session" />
            <KeyboardShortcutHint shortcut="b" action="block session" />
            <KeyboardShortcutHint shortcut="r" action="reset/ask" />
            <KeyboardShortcutHint shortcut="esc" action="close" />
          </Byline>
        </Text>
      }
    >
      <Box gap={1} flexWrap="wrap">
        {groups.map((group, index) => {
          const isActive = index === groupIndex;
          return (
            <Pill
              key={group.id}
              label={`${index + 1}. ${group.title}`}
              color={group.tone}
              active={isActive}
            />
          );
        })}
      </Box>

      {summary}

      <Box marginTop={1} flexDirection="column">
        <Text color={PRIMARY}>{activeGroup.title}</Text>
        {activeGroup.tools.length === 0 ? (
          <Text color={TEXT_DIM}>No tools in this group.</Text>
        ) : (
          activeGroup.tools.map((tool, index) => (
            <Box key={tool.name} flexDirection="column" marginTop={1}>
              <Text color={index === toolIndex ? TEXT : TEXT_MUTED} bold={index === toolIndex}>
                {index === toolIndex ? "❯ " : "  "}
                {tool.name}
              </Text>
              <Text color={TEXT_DIM}>{summarizeTool(tool)}</Text>
            </Box>
          ))
        )}
      </Box>
    </Pane>
  );
}
