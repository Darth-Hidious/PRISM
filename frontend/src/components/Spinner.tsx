import React from "react";
import { Text, Box } from "ink";
import InkSpinner from "ink-spinner";
import { PRIMARY } from "../theme.js";

interface Props {
  verb: string;
}

export function Spinner({ verb }: Props) {
  return (
    <Box>
      <Text color={PRIMARY}><InkSpinner type="dots" /></Text>
      <Text> {verb}</Text>
    </Box>
  );
}
