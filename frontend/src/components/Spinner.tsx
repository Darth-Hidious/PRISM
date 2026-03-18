import React from "react";
import { Text, Box } from "ink";
import InkSpinner from "ink-spinner";
import { PRIMARY, MUTED } from "../theme.js";

interface Props {
  verb: string;
}

export function Spinner({ verb }: Props) {
  return (
    <Box>
      <Text color={PRIMARY}><InkSpinner type="dots" /></Text>
      <Text color={MUTED}>{` ${verb}`}</Text>
    </Box>
  );
}
