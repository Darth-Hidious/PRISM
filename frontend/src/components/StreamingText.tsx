import React from "react";
import { Text } from "ink";

interface Props {
  text: string;
}

export function StreamingText({ text }: Props) {
  if (!text) return null;
  return <Text>{text}</Text>;
}
