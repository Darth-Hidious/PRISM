import React from "react";
import { Box, Text, useInput } from "ink";
import { PRIMARY, SECONDARY, TEXT, TEXT_DIM, TEXT_MUTED } from "../theme.js";
import { Byline } from "./chrome/Byline.js";
import { KeyboardShortcutHint } from "./chrome/KeyboardShortcutHint.js";
import { Pane } from "./chrome/Pane.js";
import { Pill } from "./chrome/Pill.js";
import type { TurnSessionSummary } from "./TurnCard.js";

interface Props {
  sessions: TurnSessionSummary[];
  onResume: (sessionId: string) => void;
  onClose: () => void;
}

function formatCreatedAt(createdAt: number): string {
  const date = new Date(createdAt * 1000);
  if (Number.isNaN(date.getTime())) return "unknown time";
  return date.toLocaleString();
}

export function SessionPicker({ sessions, onResume, onClose }: Props) {
  const [selected, setSelected] = React.useState(0);

  React.useEffect(() => {
    if (selected >= sessions.length) {
      setSelected(Math.max(0, sessions.length - 1));
    }
  }, [selected, sessions.length]);

  useInput((input, key) => {
    if (key.escape || input === "q" || input === "Q") {
      onClose();
      return;
    }

    if (sessions.length === 0) return;

    if (key.upArrow) {
      setSelected((index) => (index - 1 + sessions.length) % sessions.length);
      return;
    }

    if (key.downArrow) {
      setSelected((index) => (index + 1) % sessions.length);
      return;
    }

    if (key.return) {
      onResume(sessions[selected]!.session_id);
    }
  });

  const active = sessions[selected];

  return (
    <Pane
      color={SECONDARY}
      title="Sessions"
      subtitle={active ? active.session_id.slice(0, 16) : "No saved sessions"}
      footer={
        <Text color={TEXT_DIM}>
          <Byline>
            <KeyboardShortcutHint shortcut="↑/↓" action="move" />
            <KeyboardShortcutHint shortcut="enter" action="resume" />
            <KeyboardShortcutHint shortcut="esc" action="close" />
          </Byline>
        </Text>
      }
    >
      {active ? (
        <Box flexDirection="column">
          <Text color={TEXT_DIM}>
            <Byline>
              {active.is_latest ? <Pill label="latest" color={PRIMARY} /> : null}
              <Pill label={`${active.turn_count} turns`} color={TEXT_MUTED} />
              <Pill label={`${active.size_kb.toFixed(1)}KB`} color={TEXT_MUTED} />
              <Text>{active.model || "unknown model"}</Text>
            </Byline>
          </Text>
          <Text color={TEXT_MUTED}>{formatCreatedAt(active.created_at)}</Text>
        </Box>
      ) : null}

      <Box marginTop={1} flexDirection="column">
        {sessions.length === 0 ? (
          <Text color={TEXT_DIM}>No saved sessions.</Text>
        ) : (
          sessions.map((session, index) => (
            <Box key={session.session_id} flexDirection="column" marginTop={1}>
              <Text color={index === selected ? TEXT : TEXT_MUTED} bold={index === selected}>
                {index === selected ? "❯ " : "  "}
                {session.session_id.slice(0, 20)}
              </Text>
              <Text color={TEXT_DIM}>
                {formatCreatedAt(session.created_at)}
                {` · ${session.turn_count} turns`}
              </Text>
            </Box>
          ))
        )}
      </Box>
    </Pane>
  );
}
