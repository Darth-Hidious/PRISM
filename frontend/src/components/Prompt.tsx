import React, { useState } from "react";
import { Box, Text, useStdout } from "ink";
import TextInput from "ink-text-input";
import { PRIMARY, ACCENT_CYAN, MUTED, TEXT, DIM } from "../theme.js";

interface Props {
  onSubmit: (text: string) => void;
  active?: boolean;
}

export function Prompt({ onSubmit, active = true }: Props) {
  const [value, setValue] = useState("");
  const { stdout } = useStdout();
  const cols = stdout?.columns ?? 80;

  const handleSubmit = (text: string) => {
    if (text.trim()) {
      onSubmit(text.trim());
      setValue("");
    }
  };

  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      borderColor={active ? ACCENT_CYAN : DIM}
      paddingX={1}
      width={Math.max(cols, 40)}
    >
      <Text color={MUTED}>message</Text>
      <Box>
        <Text color={active ? PRIMARY : MUTED} bold>{"> "}</Text>
        {active ? (
          <TextInput
            value={value}
            onChange={setValue}
            onSubmit={handleSubmit}
          />
        ) : (
          <Text color={MUTED}>waiting for agent...</Text>
        )}
      </Box>
      <Text color={MUTED}>
        <Text color={TEXT}>enter</Text> send  ·  <Text color={TEXT}>/</Text> command  ·  <Text color={TEXT}>ctrl+c</Text> exit
      </Text>
    </Box>
  );
}
