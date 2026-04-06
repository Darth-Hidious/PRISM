import React from "react";
import { Box, Text } from "ink";
import { MUTED, SECONDARY, TEXT_DIM, TEXT_MUTED, PRIMARY } from "../theme.js";

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
    <Box
      flexDirection="column"
      borderStyle="single"
      borderLeft
      borderRight={false}
      borderTop={false}
      borderBottom={false}
      borderColor={SECONDARY}
      paddingLeft={2}
      marginTop={1}
      marginBottom={1}
    >
      <Text color={SECONDARY} bold>Sessions</Text>
      {sessions.map((s) => (
        <Box key={s.session_id} flexDirection="column" marginTop={1}>
          <Box>
            <Text color={s.is_latest ? PRIMARY : MUTED}>{s.session_id.slice(0, 16)}</Text>
            {s.is_latest ? <Text color={PRIMARY}>{"  latest"}</Text> : null}
          </Box>
          <Text color={TEXT_MUTED}>
            {formatCreatedAt(s.created_at)}
            {` · ${s.turn_count} turns · ${s.model || "unknown model"} · ${s.size_kb.toFixed(1)}KB`}
          </Text>
        </Box>
      ))}
      <Text color={TEXT_DIM}>/resume &lt;session-id&gt; to reopen one</Text>
    </Box>
  );
}
