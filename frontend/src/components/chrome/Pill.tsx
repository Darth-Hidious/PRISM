import React from "react";
import { Text } from "ink";
import { TEXT, TEXT_DIM } from "../../theme.js";

interface Props {
  label: string;
  color?: string;
  active?: boolean;
  bold?: boolean;
}

export function Pill({ label, color, active = false, bold = false }: Props) {
  return (
    <Text
      color={active ? TEXT : color ?? TEXT_DIM}
      inverse={active}
      bold={active || bold}
    >
      {` ${label} `}
    </Text>
  );
}
