import React, { useState, useEffect } from "react";
import { Box, Text } from "ink";
import { BORDER_AGENT, TEXT, TEXT_MUTED, TEXT_DIM } from "../theme.js";
import { MarkdownText } from "./MarkdownText.js";

const SPINNER_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

interface Props {
  text: string;
  streaming?: boolean;
}

function Thinking() {
  const [frame, setFrame] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => setFrame((f) => (f + 1) % SPINNER_FRAMES.length), 80);
    return () => clearInterval(timer);
  }, []);
  return (
    <Text color={TEXT_MUTED}>
      {SPINNER_FRAMES[frame]} thinking
    </Text>
  );
}

export function StreamingText({ text, streaming = false }: Props) {
  if (!text && streaming) {
    // Waiting for first token
    return (
      <Box paddingLeft={3} marginTop={1}>
        <Thinking />
      </Box>
    );
  }

  if (!text) return null;

  if (streaming) {
    // Active streaming: content with left border, no markdown yet
    return (
      <Box
        borderStyle="single"
        borderLeft
        borderRight={false}
        borderTop={false}
        borderBottom={false}
        borderColor={BORDER_AGENT}
        paddingLeft={2}
        marginTop={1}
      >
        <Box flexDirection="column" flexGrow={1}>
          <Text color={TEXT}>{text}</Text>
          <Box marginTop={0}>
            <Thinking />
          </Box>
        </Box>
      </Box>
    );
  }

  // Finalized: left border + full markdown rendering
  return (
    <Box
      borderStyle="single"
      borderLeft
      borderRight={false}
      borderTop={false}
      borderBottom={false}
      borderColor={BORDER_AGENT}
      paddingLeft={2}
      marginTop={1}
      marginBottom={1}
    >
      <Box flexDirection="column" flexGrow={1}>
        <MarkdownText text={text} />
      </Box>
    </Box>
  );
}
