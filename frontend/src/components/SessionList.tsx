import React from "react";
import { Box, Text } from "ink";
import { MUTED, SECONDARY, TEXT_MUTED, PRIMARY } from "../theme.js";
import { Pane } from "./chrome/Pane.js";
import { Byline } from "./chrome/Byline.js";
import { Pill } from "./chrome/Pill.js";

interface Session {
  session_id: string;
  created_at: number;
  turn_count: number;
  model: string;
  size_kb: number;
  is_latest: boolean;
}

interface Props {
  sessions: Session[];
}

export function SessionList({ sessions }: Props) {
  if (sessions.length === 0) {
    return <Text dimColor>No saved sessions.</Text>;
  }

  const formatCreatedAt = (createdAt: number) => {
    const date = new Date(createdAt * 1000);
    if (Number.isNaN(date.getTime())) {
      return "unknown time";
    }
    return date.toLocaleString();
  };

  return (
    <Pane
      color={SECONDARY}
      title="Sessions"
      footer="/resume <session-id> to reopen one"
    >
      {sessions.map((s) => (
        <Box key={s.session_id} flexDirection="column" marginTop={1}>
          <Text color={TEXT_MUTED}>
            <Byline>
              <Text color={s.is_latest ? PRIMARY : MUTED}>{s.session_id.slice(0, 16)}</Text>
              {s.is_latest ? <Pill label="latest" color={PRIMARY} /> : null}
              <Pill label={`${s.turn_count} turns`} color={TEXT_MUTED} />
              <Pill label={`${s.size_kb.toFixed(1)}KB`} color={TEXT_MUTED} />
            </Byline>
          </Text>
          <Text color={TEXT_MUTED}>
            {formatCreatedAt(s.created_at)}
            {` · ${s.model || "unknown model"}`}
          </Text>
        </Box>
      ))}
    </Pane>
  );
}
