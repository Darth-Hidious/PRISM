import React from "react";
import { Text } from "ink";
import { MarkdownText } from "./MarkdownText.js";

interface Props {
  text: string;
  /** When true, render raw text (for live streaming). When false, render markdown. */
  streaming?: boolean;
}

export function StreamingText({ text, streaming = false }: Props) {
  if (!text) return null;
  // During active streaming, render raw (markdown may be incomplete).
  // Once flushed/finalized, render with markdown formatting.
  if (streaming) return <Text>{text}</Text>;
  return <MarkdownText text={text} />;
}
