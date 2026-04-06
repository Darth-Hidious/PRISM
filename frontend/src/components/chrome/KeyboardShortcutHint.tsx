import React from "react";
import { Text } from "ink";

interface Props {
  shortcut: string;
  action: string;
  bold?: boolean;
  parens?: boolean;
}

export function KeyboardShortcutHint({
  shortcut,
  action,
  bold = false,
  parens = false,
}: Props) {
  const label = (
    <>
      <Text bold={bold}>{shortcut}</Text>
      <Text>{` to ${action}`}</Text>
    </>
  );

  if (parens) {
    return (
      <Text>
        (
        {label}
        )
      </Text>
    );
  }

  return <Text>{label}</Text>;
}
