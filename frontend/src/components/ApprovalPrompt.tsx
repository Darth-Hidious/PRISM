import React, { useState } from "react";
import { Box, Text, useInput, useStdout } from "ink";
import { WARNING, MUTED, TEXT, ERROR } from "../theme.js";

interface Props {
  toolName: string;
  toolArgs: Record<string, any>;
  onResponse: (response: string) => void;
}

const OPTIONS = [
  { key: "y", label: "Allow once" },
  { key: "a", label: "Allow always" },
  { key: "n", label: "Reject" },
] as const;

/** Tool-specific one-line summary for the approval body. */
function toolSummary(name: string, args: Record<string, any>): string {
  // Recognise common PRISM tools by name pattern
  if (name.includes("python") || name.includes("execute"))
    return `$ ${String(args.code ?? args.command ?? "").slice(0, 120)}`;
  if (name.includes("search") || name.includes("query"))
    return args.formula ?? args.query ?? args.text ?? "";
  if (args.command) return `$ ${String(args.command).slice(0, 120)}`;
  // Fallback: compact key=value summary
  const pairs = Object.entries(args)
    .slice(0, 3)
    .map(([k, v]) => `${k}=${typeof v === "string" ? v : JSON.stringify(v)}`);
  return pairs.join(", ");
}

export function ApprovalPrompt({ toolName, toolArgs, onResponse }: Props) {
  const [selected, setSelected] = useState(0);
  const { stdout } = useStdout();
  const cols = stdout?.columns ?? 80;

  useInput((input, key) => {
    if (key.leftArrow) {
      setSelected((s) => (s - 1 + OPTIONS.length) % OPTIONS.length);
    } else if (key.rightArrow) {
      setSelected((s) => (s + 1) % OPTIONS.length);
    } else if (key.return) {
      onResponse(OPTIONS[selected].key);
    } else if (key.escape) {
      onResponse("n");
    } else if (input === "y" || input === "Y") {
      onResponse("y");
    } else if (input === "a" || input === "A") {
      onResponse("a");
    } else if (input === "n" || input === "N") {
      onResponse("n");
    }
  });

  const summary = toolSummary(toolName, toolArgs);

  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      borderColor={WARNING}
      paddingX={1}
      width={Math.max(cols, 40)}
    >
      <Text color={WARNING}>approval required</Text>
      <Text color={TEXT}>
        <Text bold>{toolName}</Text>
        {summary ? <Text color={MUTED}>{`  ${summary}`}</Text> : null}
      </Text>
      <Text color={ERROR}>This tool can change state or execute commands.</Text>
      <Box marginTop={1}>
        {OPTIONS.map((opt, i) => (
          <React.Fragment key={opt.key}>
            <Text
              backgroundColor={i === selected ? WARNING : undefined}
              color={i === selected ? "#111111" : MUTED}
            >
              {" "}{opt.label}{" "}
            </Text>
            {i < OPTIONS.length - 1 ? <Text>{"  "}</Text> : null}
          </React.Fragment>
        ))}
      </Box>
      <Text color={MUTED}>left/right select  ·  enter confirm  ·  esc reject</Text>
    </Box>
  );
}
