import React from "react";
import { Box, Text } from "ink";
import { MUTED, SECONDARY } from "../theme.js";

interface Session {
  session_id: string;
  timestamp: string;
  message_count: number;
}

interface Props {
  sessions: Session[];
}

export function SessionList({ sessions }: Props) {
  if (sessions.length === 0) {
    return <Text dimColor>No saved sessions.</Text>;
  }

  return (
    <Box flexDirection="column">
      <Text color={SECONDARY} bold>Saved Sessions</Text>
      {sessions.map((s) => (
        <Box key={s.session_id}>
          <Text color={MUTED}>{`  ${s.session_id.slice(0, 8)}  `}</Text>
          <Text dimColor>{s.timestamp}</Text>
          <Text dimColor>{`  (${s.message_count} msgs)`}</Text>
        </Box>
      ))}
    </Box>
  );
}
