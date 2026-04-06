import React, { useState } from "react";
import { Box, Text, useInput } from "ink";
import {
  WARNING,
  TEXT,
  TEXT_MUTED,
  TEXT_DIM,
  BG_MENU,
} from "../theme.js";
import { Byline } from "./chrome/Byline.js";
import { KeyboardShortcutHint } from "./chrome/KeyboardShortcutHint.js";
import { Pane } from "./chrome/Pane.js";
import { Pill } from "./chrome/Pill.js";

interface Props {
  toolName: string;
  toolArgs: Record<string, any>;
  toolDescription?: string;
  requiresApproval?: boolean;
  permissionMode?: string;
  onResponse: (response: string) => void;
}

const OPTIONS = [
  { key: "y", label: "Allow Once" },
  { key: "n", label: "Reject" },
  { key: "a", label: "Allow Session" },
  { key: "b", label: "Block Session" },
] as const;

function toolSummary(name: string, args: Record<string, any>): string {
  if (name.includes("python") || name.includes("execute"))
    return String(args.code ?? args.command ?? "").split("\n")[0]?.slice(0, 60) ?? "";
  if (name.includes("search") || name.includes("query"))
    return String(args.formula ?? args.query ?? args.text ?? "").slice(0, 60);
  if (args.command) return `$ ${String(args.command).slice(0, 60)}`;
  if (args.path) return String(args.path);
  const pairs = Object.entries(args)
    .slice(0, 3)
    .map(([k, v]) => `${k}=${typeof v === "string" ? v.slice(0, 30) : JSON.stringify(v).slice(0, 30)}`);
  return pairs.join(", ");
}

export function ApprovalPrompt({
  toolName,
  toolArgs,
  toolDescription,
  requiresApproval,
  permissionMode,
  onResponse,
}: Props) {
  const [selected, setSelected] = useState(0);

  useInput((input, key) => {
    if (key.leftArrow) setSelected((s) => (s - 1 + OPTIONS.length) % OPTIONS.length);
    else if (key.rightArrow) setSelected((s) => (s + 1) % OPTIONS.length);
    else if (key.return) onResponse(OPTIONS[selected].key);
    else if (key.escape) onResponse("n");
    else if (input === "y" || input === "Y") onResponse("y");
    else if (input === "a" || input === "A") onResponse("a");
    else if (input === "b" || input === "B") onResponse("b");
    else if (input === "n" || input === "N") onResponse("n");
  });

  const summary = toolSummary(toolName, toolArgs);
  const description = toolDescription?.trim();
  const meta = [permissionMode, requiresApproval ? "approval required" : undefined]
    .filter(Boolean)
    .join(" · ");

  return (
    <Pane
      color={WARNING}
      title="Permission required"
      subtitle={
        meta ? `${toolName} · ${meta}` : toolName
      }
      footer={
        <Text color={TEXT_DIM}>
          <Byline>
            <KeyboardShortcutHint shortcut="←/→" action="choose" />
            <KeyboardShortcutHint shortcut="enter" action="confirm" />
            <KeyboardShortcutHint shortcut="esc" action="reject" />
          </Byline>
        </Text>
      }
    >
      {summary ? (
        <Box>
          <Text color={TEXT_DIM}>{summary}</Text>
        </Box>
      ) : null}

      {description ? (
        <Box marginTop={summary ? 1 : 0}>
          <Text color={TEXT_MUTED}>{description}</Text>
        </Box>
      ) : null}

      <Box marginTop={1} gap={1}>
        {OPTIONS.map((opt, i) => (
          <Pill
            key={opt.key}
            label={`${opt.label} (${opt.key})`}
            color={i === selected ? "#0a0a0a" : TEXT_MUTED}
            active={i === selected}
          />
        ))}
      </Box>
    </Pane>
  );
}
