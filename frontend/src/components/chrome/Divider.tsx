import React from "react";
import { Text } from "ink";
import { PRIMARY, TEXT_DIM } from "../../theme.js";

interface Props {
  width?: number;
  color?: string;
  char?: string;
  padding?: number;
  title?: string;
}

export function Divider({
  width,
  color,
  char = "─",
  padding = 0,
  title,
}: Props) {
  const terminalWidth = process.stdout.columns ?? 80;
  const effectiveWidth = Math.max(0, (width ?? terminalWidth) - padding);

  if (title) {
    const titleWidth = title.length + 2;
    const sideWidth = Math.max(0, effectiveWidth - titleWidth);
    const leftWidth = Math.floor(sideWidth / 2);
    const rightWidth = sideWidth - leftWidth;

    return (
      <Text color={color ?? PRIMARY}>
        {char.repeat(leftWidth)} <Text color={TEXT_DIM}>{title}</Text>{" "}
        {char.repeat(rightWidth)}
      </Text>
    );
  }

  return <Text color={color ?? TEXT_DIM}>{char.repeat(effectiveWidth)}</Text>;
}
