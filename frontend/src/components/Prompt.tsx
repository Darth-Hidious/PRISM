import React, { useState } from "react";
import { Box, Text } from "ink";
import TextInput from "ink-text-input";
import { ACCENT_CYAN } from "../theme.js";

interface Props {
  onSubmit: (text: string) => void;
}

export function Prompt({ onSubmit }: Props) {
  const [value, setValue] = useState("");

  const handleSubmit = (text: string) => {
    if (text.trim()) {
      onSubmit(text.trim());
      setValue("");
    }
  };

  return (
    <Box>
      <Text color={ACCENT_CYAN} bold>{"\u276f "}</Text>
      <TextInput value={value} onChange={setValue} onSubmit={handleSubmit} />
    </Box>
  );
}
